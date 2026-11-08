# Scene pass — measurements

The before-photo (and later the after-photos) for
`docs/SCENE_PASS_PLAN.md`. Kept here rather than in `memory.md`: these are
tables that get re-taken at every phase, and they would swamp it.

---

## 1. How to take a measurement

The numbers only exist in a build with bevy's span instrumentation, which is
`profiling` / `web-profile` only — the plain dev build shows a hint instead of
a table (`gfx/system_timing.rs`).

```
cargo run-profiling
```

Measure **in gameplay**, not the editor. The editor's own egui pass
(`editor_ui`, ~1.8 ms) was the biggest row in every early table and has
nothing to do with the renderer; editor optimisation is a separate job.

1. **Turn vsync off** (editor → Inspector, nothing selected → Performance →
   Display) and leave the frame cap off. The setting persists. With vsync on,
   anything cheaper than a refresh reads as idle and the whole table becomes
   meaningless — `docs/PERFORMANCE.md` has the long version.
2. **F1 into gameplay.** Put the player somewhere reproducible and **stand
   still for ~10 seconds**. The table is an exponential moving average over
   ~60 frames; the frame breakdown is a median over the last few hundred.
   Both need to settle.
3. **Press F9.** That writes `perf-snapshot-<unix-seconds>.md` into the
   working directory, with `(gameplay)` in its heading. (The editor panel's
   **Save snapshot** button does the same with `(editor)`, and also copies to
   the clipboard.) Paste the relevant part into a section below with the
   scene, the date and what build it was.

**Every snapshot records the commit it came from**, in its heading and its
file name (`perf-snapshot-<commit>-<secs>.md`, from `BUILD_COMMIT` in
`build.rs`). Uncommitted edits are not captured — the hash is HEAD's — so
commit before a measurement you intend to keep. This matters because more
than one checkout writes snapshots into its own working directory with the
same naming, and a parallel branch of this same work exists; before the
stamp existed, row names were the only way to tell two tables apart.

Two scenes, because they stress different halves:

| Scene | Why |
|---|---|
| **Editor, terrain world** | Terrain-dominated: the chunk ring is ~81 materials and ~81 draws, all one material type. This is the one Phase 1 moves. |
| **Cafe (`cafe-sim`), gameplay** | Object-dominated: many `ToonMaterial` sub-meshes, the skinned player, probe-lit furniture that batches. This is what Phase 3 moves — and it must *not* get worse in Phase 1. |

Record the loaded-chunk count and roughly what's on screen; a table taken at a
different camera position is not comparable to one that isn't.

## 2. The rows that should move

Phase 1 removes `MaterialPlugin::<TerrainMaterial>` and everything bevy_pbr
did for a terrain chunk. Rows to watch, by the suffix of the system path —
the `::<…TerrainMaterial>` generic instantiations are the ones that should
**disappear entirely**:

- `bevy_pbr::material::extract_mesh_materials::<TerrainMaterial>`
- `bevy_pbr::material::early_sweep_material_instances::<TerrainMaterial>`
- `bevy_pbr::material::extract_entities_needs_specialization::<TerrainMaterial>`
- `bevy_pbr::material::check_entities_needing_specialization::<TerrainMaterial>`
- `bevy_pbr::material::mark_meshes_as_changed_if_their_materials_changed::<TerrainMaterial>`
- `bevy_render::render_asset::extract_render_asset::<MeshMaterial3d<TerrainMaterial>>`
  and its `prepare_assets::<…>` counterpart

Rows that should **shrink** (they stay, but with terrain's share of the work
gone — they are shared with `ToonMaterial` and the stragglers):

- `bevy_pbr::material::specialize_material_meshes`
- `bevy_pbr::material::queue_material_meshes`
- `bevy_pbr::prepass::queue_prepass_material_meshes`
- `bevy_render::batching::gpu_preprocessing::batch_and_prepare_binned_render_phase`
- `bevy_pbr::render::mesh::extract_meshes_for_gpu_building`
- `bevy_pbr::render::mesh::prepare_preprocess_bind_groups` / `write_mesh_view_bind_groups`

Rows that should **appear**, and whose total is the honest cost of the
replacement:

- `scene_pass::terrain::extract_terrain_instances`
- `scene_pass::terrain::queue_terrain_draws`
- `scene_pass::clear_scene_draws`
- `scene_pass::prepare_scene_instances`
- `scene_pass::prepare_scene_bind_group`
- `render_asset::prepare_assets::<PreparedTerrainMaterial>`

