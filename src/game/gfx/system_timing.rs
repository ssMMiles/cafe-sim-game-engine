//! Per-system CPU timing for the editor's Performance panel.
//!
//! Bevy wraps every system run in a tracing span named `"system"` whose
//! `name` field is the system's full path — but only when built with the
//! `trace` feature (bevy_ecs `function_system.rs`). Our `profiling` and
//! `web-profile` features turn that on; the plain dev build emits no system
//! spans at all, so the panel shows a hint instead of an empty table there.
//!
//! The pieces:
//! - [`timing_layer`] is given to `LogPlugin::custom_layer` (src/lib.rs). It
//!   clocks every `"system"` span on enter/exit — whichever thread it runs
//!   on, so render-app systems are included — and adds its *self time* to a
//!   per-system [`Slot`]. Systems that run schedules are flagged rather than
//!   counted (`ThreadFrame`).
//! - [`collect_system_timings`] (in bevy's `Last`) drains every slot once per
//!   main-world frame and folds it into [`SystemTimings`], an exponential
//!   moving average per system, which is what the panel reads. The EMA is
//!   the smoothing: render-thread spans land whenever that thread finishes,
//!   so a single frame's slice is jittery by construction.
//! - **F9** saves a snapshot of the table from *any* mode, so a gameplay
//!   measurement doesn't carry the editor's own egui pass in it.
//!
//! The hot path is deliberately hash-free. Bevy creates one span per system
//! and re-enters it every run, so the system's slot is interned **once**, in
//! `on_new_span`, and stashed in the span's extensions as an `Arc<Slot>`;
//! each exit is then two atomic adds. An earlier version keyed a mutexed
//! `HashMap<String, _>` per exit and cloned ~1000 names per frame, which made
//! the collector the second most expensive row in its own table (1.2 ms).
//!
//! The layer is registered in every build. Without system spans its hooks
//! only ever see bevy's few log spans and return on the name check, so the
//! normal build pays nothing measurable.

use std::{
    cell::RefCell,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
    },
    time::Duration,
};

use bevy::{
    log::{
        BoxedLayer,
        tracing_subscriber::{Layer, layer::Context, registry::LookupSpan},
    },
    platform::time::Instant,
    prelude::*,
};
use tracing::{
    Id, Subscriber,
    field::{Field, Visit},
    span::Attributes,
};

use super::frame_pacing::{FrameBreakdown, FrameSample};
use crate::editor::EditorMode;

/// One system's running totals since the last collection, written from any
/// thread without a lock.
pub struct Slot {
    name: String,
    /// Self time in nanoseconds: the span's duration minus the system spans
    /// nested in it.
    total_ns: AtomicU64,
    runs: AtomicU32,
    /// Another system ran inside this one (see [`ThreadFrame`]).
    nests: AtomicBool,
}

impl Slot {
    fn new(name: String) -> Self {
        Self {
            name,
            total_ns: AtomicU64::new(0),
            runs: AtomicU32::new(0),
            nests: AtomicBool::new(false),
        }
    }

    fn record(&self, elapsed: Duration, nests: bool) {
        self.total_ns
            .fetch_add(elapsed.as_nanos() as u64, Ordering::Relaxed);
        self.runs.fetch_add(1, Ordering::Relaxed);
        if nests {
            self.nests.store(true, Ordering::Relaxed);
        }
    }

    /// This frame's totals, reset to zero.
    fn take(&self) -> FrameAccum {
        FrameAccum {
            total: Duration::from_nanos(self.total_ns.swap(0, Ordering::Relaxed)),
            runs: self.runs.swap(0, Ordering::Relaxed),
            nests: self.nests.load(Ordering::Relaxed),
        }
    }
}

#[derive(Clone, Copy, Default)]
struct FrameAccum {
    total: Duration,
    runs: u32,
    nests: bool,
}

/// Every system the layer has seen, in first-seen order. Appended to only
/// from `on_new_span` (once per system for the process lifetime), read once
/// per frame by [`collect_system_timings`]; the hot path never touches it.
#[derive(Resource, Clone, Default)]
pub struct SpanRegistry(Arc<Mutex<Vec<Arc<Slot>>>>);

