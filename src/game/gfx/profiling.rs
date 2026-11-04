//! Measuring what the renderer costs, from inside the editor.
//!
//! Two independent things live here because they answer the same question
//! on different hardware:
//!
//! - [`GpuTimingSupport`] records what the live adapter can actually time.
//!   Bevy's `RenderDiagnosticsPlugin` brackets each pass in timestamp
//!   queries written *inside* the render pass, which needs
//!   `TIMESTAMP_QUERY_INSIDE_PASSES`. Metal does not have it (Apple GPUs
//!   sample counters at stage boundaries, not at arbitrary points in a
//!   pass), so on macOS every per-pass span silently records nothing — the
//!   recorder only falls back to CPU timings when there is no query set at
//!   all, and `TIMESTAMP_QUERY` on its own is enough to create one. The
//!   editor reads this flag so it can say that, rather than showing an
//!   empty list.
//!
//! - [`EffectBench`] is an A/B measurement that needs no GPU timers: it
//!   switches one effect off and on for a few hundred frames and compares
//!   the frame times. That works on every platform, and it measures what a
//!   player would actually feel. Its weakness is the mirror image of the
//!   timestamps': it only sees a difference while the frame is *GPU-bound*,
//!   so it reports the sample spread alongside the delta and lets the
//!   reader judge.
//!
//! `docs/ATMOSPHERE.md` §7 is the user-facing version of all this.

use std::sync::{
    Arc,
    atomic::{AtomicU8, Ordering},
};

use bevy::{
    prelude::*,
    render::{RenderApp, RenderStartup, renderer::RenderDevice, settings::WgpuFeatures},
    time::Real,
};

/// Frames at the start of each phase that are thrown away, so a pipeline
/// compile or a texture upload triggered by the switch can't land in the
/// samples.
const WARMUP_FRAMES: u32 = 20;
/// Frames measured per phase, after the warm-up.
const MEASURED_FRAMES: u32 = 80;
/// Phases per run, alternating off/on. More phases average out slow drift
/// (thermal throttling, a background process) that would otherwise land
/// entirely on whichever phase ran last.
const PHASES: u32 = 8;

// ---------------------------------------------------------------------------
// What this adapter can time
// ---------------------------------------------------------------------------

const HAS_TIMESTAMPS: u8 = 1 << 0;
const HAS_TIMESTAMPS_IN_PASSES: u8 = 1 << 1;
const DETECTED: u8 = 1 << 2;

/// Whether bevy's per-pass GPU spans can produce anything on this adapter.
///
/// Shared from the render world through an `Arc`, the same way
/// `GpuBakeSupported` is: the features live on the render device, the
/// editor panel that needs to explain itself lives in the main world.
#[derive(Resource, Clone, Default)]
pub struct GpuTimingSupport(Arc<AtomicU8>);

impl GpuTimingSupport {
    fn bits(&self) -> u8 {
        self.0.load(Ordering::Relaxed)
    }

    /// `None` until `RenderStartup` has run.
    pub fn detected(&self) -> bool {
        self.bits() & DETECTED != 0
    }

    /// The adapter has timestamp queries at all.
    pub fn timestamps(&self) -> bool {
        self.bits() & HAS_TIMESTAMPS != 0
    }

    /// Timestamps can be written *inside* a render pass, which is what
    /// bevy's `pass_span` needs.
    pub fn inside_passes(&self) -> bool {
        self.bits() & HAS_TIMESTAMPS_IN_PASSES != 0
    }

    /// One line explaining what the GPU timings list can show here.
    pub fn explain(&self) -> &'static str {
        if !self.detected() {
            "Checking what this adapter supports…"
        } else if self.inside_passes() {
            "Per-pass GPU times, measured with timestamp queries."
        } else if self.timestamps() {
            "This adapter has timestamp queries but can't write them inside a \
             render pass (normal on Metal/macOS), which is what bevy's per-pass \
             spans need — so the list stays empty. Use the A/B measurement \
             below, or a GPU capture in Xcode."
        } else {
            "This adapter has no timestamp queries, so per-pass GPU times \
             aren't available. Use the A/B measurement below."
        }
    }
}