The honest summary for each phase is one line: **`total_ms` before →
`total_ms` after**, plus the appeared/vanished lists. Tables taken before
2026-11-07 evening carry two rows that are not renderer cost and should be
subtracted when comparing against newer ones: `editor_ui` (~1.8 ms, editor
only) and `collect_system_timings` (~1.2 ms, the profiler itself — rewritten
hash-free the same day, expect it well under 0.1 ms). Frame time is *not* the
headline — with vsync off on macOS the compositor still paces the drawable, so
read work time, not frame time.

GPU per-pass timings (the panel's last section) stay empty on macOS: Metal has
no `TIMESTAMP_QUERY_INSIDE_PASSES`. On a machine where they work, the
`scene prepass` and `scene opaque` spans are named and will show up there.

---

## 3. Baselines

### 3.1 Before Phase 1 — editor, terrain world (taken 2026-11-07, Phase 0 build)

Six snapshots, standing still, vsync off. Representative one
(`perf-snapshot-1794093637`): frame 10.0 ms · main 9.2 · render 5.9 busy +
0.1 waiting · 8.97 ms of system work.

| ms | system |
|---:|---|
| 1.97 | `editor::ui::editor_ui` |
| 1.27 | `system_timing::collect_system_timings` (the measurement itself) |
| 0.21 + 0.20 | `write_binned_instance_buffers<Opaque3d>` / `<Opaque3dPrepass>` |
| 0.16 | `gpu_preprocess::prepare_preprocess_bind_groups` |
| ~0.07 | **all ten `…<TerrainMaterial>` rows together** (extract, sweep, specialization checks, erased render asset, `Assets` bookkeeping) |
| ~0.03 | `specialize_material_meshes` + `queue_material_meshes` + `queue_prepass_material_meshes` |

So the ceiling on what Phase 1 could remove was ~0.1–0.3 ms of a ~9 ms frame.
The two biggest rows are the editor's own egui pass and the profiler; nothing
in bevy_pbr is near them at this scene size.

### 3.2 Before Phase 1 — cafe, gameplay

> **Not yet taken.**

### 3.3 After Phase 1 — editor, terrain world (taken 2026-11-07)

Representative (`perf-snapshot-1794097050`): frame 9.8 ms · main 8.9 ·
render 5.4 busy + 0.1 waiting · 8.45 ms of system work.

- **Vanished**: all eight `MaterialPlugin::<TerrainMaterial>` rows. The two
  `Assets<TerrainMaterial>` bookkeeping rows remain (they're bevy_asset's, and
  the asset type still exists).
- **Appeared**: nine `scene_pass` rows, **0.09 ms together** —
  `scene_prepass` 0.019, `prepare_scene_instances` 0.015, `scene_opaque`
  0.011, `queue_terrain_draws` 0.011, `extract_render_asset<PreparedTerrainMaterial>`
  0.011, `prepare_scene_bind_group` 0.008, `extract_terrain_instances` 0.006,
  `prepare_assets<…>` 0.005, `clear_scene_draws` 0.003.
- **Shrank**: `write_binned_instance_buffers` 0.41 → 0.27 ms (both),
  `prepare_preprocess_bind_groups` 0.16 → 0.11, `early_prepass` 0.05 → 0.04.

Net ≈ **−0.15 ms** per frame, inside the run-to-run noise of the totals
(8.3–9.4 ms across the six Phase 0 snapshots). As the plan said: the win is
ownership, not milliseconds, at this scene size.

### 3.4 After Phases 1–3 — editor and gameplay (taken 2026-11-07, build `19225e1`)

The first measurement with the editor out of the way (F9 in gameplay) and the
profiler's own cost fixed.

| | Editor | Gameplay |
|---|---:|---:|
| Frame | 8.50 ms | 7.64 ms |
| Main thread | 7.63 | 5.61 |
| Render busy + waiting | 5.19 + 0.11 | 5.70 + 0.76 |
| System work total | 7.37 | 6.80 |
| `editor_ui` | 1.69 | — |
| `collect_system_timings` | 0.19 | 0.28 |
| **All `scene_pass` rows** | **~0.21** | **~0.27** |

What the whole renderer-ownership change costs on CPU is that last row:
roughly a quarter of a millisecond, against a 7.6 ms frame. The two rows that
used to dominate are gone or shrunk — `editor_ui` by measuring in gameplay,
`collect_system_timings` from 1.2 ms by the hash-free rewrite (commit
`19225e1`).

**The interesting leftover.** With none of our geometry in bevy's phases any
more, bevy_pbr still costs about **0.5 ms** in gameplay for the handful of
`StandardMaterial` stragglers:

| ms | row | what it is for us |
|---:|---|---|
| 0.236 | `prepare_clusters_for_gpu_clustering` | clustered lighting — and *all* our lighting is baked |
| 0.098 | `prepare_preprocess_bind_groups` | GPU mesh preprocessing |
| ~0.15 | `write_binned_instance_buffers<…>` × 8 | one instantiation per phase, mostly empty |

That is about twice what our own pass costs, which is the first real evidence
for Phase 5 (or at least for killing the `BakedSun` directional light that
drives the clustering row).

### 3.5 Does a wider view cost more?

Two gameplay-era snapshots from the same build, low angle vs a high angle
seeing much more terrain and more objects:

| ms | Low | High |
|---|---:|---:|
| `scene_prepass` | 0.021 | 0.038 |
| `scene_opaque` | 0.012 | 0.023 |
| toon queue | 0.012 | 0.029 |
| **All `scene_pass`** | **~0.09** | **~0.16** |
| Render thread busy | 5.07 | 5.11 |

Roughly double the visible draw count costs ~0.07 ms of CPU, and the render
thread's total barely moves — the per-draw CPU cost is not where the frame
goes. What the wider view costs on the *GPU* is not visible here: macOS has no
in-pass timestamps (`docs/PERFORMANCE.md`), and most of the 5 ms of render-thread
work has no span of its own.

### 3.6 After Phase 5 — gameplay (taken 2026-11-08, build `f6fbe43`, vs `bc2b5fe` the same day)

Six snapshots on `f6fbe43`, two on `bc2b5fe` immediately before, same scene.

| | `bc2b5fe` (before) | `f6fbe43` (after) |
|---|---:|---:|
| Frame | 8.51–9.00 ms | 7.15–7.61 ms |
| Main thread | 6.41–6.47 | 4.01–4.48 |
| Render **busy** | 7.10–7.52 | **2.93–3.15** |
| Render waiting (swapchain acquire) | 0.13 | **1.1–3.9** |
| System work total | 7.1–7.4 | 6.3–7.5 † |

† inflated — see the wait note below.

Everything the prediction above listed happened: **every working `bevy_pbr::`
row is gone** (only the two `Assets<StandardMaterial>` bookkeeping rows
remain, and the asset type is kept deliberately), all
`write_binned_instance_buffers` / indirect-parameters rows gone, all
`bevy_gizmos_render` rows gone. Appeared: `scene_pass::skin::prepare_skins`
(~0.03) + `extract_skins` (0.004) and the `scene_pass::gizmos::` rows (~0.02
together). All `scene_pass` rows total **~0.3 ms**. `runs` on `scene_prepass`
is **1.0** — the parked thumbnail rig (`4a0e889`) halved the per-view costs as
hoped.

Net: **render-thread CPU work more than halved (−4.3 ms), main thread −2.2 ms.**

**The new big "wait" is not work.** With the render thread finishing its CPU
work in ~3 ms, it now arrives at `get_current_texture` long before a drawable
is free, so the acquire wait balloons from 0.1 to 1–4 ms — the slack between
our pace and whatever paces presentation (the macOS compositor, or the GPU
finishing the previous frame). Two traps when reading these tables:

- `prepare_windows`' self-time row (2.5–3.7 ms, now the top row by far)
  **contains** that same acquire wait — it is the span the
  `frame_pacing` brackets sit around. The same idle time therefore appears in
  both the "waiting" header *and* the system table, and inflates "system work
  total" by ~3 ms. Honest spanned CPU work on this build is ~3.5 ms/frame.
- The ~7.3 ms frame floor is **presentation- or GPU-bound, not CPU-bound**:
  main is 4.2, render busy 3.0. No CPU row will show where it goes, and macOS
  has no in-pass GPU timestamps. To find out which: A/B the post passes with
  `EffectBench` (`docs/PERFORMANCE.md`) and/or shrink the window — a
  GPU-bound wait scales with resolution, compositor pacing doesn't.

With `prepare_windows` subtracted, no single hot row is left: the biggest
real rows are `apply_extract_commands` (~0.15) and the profiler itself
(~0.13–0.25); the rest is a long tail of hundreds of ≤0.09 ms systems, much
of it machinery nothing uses (mesh2d/tilemap/sprite-mesh materials, OIT,
SMAA/TAA/CAS pipeline prep, skybox, deferred, depth pyramids). Further CPU
wins are plugin-level trims and schedule overhead, not any one system.

### 3.7 After folding the prepass into the colour pass — not yet measured

> Done 2026-11-08, on top of `f6fbe43`. The scene used to be rasterised
> twice — `scene_prepass` (depth + normals) then `scene_opaque` (colour, depth
> test `GreaterEqual` against it). Now `scene_pass` does colour, depth and the
> normal target in one pass, before bevy's `early_prepass` (which still copies
> the depth for the edge detector and the atmosphere). On a tile-based GPU
> (every Apple GPU) hidden-surface removal already shades nothing occluded,
> so the prepass bought nothing there and cost a full vertex + raster pass of
> every chunk and object, plus the MSAA normal/depth stores.
>
> What to look at, in gameplay with vsync off: **the frame time and the
> acquire wait**, not the system table — §3.6 established the frame is
> GPU-bound at ~7.3 ms, which no CPU row shows. Expect `scene_prepass` gone
> and `scene_pass` in its place at about what `scene_opaque` cost; the win,
> if there is one, is a lower frame floor. The visual checklist in §4 applies
> again (outlines on double-sided leaves, fog not touching the sky, blob
> shadows outline-free, thumbnails still rendering — their cameras have no
> `NormalPrepass` and use the one-target variant).
>
> Next GPU suspects, in order: the camera's default **`Msaa::Sample4`** on a
> 1920×1080 HDR target (the post passes read the prepass textures as
> multisampled, so turning it off needs their bind group layouts changed
> too); bevy's **empty passes still open render passes** — `early_prepass`
> and `main_opaque_pass_3d` begin a pass and load+store every multisampled
> attachment even with nothing to draw (their `is_empty` checks only guard
> the draw calls); and the five full-screen post/blit passes.