/// `LogPlugin::custom_layer` hook: creates the shared registry, hands it to
/// the app as a resource, and returns the layer that fills it.
pub fn timing_layer(app: &mut App) -> Option<BoxedLayer> {
    let shared = SpanRegistry::default();
    app.insert_resource(shared.clone());
    Some(Box::new(TimingLayer(shared.0)))
}

struct TimingLayer(Arc<Mutex<Vec<Arc<Slot>>>>);

/// One open system span on this thread.
///
/// Some systems run whole schedules: `Main::run_main`, `run_fixed_main_schedule`,
/// bevy_render's `run_render_schedule` (the entire render frame). Their spans
/// *contain* other systems' spans, so a plain duration counts that work twice
/// and puts the wrapper at the top of the table. Span guards can't cross
/// threads, so a per-thread stack sees exactly the nesting that happened on
/// it: a child's duration is taken off its parent's (self time) and the parent
/// is flagged. Children the executor ran on *other* threads can't be
/// attributed this way, so a schedule runner's self time is mostly it waiting
/// for them — which is why flagged rows are kept out of the total.
struct ThreadFrame {
    id: Id,
    start: Instant,
    nested: Duration,
    nests: bool,
}

thread_local! {
    static OPEN: RefCell<Vec<ThreadFrame>> = const { RefCell::new(Vec::new()) };
}

/// Pulls the `name = <system path>` field out of a span's attributes.
struct NameVisitor(Option<String>);

impl Visit for NameVisitor {
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "name" {
            self.0 = Some(value.to_owned());
        }
    }
    fn record_debug(&mut self, _: &Field, _: &dyn std::fmt::Debug) {}
}

impl<S> Layer<S> for TimingLayer
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_new_span(&self, attrs: &Attributes<'_>, id: &Id, ctx: Context<'_, S>) {
        if attrs.metadata().name() != "system" {
            return;
        }
        let mut visitor = NameVisitor(None);
        attrs.record(&mut visitor);
        let Some(name) = visitor.0 else { return };
        let slot = Arc::new(Slot::new(name));
        self.0.lock().unwrap().push(slot.clone());
        if let Some(span) = ctx.span(id) {
            span.extensions_mut().insert(slot);
        }
    }

    fn on_enter(&self, id: &Id, ctx: Context<'_, S>) {
        let Some(span) = ctx.span(id) else { return };
        if span.metadata().name() != "system" {
            return;
        }
        OPEN.with_borrow_mut(|open| {
            if let Some(parent) = open.last_mut() {
                parent.nests = true;
            }
            open.push(ThreadFrame {
                id: id.clone(),
                start: Instant::now(),
                nested: Duration::ZERO,
                nests: false,
            });
        });
    }

    fn on_exit(&self, id: &Id, ctx: Context<'_, S>) {
        let Some(span) = ctx.span(id) else { return };
        if span.metadata().name() != "system" {
            return;
        }
        let Some((frame, elapsed)) = OPEN.with_borrow_mut(|open| {
            // Guards drop in reverse order, so this is the top; search anyway
            // rather than corrupt the stack if that ever isn't true.
            let i = open.iter().rposition(|f| f.id == *id)?;
            let frame = open.remove(i);
            let elapsed = frame.start.elapsed();
            if let Some(parent) = i.checked_sub(1).map(|p| &mut open[p]) {
                parent.nested += elapsed;
            }
            Some((frame, elapsed))
        }) else {
            return;
        };
        if let Some(slot) = span.extensions().get::<Arc<Slot>>() {
            slot.record(elapsed.saturating_sub(frame.nested), frame.nests);
        }
    }
}

// ---------------------------------------------------------------------------
// Per-frame aggregation
// ---------------------------------------------------------------------------

/// Smoothing factor: ~0.05 means a step settles over roughly 60 frames —
/// steady enough to read as a table, quick enough to see a toggle's effect.
const EMA_ALPHA: f32 = 0.05;

/// Hide a system once its average falls this far below a microsecond; keeps
/// one-shot startup systems from pinning rows forever. (The slot stays — a
/// system that runs again just reappears.)
const FORGET_BELOW_MS: f32 = 1e-4;

