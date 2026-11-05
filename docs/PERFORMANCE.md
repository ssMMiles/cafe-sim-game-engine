# Performance: reading the frame

Inspector → nothing selected → **Performance**. Everything the editor can
tell you about what a frame costs, and the settings that pace it.

| Piece | Where |
|-------|-------|
| Vsync, frame cap, the thread breakdown | `src/game/gfx/frame_pacing.rs` |
| A/B measurement, GPU-timer capability | `src/game/gfx/profiling.rs` |
| Panel | `src/editor/ui/inspector.rs` (`performance` and below) |
| Persisted | `editor_settings.json` → `display` (vsync, cap) |

## 1. Frame time is not work time

The first thing to know, because it explains most confusing numbers: a
16.7 ms frame might be 3 ms of work and 13 ms of **waiting**. With vsync on,
the frame can't be shorter than the display's refresh. Even with vsync off,
a window on macOS is composited, and the compositor hands out drawables at
the display's pace.

Two consequences:

- **The frame-time number can't tell you what anything costs** while the
  frame is pinned to the display. A cost that fits inside the wait is
  invisible to it.
- **Vsync halves rather than degrades.** If each frame takes slightly longer
  than one refresh (more than 8.3 ms at 120 Hz), the swapchain misses every
  other refresh and you get exactly 60 fps, not 110. A frame rate that is an
  exact fraction of the display (120 → 60 → 40) is the signature.

So the panel measures work and waiting separately.

## 2. The thread breakdown

Bevy renders on two threads: the main thread runs the game and editor
systems for frame N+1 while the render thread draws frame N. Each gets a
bar the width of the frame:

- **Main thread**: busy time from `FrameStart` to `FrameEnd`. These are two
  schedules inserted around bevy's `First` and `Last` (`MainScheduleOrder`),
  so they bracket every game and editor system.
- **Render thread**: busy time, plus — in grey — time **waiting for a
  swapchain image**. That wait is `get_current_texture` inside bevy's
  `prepare_windows`, which bevy's own docs name as where vsync and a GPU
  bottleneck both show up. It is timed directly, by systems ordered just
  before and after `prepare_windows`.

The figures are per-field medians over the last 120 frames, so one spike
doesn't move them. A sentence underneath says what is limiting the frame:

| Reads | Means |
|-------|-------|
| Running at 1/k of the display's N Hz | each frame takes a little longer than one refresh, so vsync drops to a fraction. With both threads mostly idle, the GPU is the likely cause — or the display is actually running slower than it reports (Low Power Mode on a MacBook caps ProMotion at 60 Hz) |
| Waiting for the display | vsync on, frame done early. Idle, not cost |
| Waiting for a swapchain image with vsync off | GPU-bound, or the macOS compositor pacing a window |
| CPU-bound on the main thread | game + editor systems fill the frame |
| CPU-bound on the render thread | extraction and command building fill the frame |
| Held by the frame cap | the cap is the limit |

The thresholds (`FrameSample::limit`): presentation if the render thread
waits for more than 40% of the frame, else a thread if it is busy for more
than 80%. Presentation is checked first because a long wait bounds the CPU
numbers, not the other way round. `vsync_divisor` recognises a frame time
within 6% of a whole multiple (≥ 2) of the refresh period. Both are
unit-tested.

