# Senior Dev Agent Memory

## Infinite Terrain / HeightmapData (post-refactor, March 2026)
- `HeightmapData` is now a **sparse `HashMap<(i32, i32), f32>`** (no `grid_w`/`grid_h`/`heights: Vec`)
- Public API: `get_tile(x: i32, y: i32) -> f32` (returns 0.0 if absent), `set_tile(x, y, value)`, `sample_height(wx, wz) -> Option<f32>` (always Some), `sample_normal(wx, wz) -> Option<(Vec3, f32)>` (always Some), `remove_chunk_tiles(cx, cy)` for unload eviction
- `HeightmapData::new(tile_size: (f32, f32))` is the constructor; inserted in `bootstrap.rs::create_terrain`
- `generate_lod_mesh` signature: `(get_height: impl Fn(i32,i32)->f32, start_x: i32, start_y: i32, chunk_width, chunk_height, tile_size, lod_level)` — no more `heights: &[f32]` or `height_scale`
- Chunk coords are `i32` everywhere; `StreamingState` now has `loaded/pending/computing` fields
- `world_pos_to_chunk` returns `(i32, i32)` using `.floor() as i32`
- Format versions bumped: terrain `FORMAT_VERSION = 2`, objects `CHUNK_OBJECTS_VERSION = 2`

## Key File Locations
- `src/game/world/terrain/types.rs` — `HeightmapData`, `TerrainChunk`
- `src/game/world/terrain/mesh.rs` — `generate_lod_mesh`, `LOD_LEVELS`
- `src/game/world/terrain/bootstrap.rs` — `create_terrain`, `spawn_chunk_from_result`, `compute_chunk_meshes`, `extract_chunk_heights`, `apply_chunk_to_heightmap`
- `src/game/world/terrain/storage.rs` — rkyv data types, `TerrainChunkRegistry`, file I/O
- `src/game/world/terrain/save.rs` — `save_terrain`, `extract_chunk_data`
- `src/game/world/terrain/edit.rs` — brush systems; `toggle_terrain_edit_mode`, `apply_terrain_brush`
- `src/game/world/streaming.rs` — `update_streaming_ring`, `launch_chunk_tasks`, `finalize_computed_chunks`
- `src/game/world/objects.rs` — `PlacedObject`, `world_pos_to_chunk`, `spawn_chunk_objects`
- `src/game/world/baking/` — lightmap baking pipeline (see Baking System section)

## Async Chunk Mesh Generation (March 2026)
- `spawn_terrain_chunk` was replaced with a 3-stage async pipeline (see streaming.rs)
- `launch_chunk_tasks`: patches heightmap on main thread, extracts heights, spawns `AsyncComputeTaskPool` task
- `finalize_computed_chunks`: polls `task.is_finished()`, then `check_ready(&mut task)` to get result; builds collider + spawns entity on main thread
- `ChunkComputeResult { chunk_x, chunk_y, lod_meshes: [Mesh; LOD_LEVELS], aabb: Aabb, hf_heights }` — all Send
- `apply_chunk_to_heightmap` is `pub(crate)` (not `pub(super)`) so streaming.rs can access it

## Bevy AsyncComputeTaskPool Patterns (Bevy 0.18)
- Import: `bevy::tasks::{AsyncComputeTaskPool, Task, futures::check_ready}`
- Correct poll pattern: `task.is_finished()` to check, then `check_ready(&mut task)` to consume result
- DO NOT use `future::block_on(poll_once(&mut task))` — expensive and leaves broken Task that panics if re-polled
- `bevy::tasks::futures` module has `check_ready` and `now_or_never`; `poll_once` is at crate root from `futures_lite`
- Tasks are cancelled automatically when dropped