/// Smoothed per-system cost, in display order (most expensive first).
///
/// `stats[i]` belongs to the registry's `i`-th slot; slots are only ever
/// appended, so the index is stable and nothing here hashes a name.
#[derive(Resource, Default)]
pub struct SystemTimings {
    names: Vec<Arc<Slot>>,
    stats: Vec<SystemStat>,
    /// Rebuilt after each absorb: indices of rows above the forget threshold,
    /// sorted by `ms` descending.
    order: Vec<usize>,
}

#[derive(Clone, Copy, Default)]
pub struct SystemStat {
    /// EMA of milliseconds per main-world frame.
    pub ms: f32,
    /// EMA of runs per main-world frame (FixedUpdate systems exceed 1).
    pub runs: f32,
    /// Has been seen running other systems inside itself: a schedule runner,
    /// whose `ms` is mostly waiting (see `ThreadFrame`). Sticky — a fixed-step
    /// runner only nests on frames that take a step.
    pub nests: bool,
}

impl SystemTimings {
    pub fn is_empty(&self) -> bool {
        self.order.is_empty()
    }

    /// Rows in display order.
    pub fn rows(&self) -> impl Iterator<Item = (&str, SystemStat)> {
        self.order
            .iter()
            .map(|&i| (self.names[i].name.as_str(), self.stats[i]))
    }

    /// Sum of every tracked system's self time, both threads together.
    /// Schedule runners are left out: their self time is waiting, not work.
    pub fn total_ms(&self) -> f32 {
        self.order
            .iter()
            .map(|&i| self.stats[i])
            .filter(|s| !s.nests)
            .map(|s| s.ms)
            .sum()
    }

    /// Fold one frame in. `frame[i]` is slot `i`'s accumulation; slots past
    /// the end are ones the registry has grown since last time.
    fn absorb(&mut self, slots: &[Arc<Slot>], frame: impl Iterator<Item = FrameAccum>) {
        if self.names.len() < slots.len() {
            self.names.extend(slots[self.names.len()..].iter().cloned());
            self.stats.resize(slots.len(), SystemStat::default());
        }
        // Decay everything, then add this frame's share: a system that didn't
        // run contributes 0 to its average, same as a 0-ms run would.
        for (stat, accum) in self.stats.iter_mut().zip(frame) {
            stat.ms = stat.ms * (1.0 - EMA_ALPHA) + accum.total.as_secs_f32() * 1000.0 * EMA_ALPHA;
            stat.runs = stat.runs * (1.0 - EMA_ALPHA) + accum.runs as f32 * EMA_ALPHA;
            stat.nests |= accum.nests;
        }
        self.order.clear();
        self.order
            .extend((0..self.stats.len()).filter(|&i| self.stats[i].ms >= FORGET_BELOW_MS));
        let stats = &self.stats;
        let names = &self.names;
        self.order.sort_by(|&a, &b| {
            stats[b]
                .ms
                .total_cmp(&stats[a].ms)
                .then_with(|| names[a].name.cmp(&names[b].name))
        });
    }
}

/// The whole Systems (CPU) table plus the frame breakdown, as markdown.
///
/// Exists so a before/after measurement is a file rather than a transcription
/// from a screenshot — `docs/SCENE_PASS_BASELINE.md` is kept from these. Full
/// system paths, not [`short_name`]s: telling bevy_pbr's rows from ours is the
/// entire point of the exercise.
pub fn snapshot_markdown(
    timings: &SystemTimings,
    frame: Option<FrameSample>,
    note: &str,
) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();

    let _ = writeln!(out, "### {note}\n");
    match frame {
        Some(m) => {
            let _ = writeln!(
                out,
                "Frame {:.2} ms · main {:.2} · render {:.2} busy + {:.2} waiting\n",
                m.frame, m.main, m.render_busy, m.render_wait
            );
        }
        None => {
            let _ = writeln!(out, "Frame breakdown: not enough samples yet.\n");
        }
    }
    if timings.is_empty() {
        let _ = writeln!(
            out,
            "No per-system spans: this is not a `cargo run-profiling` build."
        );
        return out;
    }

    let _ = writeln!(
        out,
        "{:.3} ms of system work per frame, both threads (smoothed self time).\n",
        timings.total_ms()
    );
    let _ = writeln!(out, "| ms | runs | system |");
    let _ = writeln!(out, "|---:|-----:|--------|");
    for (name, stat) in timings.rows().filter(|(_, s)| !s.nests) {
        let _ = writeln!(out, "| {:.3} | {:.1} | `{name}` |", stat.ms, stat.runs);
    }
    let _ = writeln!(out, "\nSchedule runners (waiting, not work):\n");
    for (name, stat) in timings.rows().filter(|(_, s)| s.nests) {
        let _ = writeln!(out, "- {:.3} ms `{name}`", stat.ms);
    }
    out
}

