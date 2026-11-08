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

### 3.6 After Phase 5 — expected, not yet measured

> **Not yet taken.** Take two gameplay snapshots on this build (F9). What the
> table should show: **every `bevy_pbr::` row gone** (clusters, lights, GPU
> preprocess, mesh/view bind groups, skins, previous-frame transforms —
> ~1.5 ms on `bc2b5fe`), every `write_binned_instance_buffers` /
> `write_indirect_parameters_buffers` / `write_batched_instance_buffers` row
> gone, the `bevy_gizmos_render` rows gone, `check_entities_needing_specialization<StandardMaterial>`
> gone. Appearing: `scene_pass::skin::extract_skins` + `prepare_skins` and the
> `scene_pass::gizmos::` rows (extract, prepare, `gizmo_pass`), which should be
> a few hundredths of a millisecond together. The `bevy_light::` rows stay but
> have nothing to do (no light components).
>
> Also check `runs` on `scene_prepass`: it should be **1.0** now that the
> thumbnail rig is parked on entering gameplay (`4a0e889`), and that alone
> halves every per-view cost in these tables.

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
