# Profiling the web build

The wasm build is **single-threaded**. Everything the native build pushes to
the compute pool — chunk meshing, EXR and probe-grid decoding, OPFS reads,
probe-volume rebuilds — runs on the main thread between frames. So web stutter
is "which main-thread task was long", not "which system is slow on average",
and the tool for it is a Chrome performance trace with bevy's spans in it.

## Build and record

```
tools/web.sh profile          # web-dev profile + bevy/trace, own target dir
python3 tools/serve.py 8000
node tools/web-trace.mjs --url 'http://localhost:8000/index.html?world=/worlds/cafe-bundle.zip&play' \
     --out /tmp/run.jsonl  wait:40 click:640,418 wait:25 click:640,300 focus key:F1 \
     trace:start hold:KeyW:12 hold:KeyA:8 hold:KeyS:10 trace:stop
node tools/web-trace.mjs --analyse /tmp/run.jsonl        # re-read a saved run
```

`web-profile` turns on `bevy/trace`, which makes every system and schedule a
`tracing` span; on wasm bevy's log layer is `tracing-wasm`, which emits each
span as `performance.mark`/`performance.measure`, so they show on the
Timings track of Chrome's Performance panel. `tools/web-trace.mjs` drives a
fresh headless Chrome over the DevTools protocol (empty cache and OPFS each
run), records `toplevel` + `blink.user_timing`, and prints: task-length
histogram, spans by inclusive time, systems by worst single run, and the
longest tasks with the spans inside them. You can also just open the page in
Chrome and record in the Performance panel by hand.

**Run one harness at a time per machine.** The DevTools port is per-process
now, but two concurrent runs still share the CPU; and `pkill -f web-trace`
from one session kills the other's run.

Driving the engine from outside: the main menu is bevy_ui, so the harness
clicks by coordinate (`shot:` a screenshot to find them; at 1280×800 the
first project button is at 640,418). **Use the `focus` step before keys** —
winit listens on the canvas and a synthetic click does not reliably focus it;
without it the player never moves and the run measures a standing scene.
Check `shot:` before/after if in doubt.

## Reading the numbers

- **Instrumentation dominates average frame time.** A frame has ~2000 spans
  (avian's substep schedule alone is ~500 per physics step), each a
  mark+measure pair at ~10–20 µs in Chrome, so a traced frame carries tens
  of milliseconds of pure overhead and headless runs at ~20 fps. Read
  *hitches* (tasks far above the baseline) and *relative* costs, never
  absolute steady-state frame time, from a traced run. For steady state,
  record an untraced build and read the frame markers alone.
- Spans are inclusive: `schedule` totals exceed `update` because schedules
  nest.
- The harness filters `check_conditions`/`system_commands` spans and writes
  JSON Lines; a 30 s run is still ~5 M events and ~1 GB on disk.

## Measured 2026-11-03 (cafe world, walking, headless Chrome, M-series Mac)

| Finding | Measured |
|---------|----------|
| `update_probe_volume` rebuild | 47–101 ms per run, 7 runs in 9 s of walking (0.5 s cooldown while chunk probe grids stream in; also on every re-centre, every ~32 m). Natively `rebuild_cost` measures 17.8 ms single-threaded; wasm has no rayon. |
| Its GPU side | `extract_render_asset`/`prepare_assets<GpuImage>` 9–20 ms each: four 3D textures, 56.6 MB, re-uploaded whole because the `Image` asset changed. |
| Fixed-timestep catch-up | after any hitch `run_fixed_main_schedule` runs several 64 Hz steps in one frame (77–82 ms observed), each ~4–5 ms traced. Turns one hitch into a few. |
| One-off at level load | a 990 ms task with 743 ms inside `PreUpdate` before its first system — i.e. in command/observer flush, not a system. Not yet attributed; suspect scene-ready conversion (toon materials + per-triangle lightmap UV unwrap) of the cafe shell. |
| Standing still | no tasks above the fixed-timestep noise. |

## Fixed the same day: incremental probe volume

`world/probe_volume.rs` now follows the focus cell by cell and fills only
the slab each step exposes, refills just a chunk's cells when its grid
arrives, copies the stored `f16`s straight into texels, and hands regions to
the render world to `write_texture` into the existing GPU textures (the
`Image`s are render-world-only now). A per-frame cell budget bounds the
cost. `Time<Virtual>::set_max_delta(50 ms)` caps fixed-step catch-up.

Validation run (same script; **contaminated**: another session was running
its own headless Chrome and builds concurrently, load average 7, and the
world now had the cafe shell placed, so frame medians are not comparable
with the baseline — the per-system numbers are):

| System | Before (max) | After (max / p95 / mean) |
|--------|-------------:|-------------------------:|
| `update_probe_volume` | 101 ms | 4.7 / 0.9 / 0.2 ms |
| `write_probe_uploads` (render world) | — | 2.3 / 0.4 / 0.2 ms |
| `prepare_assets<GpuImage>` | 20 ms per rebuild | p95 0 (one 14 ms, first upload) |

Native `rebuild_cost`: full window 12.8 ms single-threaded (was 17.8 ms
*with* rayon), one chunk 1.4 ms, one-cell step 0.15 ms.

What the trace shows next, all level-load work rather than walking:

- **Time with no span at all.** The longest frames (600, 475, 327 ms) are
  mostly *outside* every bevy span. That is the OPFS chunk pipeline: the
  `spawn_local` futures that read a chunk's files and decode the 2.5 MB EXR +
  3.4 MB probe grid and mesh four LODs run as main-thread microtasks between
  bevy spans, invisible to this instrumentation. Add `info_span!`s around
  `compute_chunk_meshes`/the decoders before chasing it.
- `FixedMain` step of 106 ms — avian building the cafe shell's trimesh
  collider inside the physics schedule when the mesh lands.
- `apply_object_lightmaps` 71 ms, `extract_render_asset<RenderMesh>` 62 ms,
  one `egui` pass of 267 ms (first pass after the mode switch).
