//! Frame pacing: vsync, an optional frame-rate cap, and a measurement of
//! where each frame's time actually goes.
//!
//! **Frame time is not work time.** A frame that takes 16.7 ms might be
//! 3 ms of work and 13 ms of waiting for the display. The two threads are
//! measured separately so the editor can tell those apart:
//!
//! - **Main thread** — game + editor schedules, `FrameStart` → `FrameEnd`
//!   (two schedules bracketing bevy's `Main`, inserted around `First` and
//!   `Last`). The frame cap sleeps *after* this is recorded, so it never
//!   reads as work.
//! - **Render thread** — bevy's `Render` schedule (pipelined, so it runs
//!   alongside the next main frame), split into *busy* and *waiting for a
//!   swapchain image*. That wait is `prepare_windows`'
//!   `get_current_texture`, which bevy's own docs name as the place vsync
//!   and a GPU bottleneck both show up. It is bracketed directly.
//!
//! If the render thread spends most of the frame waiting, the limit is the
//! display (vsync on), the GPU, or — windowed on macOS — the compositor,
//! which paces drawables to the display even with vsync off. If a thread is
//! busy for most of the frame, the limit is CPU on that thread.
//!
//! [`DisplaySettings`] (vsync + cap) is persisted per user in
//! `editor_settings.json`; the editor's Performance panel edits it and
//! shows the breakdown. `docs/ATMOSPHERE.md` §7.

use std::{
    collections::VecDeque,
    sync::{
        Arc,
        atomic::{AtomicU32, Ordering},
    },
    time::Duration,
};

use bevy::{
    app::MainScheduleOrder,
    ecs::schedule::ScheduleLabel,
    platform::time::Instant,
    prelude::*,
    render::{Render, RenderApp, RenderSystems, view::prepare_windows},
    time::Real,
    window::{PresentMode, PrimaryWindow},
};
use serde::{Deserialize, Serialize};

/// Highest cap the panel offers. Above this, sleep granularity dominates
/// and "uncapped" is the honest setting. Native only, like the cap itself.
#[cfg(not(target_arch = "wasm32"))]
pub const FRAME_CAP_MAX: u32 = 500;

/// Frames of history the breakdown keeps, matching bevy's frame-time
/// diagnostic so the two read over the same window.
const HISTORY: usize = 120;

/// Runs before bevy's `First`, so the main-thread clock starts before any
/// game system.
#[derive(ScheduleLabel, Debug, Clone, PartialEq, Eq, Hash)]
struct FrameStart;

/// Runs after bevy's `Last`: stops the main-thread clock, then applies the
/// frame cap.
#[derive(ScheduleLabel, Debug, Clone, PartialEq, Eq, Hash)]
struct FrameEnd;

// ---------------------------------------------------------------------------
// Vsync and the frame cap
// ---------------------------------------------------------------------------

/// How frames are presented and paced. Per user, not per world.
#[derive(Resource, Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DisplaySettings {
    /// On: present in step with the display (`AutoVsync` → Fifo). Off:
    /// present as fast as possible (`AutoNoVsync` → Immediate on Metal,
    /// DX12 and Vulkan). Browsers always pace to the display.
    pub vsync: bool,
    /// Frames per second the main loop is held to; 0 = uncapped. Native
    /// only — blocking the browser's main thread isn't allowed.
    pub frame_cap: u32,
}

impl Default for DisplaySettings {
    /// Vsync off, uncapped: what `src/lib.rs` shipped with before this was a
    /// setting, so nobody's frame rate changes by upgrading.
    fn default() -> Self {
        Self {
            vsync: false,
            frame_cap: 0,
        }
    }
}

impl DisplaySettings {
    pub fn present_mode(&self) -> PresentMode {
        if self.vsync {
            PresentMode::AutoVsync
        } else {
            PresentMode::AutoNoVsync
        }
    }
}

/// Push vsync to the primary window when the setting changes. Bevy notices
/// the present-mode change and reconfigures the surface on the next frame.
fn apply_display_settings(
    settings: Res<DisplaySettings>,
    mut windows: Query<&mut Window, With<PrimaryWindow>>,
) {
    if !settings.is_changed() {
        return;
    }
    let mode = settings.present_mode();
    for mut window in &mut windows {
        if window.present_mode != mode {
            window.present_mode = mode;
        }
    }
}