fn detect_gpu_timing_support(device: Res<RenderDevice>, support: Res<GpuTimingSupport>) {
    let features = device.features();
    let mut bits = DETECTED;
    if features.contains(WgpuFeatures::TIMESTAMP_QUERY) {
        bits |= HAS_TIMESTAMPS;
    }
    if features.contains(WgpuFeatures::TIMESTAMP_QUERY_INSIDE_PASSES) {
        bits |= HAS_TIMESTAMPS_IN_PASSES;
    }
    support.0.store(bits, Ordering::Relaxed);

    if bits & HAS_TIMESTAMPS_IN_PASSES == 0 {
        info!(
            "Per-pass GPU timings unavailable (TIMESTAMP_QUERY={}, \
             TIMESTAMP_QUERY_INSIDE_PASSES=false); the editor's A/B \
             measurement works regardless",
            bits & HAS_TIMESTAMPS != 0
        );
    }
}

// ---------------------------------------------------------------------------
// A/B measurement
// ---------------------------------------------------------------------------

/// An effect an A/B run can switch off. These are the things with a
/// per-frame GPU cost that the editor can toggle without changing what the
/// level looks like permanently.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum BenchEffect {
    #[default]
    GodRays,
    Fog,
    EdgeDetection,
}

impl BenchEffect {
    pub const ALL: [BenchEffect; 3] = [
        BenchEffect::GodRays,
        BenchEffect::Fog,
        BenchEffect::EdgeDetection,
    ];

    pub fn label(self) -> &'static str {
        match self {
            BenchEffect::GodRays => "God rays",
            BenchEffect::Fog => "Fog",
            BenchEffect::EdgeDetection => "Edge detection",
        }
    }
}

/// Set by a running benchmark; read by the sync systems that build the
/// render uniforms. Forcing the effect off *here* rather than in
/// [`AtmosphereStyle`](super::atmosphere::AtmosphereStyle) keeps the
/// authored settings — and therefore `editor_settings.json` — untouched by
/// a measurement.
#[derive(Resource, Default)]
pub struct EffectOverride(Option<BenchEffect>);

impl EffectOverride {
    /// Whether `effect` should be forced off this frame.
    pub fn suppressed(&self, effect: BenchEffect) -> bool {
        self.0 == Some(effect)
    }
}

/// One A/B run in progress, plus the last result.
#[derive(Resource, Default)]
pub struct EffectBench {
    run: Option<BenchRun>,
    pub last: Option<BenchResult>,
}

impl EffectBench {
    pub fn start(&mut self, effect: BenchEffect) {
        self.run = Some(BenchRun::new(effect));
        self.last = None;
    }

    pub fn cancel(&mut self) {
        self.run = None;
    }

    pub fn running(&self) -> Option<&BenchRun> {
        self.run.as_ref()
    }
}

pub struct BenchRun {
    effect: BenchEffect,
    /// Phases finished so far.
    phase: u32,
    /// Frames into the current phase.
    frame: u32,
    /// Whether the effect is forced off for the current phase.
    off_phase: bool,
    with: Vec<f32>,
    without: Vec<f32>,
}

impl BenchRun {
    fn new(effect: BenchEffect) -> Self {
        Self {
            effect,
            phase: 0,
            frame: 0,
            // Start with the effect *off*, so the very first phase — the one
            // most likely to still be settling — is the cheap one.
            off_phase: true,
            with: Vec::new(),
            without: Vec::new(),
        }
    }

    pub fn effect(&self) -> BenchEffect {
        self.effect
    }

    /// 0..1 through the run.
    pub fn progress(&self) -> f32 {
        let per_phase = WARMUP_FRAMES + MEASURED_FRAMES;
        let done = self.phase * per_phase + self.frame;
        done as f32 / (PHASES * per_phase) as f32
    }

