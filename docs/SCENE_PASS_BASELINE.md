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

Then, **in the editor** (F1 if it starts in gameplay), with nothing selected:
Inspector → **Performance**.

1. **Turn vsync off** (Performance → Display) and leave the frame cap off.
   With vsync on, anything cheaper than a refresh reads as idle and the whole
   table becomes meaningless — `docs/PERFORMANCE.md` has the long version.
2. Put the camera somewhere reproducible and **stand still for ~10 seconds**.
   The table is an exponential moving average over ~60 frames; the frame
   breakdown is a median over the last few hundred. Both need to settle.
3. Open **Systems (CPU)** and press **Save snapshot**. That writes
   `perf-snapshot-<unix-seconds>.md` into the working directory (and copies
   the same text to the clipboard). Paste the relevant part into a section
   below with the scene, the date and what build it was.

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
`total_ms` after**, plus the appeared/vanished lists. Frame time is *not* the
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

### 3.4 After Phase 3 — editor terrain world and cafe

> **Not yet taken.** Expect `MaterialPlugin::<ToonMaterial>`'s rows to go the
> same way, and — the one that might actually register — the two
> `write_binned_instance_buffers` and `prepare_preprocess_bind_groups` rows to
> fall to their straggler-only floor, since every toon sub-mesh left bevy's
> binned phases too.

## 4. Visual checklist for Phases 1 and 3

Measurement is half of it; the exit criterion is "visually unchanged". With a
terrain world loaded:

- [ ] Terrain renders at all, with its stone texture and toon bands.
- [ ] Baked lightmaps still land: Shift+B over a region, chunks update.
- [ ] Shift+L flips between baked and flat unlit on terrain.
- [ ] Outlines trace terrain silhouettes and creases (the prepass normals are
      now ours — chunk edges and skirts are where a mistake would show).
- [ ] Fog and god rays still affect terrain (they read the prepass depth,
      which is now written by our pass and copied by bevy's).
- [ ] Streaming: walk out and back, chunks load and unload with no flicker.
- [ ] LOD: the chunk ring changes mesh with distance and nothing pops to
      black.
- [ ] Draw count = loaded chunk count (one per chunk, each has its own
      lightmap and therefore its own material).
- [ ] Thumbnails in the Assets panel are unaffected (the rig is a `Camera3d`
      on layer 7, so it must see no terrain).
- [ ] Web build: `tools/web.sh`, same checks, no `[Invalid ShaderModule]`.

With objects placed (and the cafe, for the player + furniture):

- [ ] Placed objects render with their textures, toon bands and lightmaps.
- [ ] Double-sided materials (trees, rock, table2) show both faces with the
      normal flipped on the back — no spurious crease outlines along leaves.
- [ ] The player renders, animates (skinning) and is probe-lit; its blob
      shadow (a `StandardMaterial` straggler) still draws under it.
- [ ] Shift+L flips objects to unlit too.
- [ ] Shift+B object bakes land on the right sub-meshes.
- [ ] A `DynamicLit` toggle in the inspector swaps lightmap ↔ probe source.
- [ ] Cafe furniture: draw count stays ≈ number of distinct (material, mesh)
      pairs, not number of pieces.
- [ ] Placement ghost, brush disc and thumbnails (all still bevy's) unaffected.