/// Write a snapshot to `perf-snapshot-<unix-seconds>.md` in the working
/// directory (gitignored) and return the text, so a caller can also put it on
/// the clipboard. `context` names what was being measured ("gameplay",
/// "editor") and goes in the heading; the file is which build it came from
/// only by its row names, so say so in the doc you paste it into.
pub fn save_snapshot(timings: &SystemTimings, breakdown: &FrameBreakdown, context: &str) -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let note = format!("perf snapshot {secs} ({context})");
    let text = snapshot_markdown(timings, breakdown.median(), &note);

    #[cfg(not(target_arch = "wasm32"))]
    {
        let path = std::path::PathBuf::from(format!("perf-snapshot-{secs}.md"));
        match std::fs::write(&path, &text) {
            Ok(()) => info!("perf snapshot ({context}) written to {}", path.display()),
            Err(e) => warn!("could not write {}: {e}", path.display()),
        }
    }
    #[cfg(target_arch = "wasm32")]
    info!("perf snapshot ({context}):\n{text}");

    text
}

/// F9: save a snapshot from wherever you are. The panel's button only exists
/// in the editor, and the editor's own egui pass is the most expensive row in
/// the table — so a *gameplay* measurement, which is what the renderer work
/// is judged against, needs a key.
fn snapshot_on_key(
    keys: Res<ButtonInput<KeyCode>>,
    timings: Res<SystemTimings>,
    breakdown: Res<FrameBreakdown>,
    mode: Option<Res<State<EditorMode>>>,
) {
    if !keys.just_pressed(KeyCode::F9) {
        return;
    }
    let context = match mode.as_deref().map(|m| m.get()) {
        Some(EditorMode::Gameplay) => "gameplay",
        Some(EditorMode::Editor) => "editor",
        None => "menu",
    };
    save_snapshot(&timings, &breakdown, context);
}

/// The last two path segments: enough to tell `selection::validate_selection`
/// from another crate's `validate_selection` without eating the panel's width.
/// The full path belongs in the row's hover text.
pub fn short_name(full: &str) -> &str {
    let mut cut = full.len();
    for _ in 0..2 {
        match full[..cut].rfind("::") {
            Some(i) => cut = i,
            None => return full,
        }
    }
    &full[cut + 2..]
}

fn collect_system_timings(registry: Res<SpanRegistry>, mut timings: ResMut<SystemTimings>) {
    let slots = registry.0.lock().unwrap();
    if slots.is_empty() {
        return; // The common case outside profiling builds: stay unchanged.
    }
    timings.absorb(&slots, slots.iter().map(|s| s.take()));
}

pub struct SystemTimingPlugin;