The display's refresh rate comes from bevy's `Monitor` (the primary one —
bevy can't say which monitor a window is on). It is whatever the OS
reports, which may not reflect a ProMotion panel being held at 60 Hz.

## 3. Vsync and the frame cap

- **Vsync** maps to `PresentMode::AutoVsync` (Fifo) or `AutoNoVsync`
  (Immediate on Metal, DX12 and Vulkan). Default **off**, which is what
  `src/lib.rs` hard-coded before this was a setting. Bevy reconfigures the
  surface when the window's present mode changes, so it takes effect on the
  next frame.
- **Frame cap** holds the main loop to a frame rate, measured from the end
  of the previous frame (a slow frame doesn't trigger a catch-up burst). It
  sleeps most of the way and spins the last 1.5 ms, because `thread::sleep`
  overshoots by about a millisecond on macOS. It runs in `FrameEnd`
  *after* the main thread's busy time is recorded, so the sleep never
  reads as work. Native only: blocking the browser's main thread isn't
  allowed, and browsers pace frames themselves.

**Turn vsync off before measuring anything.** With it on, every cost below
one refresh is idle time.

A trap the cap's UI avoids: the fps field is only built while the cap is
on. egui's `DragValue` clamps the *existing* value into its range every
frame, even when disabled, so drawing it over the 0 that means "off" would
silently turn it into a 1 fps cap.

## 4. Frame-time trace

The last ~120 frames (bevy's `FrameTimeDiagnosticsPlugin`) as a trace with
the median marked. A chart rather than a number because the shape is the
point: a steady 9 ms and a 6 ms average with a 40 ms spike every second read
identically as a mean and feel nothing alike.

## 5. A/B measurement

Pick an effect (god rays, fog, edge detection) and it is switched off and on
for a few hundred frames — 8 phases, 20 warm-up frames discarded then 80
measured each — and the frame times compared. Reports the median delta with
the interquartile spread of each phase, and says "no measurable cost" when
the delta is inside that noise.

This is the measurement that works on macOS. Know its limits:

- It measures **frame time**, so it only sees a cost while the frame isn't
  pinned to the display (§1). The panel warns when vsync is on. A window on
  macOS can be pinned even with vsync off; if the breakdown says the frame
  is mostly waiting, an A/B can't see GPU cost hidden in that wait.
- Hold the camera still and don't resize the viewport while it runs — both
  change the cost of the very thing being measured.
- In the editor the 3D view is only as big as the viewport tab, so a
  fullscreen effect measures smaller here than in gameplay.

It drives the effect through `EffectOverride`, read by the two uniform sync
systems, never through `AtmosphereStyle`, so a measurement can't touch your
authored settings or `editor_settings.json`. The phase machine is
unit-tested: it recovers an injected 1.25 ms cost to within 0.05 ms through
spiky synthetic frame times, discards the warm-up frames, and reports pure
noise as insignificant.

## 6. Per-pass GPU times

Each render pass with its GPU time in milliseconds, from bevy's
`RenderDiagnosticsPlugin` (bevy only adds it under `tracing-tracy`, so
`src/lib.rs` adds it explicitly; our passes are wrapped in `pass_span`s
named "edge detection" and "atmosphere").

**This list is empty on macOS, by design of the hardware.** Bevy writes its
timestamps *inside* the render pass, which needs
`TIMESTAMP_QUERY_INSIDE_PASSES`. Apple GPUs sample counters at stage
boundaries only, so Metal reports `TIMESTAMP_QUERY` (enough for wgpu to
create a query set) but not the inside-passes variant — and because a query
set exists, bevy's recorder never falls back to its CPU timings either.
Nothing is reported at all. `GpuTimingSupport` detects this and the panel
says so instead of showing an empty table.

On Windows and Linux (DX12, Vulkan) this is the precise answer. On macOS,
use the breakdown and the A/B, or take a GPU capture in Xcode for a real
per-pass breakdown. Doing it ourselves would mean our own query set written
through `RenderPassDescriptor::timestamp_writes` (stage-boundary sampling,
which Metal *does* support) plus a resolve and readback — not done.

## 7. Recipe: "my frame rate dropped"

1. Open Performance. Look at the sentence under the bars.
2. **"Running at 1/2 of the display's 120 Hz"**: frames take a little over
   8.3 ms. Turn vsync off. If the frame time drops to around 9–10 ms, that
   was it: there's a small amount of new cost, and vsync made it look like
   half the frame rate. If it stays at 16.7 ms with vsync off, check Low
   Power Mode and the display's refresh setting.
3. **Either thread CPU-bound**: the bars say which one.
4. With vsync off and the frame no longer pinned, A/B the effects one at a
   time to see which one pays for the difference.