/// Hold the main loop to `frame_cap` frames per second.
///
/// Measured from the end of the previous frame rather than to a fixed
/// schedule, so a slow frame doesn't trigger a burst of fast ones trying to
/// catch up.
#[cfg(not(target_arch = "wasm32"))]
fn limit_frame_rate(settings: Res<DisplaySettings>, mut last: Local<Option<Instant>>) {
    if settings.frame_cap == 0 {
        *last = None;
        return;
    }
    let period = Duration::from_secs_f64(1.0 / settings.frame_cap.clamp(1, FRAME_CAP_MAX) as f64);
    if let Some(previous) = *last {
        wait_until(previous + period);
    }
    *last = Some(Instant::now());
}

/// Sleep most of the way to `deadline`, then spin the rest. `thread::sleep`
/// overshoots by up to a millisecond or so on macOS, which at a 120 fps cap
/// would be an eighth of the frame.
#[cfg(not(target_arch = "wasm32"))]
fn wait_until(deadline: Instant) {
    const SPIN: Duration = Duration::from_micros(1500);
    let now = Instant::now();
    if deadline <= now {
        return;
    }
    let remaining = deadline - now;
    if remaining > SPIN {
        std::thread::sleep(remaining - SPIN);
    }
    while Instant::now() < deadline {
        std::hint::spin_loop();
    }
}

// ---------------------------------------------------------------------------
// Where the frame goes
// ---------------------------------------------------------------------------

/// The render thread's last-frame timings, shared with the main world the
/// same way `GpuBakeSupported` is. Microseconds, so they fit an atomic.
#[derive(Resource, Clone, Default)]
struct RenderThreadTimings(Arc<RenderThreadInner>);

#[derive(Default)]
struct RenderThreadInner {
    total_us: AtomicU32,
    acquire_us: AtomicU32,
}

/// Render-world clock for the frame in flight.
#[derive(Resource, Default)]
struct RenderClock {
    start: Option<Instant>,
    acquire_start: Option<Instant>,
    acquire: Duration,
}

fn micros(d: Duration) -> u32 {
    d.as_micros().min(u32::MAX as u128) as u32
}

fn render_frame_begin(mut clock: ResMut<RenderClock>) {
    clock.start = Some(Instant::now());
    clock.acquire = Duration::ZERO;
}

fn acquire_begin(mut clock: ResMut<RenderClock>) {
    clock.acquire_start = Some(Instant::now());
}

fn acquire_end(mut clock: ResMut<RenderClock>) {
    if let Some(start) = clock.acquire_start.take() {
        clock.acquire = start.elapsed();
    }
}

fn render_frame_end(clock: Res<RenderClock>, shared: Res<RenderThreadTimings>) {
    let Some(start) = clock.start else {
        return;
    };
    shared
        .0
        .total_us
        .store(micros(start.elapsed()), Ordering::Relaxed);
    shared
        .0
        .acquire_us
        .store(micros(clock.acquire), Ordering::Relaxed);
}

#[derive(Resource, Default)]
struct MainClock(Option<Instant>);

fn main_frame_begin(mut clock: ResMut<MainClock>) {
    clock.0 = Some(Instant::now());
}

/// One frame, in milliseconds.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct FrameSample {
    /// Wall time since the previous frame.
    pub frame: f32,
    /// Main thread: game + editor schedules.
    pub main: f32,
    /// Render thread, excluding the swapchain wait.
    pub render_busy: f32,
    /// Render thread, blocked waiting for a swapchain image.
    pub render_wait: f32,
}

/// The last [`HISTORY`] frames' breakdowns, for the editor to summarise.
#[derive(Resource, Default)]
pub struct FrameBreakdown {
    samples: VecDeque<FrameSample>,
}

impl FrameBreakdown {
    fn push(&mut self, sample: FrameSample) {
        if self.samples.len() == HISTORY {
            self.samples.pop_front();
        }
        self.samples.push_back(sample);
    }

