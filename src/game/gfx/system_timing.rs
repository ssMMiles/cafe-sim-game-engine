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
//!   shared map keyed by system name ([`SpanAccumulator`]). Systems that run
//!   schedules are flagged rather than counted (`ThreadFrame`).
//! - [`collect_system_timings`] (in bevy's `Last`) takes that map once per
//!   main-world frame and folds it into [`SystemTimings`], an exponential
//!   moving average per system, which is what the panel reads. The EMA is
//!   the smoothing: render-thread spans land whenever that thread finishes,
//!   so a single frame's slice is jittery by construction.
//!
//! The layer is registered in every build. Without system spans its hooks
//! only ever see bevy's few log spans and return on the name check, so the
//! normal build pays nothing measurable.

use std::{
    cell::RefCell,
    collections::HashMap,
    sync::{Arc, Mutex},
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

/// Raw durations since the last collection, filled by the tracing layer from
/// any thread, drained once per frame by [`collect_system_timings`].
#[derive(Resource, Clone, Default)]
pub struct SpanAccumulator(Arc<Mutex<HashMap<String, FrameAccum>>>);

#[derive(Clone, Copy, Default)]
struct FrameAccum {
    /// Self time: the span's duration minus the system spans nested in it.
    total: Duration,
    runs: u32,
    /// Another system ran inside this one (see [`ThreadFrame`]).
    nests: bool,
}

/// `LogPlugin::custom_layer` hook: creates the shared accumulator, hands it
/// to the app as a resource, and returns the layer that writes into it.
pub fn timing_layer(app: &mut App) -> Option<BoxedLayer> {
    let shared = SpanAccumulator::default();
    app.insert_resource(shared.clone());
    Some(Box::new(TimingLayer(shared.0)))
}

struct TimingLayer(Arc<Mutex<HashMap<String, FrameAccum>>>);

/// The system path, stored in the span's extensions when bevy creates it
/// (one cached span per system, re-entered every run).
struct SystemName(String);

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
        if let Some(span) = ctx.span(id) {
            span.extensions_mut().insert(SystemName(name));
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
        let extensions = span.extensions();
        let Some(SystemName(name)) = extensions.get::<SystemName>() else {
            return;
        };
        let mut map = self.0.lock().unwrap();
        let accum = map.entry(name.clone()).or_default();
        accum.total += elapsed.saturating_sub(frame.nested);
        accum.runs += 1;
        accum.nests |= frame.nests;
    }
}

// ---------------------------------------------------------------------------
// Per-frame aggregation
// ---------------------------------------------------------------------------

/// Smoothing factor: ~0.05 means a step settles over roughly 60 frames —
/// steady enough to read as a table, quick enough to see a toggle's effect.
const EMA_ALPHA: f32 = 0.05;

/// Forget a system once its average falls this far below a microsecond; keeps
/// one-shot startup systems from pinning rows forever.
const FORGET_BELOW_MS: f32 = 1e-4;

