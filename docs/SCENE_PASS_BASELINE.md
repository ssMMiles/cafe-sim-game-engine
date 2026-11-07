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

### 3.1 Before Phase 1 — editor, terrain world

> **Not yet taken.** `cargo run-profiling`, follow §1, paste the snapshot
> here. This is the row set Phase 1 is judged against.

### 3.2 Before Phase 1 — cafe, gameplay

> **Not yet taken.** The control: Phase 1 touches no toon material, so this
> table should be unchanged afterwards apart from noise.

### 3.3 After Phase 1

> **Not yet taken.**

## 4. Visual checklist for Phase 1

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
