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

## Doctest Gotcha
- `//! ``` ... ` code blocks in module-level doc comments are treated as Rust doctests
- Use `//! ```text` for non-Rust content (file trees, pseudocode, etc.)