**Measured on the folded build (same `f6fbe43` stamp, four clean gameplay
snapshots):** frame 7.73–8.05, main ~4.1–4.5, render busy 2.77–2.90, wait
4.26–4.32. Render busy *down* ~0.2 ms, **frame time unchanged, the wait
absorbed everything** — the removed GPU work did not move the frame floor,
which means our geometry cost was not the pacer. What paces the ~7.8 ms frame
is still either the macOS compositor's drawable pacing or GPU work that
didn't change (the post passes, the MSAA load/store/resolve traffic, egui).
The CPU tables cannot distinguish these; an `xctrace` Metal System Trace can
(per-encoder GPU occupancy + the display track), and that is the next
measurement.

**Empty bevy passes suppressed the same day:** `suppress_bevy_core3d_phases`
(scene_pass/mod.rs) removes each drawn view's entries from the five 3D phase
maps in `PrepareBindGroups` — after `prepare_prepass_textures` (which keys
texture creation on those maps), before the camera driver — so `early_prepass`
and `main_opaque_pass_3d` hit their absent-phase early return instead of
opening and resolving a full-size MSAA pass each. That early return also
skipped bevy's depth copy, so `scene_pass` now makes the
`ViewPrepassTextures.depth` copy itself, same encoder position. Views with no
scene draws (a thumbnail rig while its subject loads) keep their phases so
bevy still clears their targets.

## 4. Visual checklist

Phase 5 (nothing on bevy_pbr):

- [ ] Editor: the transform gizmo, selection boxes, light markers (F8) and
      collision view (Shift+P) all draw, in front of geometry as before
      (`EditorGizmoGroup`'s `depth_bias: -1`).
- [ ] The placement ghost is a translucent grey silhouette; the brush disc a
      translucent cyan disc; blob shadows under the player and furniture.
- [ ] The player still animates (skinning is ours now).
- [ ] Assets-panel thumbnails render — flat, in their own colours/textures.
- [ ] cafe-sim: the shop's piece thumbnails render and then their cameras
      switch off (no `runs: 2.0+` in a gameplay snapshot after a minute);
      the placement ghost is a translucent tinted silhouette.
- [ ] The sun debug cone is flat yellow.
- [ ] Startup log: no "PbrPlugin was not detected" warning (the gizmo render
      plugin is disabled, not just unsatisfied).