/// Smoothed per-system cost, in display order (most expensive first).
#[derive(Resource, Default)]
pub struct SystemTimings {
    stats: HashMap<String, SystemStat>,
    /// Rebuilt after each absorb: names sorted by `ms` descending.
    order: Vec<String>,
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
            .map(|name| (name.as_str(), self.stats[name]))
    }

    /// Sum of every tracked system's self time, both threads together.
    /// Schedule runners are left out: their self time is waiting, not work.
    pub fn total_ms(&self) -> f32 {
        self.stats.values().filter(|s| !s.nests).map(|s| s.ms).sum()
    }

    fn absorb(&mut self, frame: HashMap<String, FrameAccum>) {
        // Decay everything, then add this frame's share: a system that didn't
        // run contributes 0 to its average, same as a 0-ms run would.
        self.stats.retain(|name, stat| {
            stat.ms *= 1.0 - EMA_ALPHA;
            stat.runs *= 1.0 - EMA_ALPHA;
            stat.ms >= FORGET_BELOW_MS || frame.contains_key(name)
        });
        for (name, accum) in frame {
            let stat = self.stats.entry(name).or_default();
            stat.ms += accum.total.as_secs_f32() * 1000.0 * EMA_ALPHA;
            stat.runs += accum.runs as f32 * EMA_ALPHA;
            stat.nests |= accum.nests;
        }
        self.order = self.stats.keys().cloned().collect();
        self.order.sort_by(|a, b| {
            self.stats[b]
                .ms
                .total_cmp(&self.stats[a].ms)
                .then_with(|| a.cmp(b))
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
    frame: Option<super::frame_pacing::FrameSample>,
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

fn collect_system_timings(acc: Res<SpanAccumulator>, mut timings: ResMut<SystemTimings>) {
    let frame = std::mem::take(&mut *acc.0.lock().unwrap());
    if frame.is_empty() && timings.is_empty() {
        return; // The common case outside profiling builds: stay unchanged.
    }
    timings.absorb(frame);
}

pub struct SystemTimingPlugin;

impl Plugin for SystemTimingPlugin {
    fn build(&self, app: &mut App) {
        // The accumulator normally exists already (inserted by `timing_layer`
        // when LogPlugin built); this is only a fallback so `collect` can
        // take a plain `Res` even if the log plugin is ever disabled.
        app.init_resource::<SpanAccumulator>()
            .init_resource::<SystemTimings>()
            .add_systems(Last, collect_system_timings);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(entries: &[(&str, u64, u32)]) -> HashMap<String, FrameAccum> {
        entries
            .iter()
            .map(|(name, us, runs)| {
                (
                    name.to_string(),
                    FrameAccum {
                        total: Duration::from_micros(*us),
                        runs: *runs,
                        nests: false,
                    },
                )
            })
            .collect()
    }

    #[test]
    fn rows_are_ordered_by_cost_and_converge_to_the_steady_state() {
        let mut t = SystemTimings::default();
        for _ in 0..400 {
            t.absorb(frame(&[("a::cheap", 100, 1), ("b::dear", 2000, 1)]));
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
    fn a_system_that_stops_running_fades_out_and_is_forgotten() {
        let mut t = SystemTimings::default();
        t.absorb(frame(&[("startup::once", 5000, 1)]));
        assert!(!t.is_empty());
        for _ in 0..400 {
            t.absorb(HashMap::new());
        }
        assert!(t.is_empty(), "still tracking {:?}", t.order);
    }

    /// A schedule runner's span contains its children's: self time must take
    /// them off and flag the runner, and a leaf must keep its own time.
    #[test]
    fn nested_system_spans_are_charged_to_the_child_only() {
        use bevy::log::tracing_subscriber::{Registry, layer::SubscriberExt};
        let acc = SpanAccumulator::default();
        let subscriber = Registry::default().with(TimingLayer(acc.0.clone()));
        tracing::subscriber::with_default(subscriber, || {
            let runner = tracing::info_span!("system", name = "app::run_schedule");
            let child = tracing::info_span!("system", name = "game::busy");
            let _r = runner.enter();
            let _c = child.enter();
            std::thread::sleep(Duration::from_millis(20));
        });
        let map = acc.0.lock().unwrap();
        let runner = map["app::run_schedule"];
        let child = map["game::busy"];
        assert!(runner.nests && !child.nests);
        assert!(child.total >= Duration::from_millis(20));
        assert!(
            runner.total < Duration::from_millis(5),
            "runner charged {:?} of its child's time",
            runner.total
        );
        assert_eq!((runner.runs, child.runs), (1, 1));
    }

    #[test]
    fn schedule_runners_stay_out_of_the_total() {
        let mut t = SystemTimings::default();
        let mut f = frame(&[("a::work", 1000, 1), ("b::runner", 9000, 1)]);
        f.get_mut("b::runner").unwrap().nests = true;
        t.absorb(f);
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