    /// Per-field medians over the history; `None` until a few frames exist.
    ///
    /// Medians field by field, not the median frame: each column is its own
    /// distribution, and a single spiky frame shouldn't decide all four.
    pub fn median(&self) -> Option<FrameSample> {
        if self.samples.len() < 8 {
            return None;
        }
        let column = |f: fn(&FrameSample) -> f32| {
            let mut v: Vec<f32> = self.samples.iter().map(f).collect();
            v.sort_by(f32::total_cmp);
            v[v.len() / 2]
        };
        Some(FrameSample {
            frame: column(|s| s.frame),
            main: column(|s| s.main),
            render_busy: column(|s| s.render_busy),
            render_wait: column(|s| s.render_wait),
        })
    }
}

/// Close the main-thread clock and file this frame's sample. The render
/// thread's figures are from the frame it finished most recently, which with
/// pipelined rendering is the previous one — irrelevant over a 120-frame
/// median.
fn main_frame_end(
    clock: Res<MainClock>,
    time: Res<Time<Real>>,
    render: Res<RenderThreadTimings>,
    mut breakdown: ResMut<FrameBreakdown>,
) {
    let Some(start) = clock.0 else {
        return;
    };
    let ms = |us: u32| us as f32 / 1000.0;
    let total = ms(render.0.total_us.load(Ordering::Relaxed));
    let wait = ms(render.0.acquire_us.load(Ordering::Relaxed));
    breakdown.push(FrameSample {
        frame: time.delta_secs() * 1000.0,
        main: start.elapsed().as_secs_f32() * 1000.0,
        render_busy: (total - wait).max(0.0),
        render_wait: wait,
    });
}

/// What is holding the frame rate down, in a sentence.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameLimit {
    /// The render thread waits for most of the frame: display, GPU or
    /// compositor.
    Presentation,
    /// The main thread is busy for most of the frame.
    MainThread,
    /// The render thread is busy for most of the frame.
    RenderThread,
    /// Nothing is close to the frame time: a cap, or headroom.
    Headroom,
}

impl FrameSample {
    /// Classify the frame. A thread "dominates" when it accounts for most of
    /// the frame time; presentation is checked first because a long wait
    /// means the CPU numbers are bounded by it, not the other way round.
    pub fn limit(&self) -> FrameLimit {
        let frame = self.frame.max(1e-3);
        if self.render_wait > 0.4 * frame {
            FrameLimit::Presentation
        } else if self.main > 0.8 * frame {
            FrameLimit::MainThread
        } else if self.render_busy > 0.8 * frame {
            FrameLimit::RenderThread
        } else {
            FrameLimit::Headroom
        }
    }
}

/// If `frame_ms` is a whole multiple (≥ 2) of the display's refresh period,
/// return that multiple: the signature of vsync dropping frames because each
/// one takes a little longer than a refresh.
pub fn vsync_divisor(frame_ms: f32, refresh_hz: f32) -> Option<u32> {
    if refresh_hz <= 0.0 || frame_ms <= 0.0 {
        return None;
    }
    let period = 1000.0 / refresh_hz;
    let ratio = frame_ms / period;
    let k = ratio.round();
    (k >= 2.0 && (ratio - k).abs() < 0.06).then_some(k as u32)
}

// ---------------------------------------------------------------------------

pub struct FramePacingPlugin;