    /// What to force off this frame.
    fn suppressed(&self) -> Option<BenchEffect> {
        self.off_phase.then_some(self.effect)
    }

    /// Feed one frame's wall time. Returns the result on the final frame.
    fn tick(&mut self, frame_ms: f32) -> Option<BenchResult> {
        // Frames right after a switch are discarded: the first one also
        // carries the cost of whatever the switch itself triggered.
        if self.frame >= WARMUP_FRAMES && frame_ms.is_finite() {
            if self.off_phase {
                self.without.push(frame_ms);
            } else {
                self.with.push(frame_ms);
            }
        }
        self.frame += 1;

        if self.frame < WARMUP_FRAMES + MEASURED_FRAMES {
            return None;
        }
        self.frame = 0;
        self.off_phase = !self.off_phase;
        self.phase += 1;
        if self.phase < PHASES {
            return None;
        }

        Some(BenchResult {
            effect: self.effect,
            with_ms: median(&mut self.with),
            without_ms: median(&mut self.without),
            with_spread: iqr(&mut self.with),
            without_spread: iqr(&mut self.without),
            samples: self.with.len().min(self.without.len()),
        })
    }
}

/// What an A/B run found. Medians rather than means, and an interquartile
/// spread rather than a standard deviation, because frame times are full of
/// one-sided outliers (a stream load, the OS scheduling something else) that
/// would drag a mean around.
#[derive(Clone, Copy, Debug)]
pub struct BenchResult {
    pub effect: BenchEffect,
    pub with_ms: f32,
    pub without_ms: f32,
    pub with_spread: f32,
    pub without_spread: f32,
    pub samples: usize,
}

impl BenchResult {
    /// What the effect costs per frame, in milliseconds.
    pub fn delta_ms(&self) -> f32 {
        self.with_ms - self.without_ms
    }

    /// Whether the difference is bigger than the noise it was measured in.
    /// Deliberately conservative: the spread of a single phase is already an
    /// optimistic estimate of run-to-run variation.
    pub fn significant(&self) -> bool {
        self.delta_ms().abs() > 0.5 * (self.with_spread + self.without_spread).max(1e-4)
    }
}

fn median(values: &mut [f32]) -> f32 {
    if values.is_empty() {
        return 0.0;
    }
    values.sort_by(f32::total_cmp);
    let mid = values.len() / 2;
    if values.len() % 2 == 0 {
        (values[mid - 1] + values[mid]) * 0.5
    } else {
        values[mid]
    }
}

/// Interquartile range: the width of the middle half of the samples.
fn iqr(values: &mut [f32]) -> f32 {
    if values.len() < 4 {
        return 0.0;
    }
    values.sort_by(f32::total_cmp);
    let q = |f: f32| values[((values.len() as f32 * f) as usize).min(values.len() - 1)];
    (q(0.75) - q(0.25)).max(0.0)
}

/// Drive a running benchmark: set the override for this frame, record the
/// last frame's wall time, and finish the run when it has enough samples.
///
/// Ordered before the uniform sync systems so the override it sets takes
/// effect on the frame it measures.
pub fn run_effect_bench(
    time: Res<Time<Real>>,
    mut bench: ResMut<EffectBench>,
    mut over: ResMut<EffectOverride>,
) {
    let Some(run) = bench.run.as_mut() else {
        if over.0.is_some() {
            over.0 = None;
        }
        return;
    };
    over.0 = run.suppressed();
    if let Some(result) = run.tick(time.delta_secs() * 1000.0) {
        bench.run = None;
        bench.last = Some(result);
        over.0 = None;
    }
}

pub struct GfxProfilingPlugin;