impl Plugin for SystemTimingPlugin {
    fn build(&self, app: &mut App) {
        // The registry normally exists already (inserted by `timing_layer`
        // when LogPlugin built); this is only a fallback so `collect` can
        // take a plain `Res` even if the log plugin is ever disabled.
        app.init_resource::<SpanRegistry>()
            .init_resource::<SystemTimings>()
            .add_systems(Last, collect_system_timings)
            .add_systems(Update, snapshot_on_key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slots(names: &[&str]) -> Vec<Arc<Slot>> {
        names
            .iter()
            .map(|n| Arc::new(Slot::new(n.to_string())))
            .collect()
    }

    fn frame(entries: &[(u64, u32)]) -> impl Iterator<Item = FrameAccum> + '_ {
        entries.iter().map(|(us, runs)| FrameAccum {
            total: Duration::from_micros(*us),
            runs: *runs,
            nests: false,
        })
    }

    #[test]
    fn rows_are_ordered_by_cost_and_converge_to_the_steady_state() {
        let slots = slots(&["a::cheap", "b::dear"]);
        let mut t = SystemTimings::default();
        for _ in 0..400 {
            t.absorb(&slots, frame(&[(100, 1), (2000, 1)]));
        }
        let rows: Vec<_> = t.rows().map(|(n, s)| (n.to_owned(), s)).collect();
        assert_eq!(rows[0].0, "b::dear");
        assert_eq!(rows[1].0, "a::cheap");
        assert!((rows[0].1.ms - 2.0).abs() < 0.01, "{}", rows[0].1.ms);
        assert!((rows[1].1.ms - 0.1).abs() < 0.01);
        assert!((rows[0].1.runs - 1.0).abs() < 0.01);
        assert!((t.total_ms() - 2.1).abs() < 0.02);
    }

    #[test]
    fn a_system_that_stops_running_fades_out_and_is_hidden() {
        let slots = slots(&["startup::once"]);
        let mut t = SystemTimings::default();
        t.absorb(&slots, frame(&[(5000, 1)]));
        assert!(!t.is_empty());
        for _ in 0..400 {
            t.absorb(&slots, frame(&[(0, 0)]));
        }
        assert!(t.is_empty(), "still showing {:?}", t.order);
    }

    /// Slots the registry gains mid-run get rows without disturbing the old
    /// ones' indices.
    #[test]
    fn late_slots_are_appended() {
        let mut all = slots(&["a::first"]);
        let mut t = SystemTimings::default();
        t.absorb(&all, frame(&[(1000, 1)]));
        all.push(Arc::new(Slot::new("b::second".into())));
        t.absorb(&all, frame(&[(1000, 1), (3000, 1)]));
        // Sorted by cost: the newcomer is the dearer one.
        let names: Vec<_> = t.rows().map(|(n, _)| n.to_owned()).collect();
        assert_eq!(names, ["b::second", "a::first"]);
        let first = t.rows().find(|(n, _)| *n == "a::first").unwrap().1;
        assert!((first.ms - 0.0975).abs() < 1e-3, "{}", first.ms);
    }

    /// A schedule runner's span contains its children's: self time must take
    /// them off and flag the runner, and a leaf must keep its own time. Also
    /// the whole point of the rewrite: the hot path does no hashing — the
    /// registry sees exactly one entry per distinct span.
    #[test]
    fn nested_system_spans_are_charged_to_the_child_only() {
        use bevy::log::tracing_subscriber::{Registry, layer::SubscriberExt};
        let registry = SpanRegistry::default();
        let subscriber = Registry::default().with(TimingLayer(registry.0.clone()));
        tracing::subscriber::with_default(subscriber, || {
            let runner = tracing::info_span!("system", name = "app::run_schedule");
            let child = tracing::info_span!("system", name = "game::busy");
            for _ in 0..3 {
                let _r = runner.enter();
                let _c = child.enter();
                std::thread::sleep(Duration::from_millis(5));
            }
        });
        let slots = registry.0.lock().unwrap();
        assert_eq!(slots.len(), 2, "one slot per span, not per run");
        let runner = slots[0].take();
        let child = slots[1].take();
        assert!(runner.nests && !child.nests);
        assert!(child.total >= Duration::from_millis(15));
        assert!(
            runner.total < Duration::from_millis(5),
            "runner charged {:?} of its child's time",
            runner.total
        );
        assert_eq!((runner.runs, child.runs), (3, 3));
        // `take` resets.
        assert_eq!(slots[1].take().runs, 0);
    }

    #[test]
    fn schedule_runners_stay_out_of_the_total() {
        let slots = slots(&["a::work", "b::runner"]);
        let mut t = SystemTimings::default();
        let f = [
            FrameAccum {
                total: Duration::from_micros(1000),
                runs: 1,
                nests: false,
            },
            FrameAccum {
                total: Duration::from_micros(9000),
                runs: 1,
                nests: true,
            },
        ];
        t.absorb(&slots, f.into_iter());
        assert!((t.total_ms() - 1.0 * EMA_ALPHA).abs() < 1e-4);
    }

    #[test]
    fn short_names_keep_the_last_two_segments() {
        assert_eq!(
            short_name("game_engine_lib::editor::selection::validate_selection"),
            "selection::validate_selection"
        );
        assert_eq!(short_name("lonely"), "lonely");
        assert_eq!(short_name("two::parts"), "two::parts");
    }
}