impl Plugin for FramePacingPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<DisplaySettings>()
            .init_resource::<FrameBreakdown>()
            .init_resource::<MainClock>();

        app.init_schedule(FrameStart);
        app.init_schedule(FrameEnd);
        {
            let mut order = app.world_mut().resource_mut::<MainScheduleOrder>();
            order.insert_before(First, FrameStart);
            order.insert_after(Last, FrameEnd);
        }

        app.add_systems(FrameStart, main_frame_begin);
        // Record first, *then* sleep, so the cap never reads as main-thread
        // work.
        #[cfg(not(target_arch = "wasm32"))]
        app.add_systems(FrameEnd, (main_frame_end, limit_frame_rate).chain());
        #[cfg(target_arch = "wasm32")]
        app.add_systems(FrameEnd, main_frame_end);
        app.add_systems(Update, apply_display_settings);

        let shared = RenderThreadTimings::default();
        app.insert_resource(shared.clone());

        let Some(render_app) = app.get_sub_app_mut(RenderApp) else {
            return;
        };
        render_app
            .insert_resource(shared)
            .init_resource::<RenderClock>()
            .add_systems(
                Render,
                (
                    render_frame_begin.in_set(RenderSystems::ExtractCommands),
                    acquire_begin
                        .in_set(RenderSystems::PrepareViews)
                        .before(prepare_windows),
                    acquire_end
                        .in_set(RenderSystems::PrepareViews)
                        .after(prepare_windows),
                    render_frame_end.in_set(RenderSystems::PostCleanup),
                ),
            );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(frame: f32, main: f32, render_busy: f32, render_wait: f32) -> FrameSample {
        FrameSample {
            frame,
            main,
            render_busy,
            render_wait,
        }
    }

    /// The situation the user reported: a 16.7 ms frame whose threads are
    /// both mostly idle. That is waiting, not working.
    #[test]
    fn a_mostly_waiting_frame_is_presentation_bound() {
        assert_eq!(
            sample(16.7, 3.0, 2.5, 11.0).limit(),
            FrameLimit::Presentation
        );
    }

    #[test]
    fn busy_threads_are_named() {
        assert_eq!(sample(16.7, 15.0, 2.0, 0.5).limit(), FrameLimit::MainThread);
        assert_eq!(
            sample(16.7, 3.0, 14.5, 0.5).limit(),
            FrameLimit::RenderThread
        );
        assert_eq!(sample(16.7, 3.0, 3.0, 0.5).limit(), FrameLimit::Headroom);
    }

    /// 120 Hz display, 16.7 ms frames: every other refresh is missed.
    #[test]
    fn vsync_halving_is_recognised() {
        assert_eq!(vsync_divisor(16.67, 120.0), Some(2));
        assert_eq!(vsync_divisor(25.0, 120.0), Some(3));
        assert_eq!(vsync_divisor(33.3, 60.0), Some(2));
        // At the refresh rate is not a dropped frame.
        assert_eq!(vsync_divisor(8.33, 120.0), None);
        // Nowhere near a multiple: not vsync.
        assert_eq!(vsync_divisor(12.0, 120.0), None);
        assert_eq!(vsync_divisor(16.7, 0.0), None);
    }

    #[test]
    fn medians_are_per_column_and_need_history() {
        let mut b = FrameBreakdown::default();
        for _ in 0..7 {
            b.push(sample(10.0, 1.0, 2.0, 3.0));
        }
        assert!(b.median().is_none(), "too few frames to summarise");
        b.push(sample(10.0, 1.0, 2.0, 3.0));
        // One wild frame must not move any median.
        b.push(sample(500.0, 400.0, 300.0, 200.0));
        assert_eq!(b.median(), Some(sample(10.0, 1.0, 2.0, 3.0)));
    }

    #[test]
    fn history_is_bounded() {
        let mut b = FrameBreakdown::default();
        for i in 0..(HISTORY * 3) {
            b.push(sample(i as f32, 0.0, 0.0, 0.0));
        }
        assert_eq!(b.samples.len(), HISTORY);
        assert_eq!(b.samples.front().unwrap().frame, (HISTORY * 2) as f32);
    }

    #[test]
    fn vsync_setting_maps_to_auto_modes() {
        let on = DisplaySettings {
            vsync: true,
            ..default()
        };
        assert_eq!(on.present_mode(), PresentMode::AutoVsync);
        assert_eq!(
            DisplaySettings::default().present_mode(),
            PresentMode::AutoNoVsync
        );
    }

    /// Lenient on purpose: this guards the logic (never early, never wildly
    /// late), not the OS scheduler.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn wait_until_reaches_its_deadline() {
        let deadline = Instant::now() + Duration::from_millis(5);
        wait_until(deadline);
        let after = Instant::now();
        assert!(after >= deadline, "returned early");
        assert!(
            after - deadline < Duration::from_millis(50),
            "overshot badly"
        );
        // A deadline in the past returns at once.
        let start = Instant::now();
        wait_until(start - Duration::from_millis(1));
        assert!(start.elapsed() < Duration::from_millis(5));
    }
}