impl Plugin for GfxProfilingPlugin {
    fn build(&self, app: &mut App) {
        let support = GpuTimingSupport::default();
        app.insert_resource(support.clone());
        app.init_resource::<EffectOverride>();
        app.init_resource::<EffectBench>();

        let Some(render_app) = app.get_sub_app_mut(RenderApp) else {
            return;
        };
        render_app.insert_resource(support);
        render_app.add_systems(RenderStartup, detect_gpu_timing_support);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn median_and_iqr_ignore_outliers() {
        let mut v = vec![10.0, 10.0, 10.0, 10.0, 10.0, 10.0, 10.0, 900.0];
        assert_eq!(median(&mut v), 10.0);
        assert_eq!(iqr(&mut v), 0.0);

        let mut v = vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0];
        assert_eq!(median(&mut v), 4.5);
        assert!(iqr(&mut v) > 0.0);
    }

    /// Drive a whole run with synthetic frame times and check it recovers
    /// the cost it was given. This is the test that says the measurement
    /// means what the panel claims it means.
    #[test]
    fn a_b_run_recovers_a_known_cost() {
        const BASE: f32 = 8.0;
        const COST: f32 = 1.25;

        let mut run = BenchRun::new(BenchEffect::GodRays);
        let mut result = None;
        let mut frames = 0;
        // A little jitter, and a spike every 37 frames that a mean would
        // notice and a median should not.
        for i in 0..100_000 {
            let suppressed = run.suppressed().is_some();
            let jitter = ((i % 7) as f32 - 3.0) * 0.02;
            let spike = if i % 37 == 0 { 25.0 } else { 0.0 };
            let ms = BASE + jitter + spike + if suppressed { 0.0 } else { COST };
            frames += 1;
            if let Some(r) = run.tick(ms) {
                result = Some(r);
                break;
            }
        }

        let r = result.expect("the run should finish");
        assert_eq!(frames, PHASES * (WARMUP_FRAMES + MEASURED_FRAMES));
        assert!(
            (r.delta_ms() - COST).abs() < 0.05,
            "measured {} ms, injected {COST} ms",
            r.delta_ms()
        );
        assert!(r.significant(), "a {COST} ms cost should read as real");
        assert_eq!(r.samples, (PHASES / 2 * MEASURED_FRAMES) as usize);
    }

    /// The warm-up frames must not reach the samples: they are where a
    /// pipeline compile caused by the switch would land.
    #[test]
    fn warmup_frames_are_discarded() {
        let mut run = BenchRun::new(BenchEffect::Fog);
        for frame in 0..WARMUP_FRAMES {
            // Absurd values that would wreck any average.
            assert!(run.tick(500.0 + frame as f32).is_none());
        }
        assert!(run.without.is_empty(), "warm-up leaked into the samples");
        run.tick(9.0);
        assert_eq!(run.without, vec![9.0]);
    }

    /// An effect that costs nothing must not be reported as costing
    /// something — the guard against reading noise as a result.
    #[test]
    fn no_cost_reads_as_insignificant() {
        let mut run = BenchRun::new(BenchEffect::EdgeDetection);
        let mut result = None;
        for i in 0..100_000 {
            let jitter = ((i % 11) as f32 - 5.0) * 0.1;
            if let Some(r) = run.tick(8.0 + jitter) {
                result = Some(r);
                break;
            }
        }
        let r = result.expect("the run should finish");
        assert!(
            !r.significant(),
            "pure noise reported as a real {} ms cost",
            r.delta_ms()
        );
    }

    #[test]
    fn a_run_alternates_phases_and_reports_progress() {
        let mut run = BenchRun::new(BenchEffect::GodRays);
        assert!(
            run.suppressed().is_some(),
            "the first phase is the cheap one"
        );
        assert_eq!(run.progress(), 0.0);
        for _ in 0..(WARMUP_FRAMES + MEASURED_FRAMES) {
            run.tick(8.0);
        }
        assert!(run.suppressed().is_none(), "phase two runs the effect");
        assert!((run.progress() - 1.0 / PHASES as f32).abs() < 1e-6);
    }
}