## OPFS Feature Flag Pattern (March 2026)
- Feature flag: `opfs = []` in Cargo.toml `[features]`
- Active condition: `#[cfg(all(target_arch = "wasm32", feature = "opfs"))]`
- `WasmStorage` is `Clone + Copy` (zero-cost unit struct) — pass into closures directly with `let s = crate::storage::wasm::WasmStorage;`
- OPFS futures are `!Send` — use `wasm_bindgen_futures::spawn_local` (NOT `AsyncComputeTaskPool`)
- Slot pattern for async results: `Arc<Mutex<Option<T>>>` — poll in Update, write inside `spawn_local`
- Slot poll: `slot.lock().unwrap().take()` — returns `Option<T>`, `None` = still pending
- All CPU work (serialization/parsing) goes inside the `spawn_local` closure to avoid blocking main thread
- Key format for OPFS: `"worlds/{world_id}/chunks/chunk_{cx}_{cy}.terrain"` / `.objects"`, `"worlds/{world_id}/world.meta"`
- Serialization helpers: `serialize_world`, `serialize_chunk` in `terrain/storage.rs`; `serialize_chunk_objects` in `objects.rs`
- Terrain bootstrap OPFS slot type: `Arc<Mutex<Option<Result<Option<Vec<u8>>, String>>>>`
- Chunk load slot types: `TerrainSlot = Arc<Mutex<Option<Option<ChunkData>>>>`, `ObjectsSlot = Arc<Mutex<Option<Option<ChunkObjectsFile>>>>`
- `PendingChunk` and `ComputingChunk` are fully cfg-split between native (handles) and wasm+opfs (slots/data)
- `wasm_bindgen_futures` is a `[target.'cfg(target_arch = "wasm32")'.dependencies]` — use `use wasm_bindgen_futures::spawn_local;` inside cfg blocks only

## Doctest Gotcha
- `//! ``` ... ` code blocks in module-level doc comments are treated as Rust doctests
- Use `//! ```text` for non-Rust content (file trees, pseudocode, etc.)

## Bevy 0.18 API Notes
- `RenderAssetUsages` is at `bevy::asset::RenderAssetUsages` (NOT `bevy::render::render_asset::RenderAssetUsages` — that is a private re-import)
- `GlobalTransform::to_matrix()` returns `Mat4` (NOT `compute_matrix()`)
- `Children` iterates as `&Entity` (slice::Iter) — use `.to_vec()` or `for &child in children.iter()` to get owned Entity
- Events use `#[derive(Message)]` + `MessageWriter<T>` / `MessageReader<T>` + `app.add_message::<T>()`
- `EventWriter`/`EventReader`/`add_event` do NOT exist in this project's Bevy 0.18 setup
- `.after()` cannot be called directly on free function items — use `.chain()` or configure in system sets

## Baking System (March 2026)
- Module: `src/game/world/baking/` with `mod.rs`, `raycast.rs`, `terrain.rs`, `objects.rs`, `uv_unwrap.rs`
- Trigger: Shift+B in edit mode → `BakeMessage` → `run_bake` system (`.chain()` after `trigger_bake`)
- `BakeConfig` resource: `terrain_resolution`, `mesh_resolution`, `ao_rays`, `ao_radius`, `shadow_steps`
- `LightmapUvCache(HashSet<AssetId<Mesh>>)` resource: tracks which meshes have had xatlas UV2 generated
- Terrain lightmaps: R8Unorm PNG, per-chunk, `worlds/{id}/chunks/chunk_{cx}_{cy}_lightmap.png`
- Object lightmaps: R8Unorm PNG, per placed-object id, `worlds/{id}/objects/obj_{id}_lightmap.png`
- Combined value per texel: `shadow * 0.5 + ao * 0.5`
- New direct dependencies: `xatlas-rs = "0.1.3"`, `bytemuck`

## Lightmap Shader Bindings (March 2026)
- `TerrainMaterial`: stone_texture at 100/101, lightmap at 102/103
- `ToonMaterial`: lightmap at 100/101 (previously the struct had no bindings and was `Default`)
- Default lightmaps: `DefaultLightmap` resource (terrain) / `DefaultToonLightmap` resource (toon)
  - Both are 1x1 white R8Unorm — created at startup, no-op until baked data is applied
  - `DefaultLightmap` inserted by `create_terrain` in bootstrap.rs (Stage 2)
  - `DefaultToonLightmap` inserted by `setup_default_toon_lightmap` Startup system in gfx/mod.rs
- Terrain UV2: planar XZ projection [0,1] per chunk stored in `ATTRIBUTE_UV_1` (location 3)
- `terrain.wgsl` vertex shader: manually reads `@location(3) uv_b`, writes `out.uv_b`
- Both fragment shaders: `lm = textureSample(lightmap, sampler, in.uv_b).r` then `toon_band(lighting * lm, 5.0)`
