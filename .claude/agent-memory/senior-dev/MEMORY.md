# Senior Dev Agent Memory

## Infinite Terrain / HeightmapData (post-refactor, March 2026)
- `HeightmapData` is now a **sparse `HashMap<(i32, i32), f32>`** (no `grid_w`/`grid_h`/`heights: Vec`)
- Public API: `get_tile(x: i32, y: i32) -> f32` (returns 0.0 if absent), `set_tile(x, y, value)`, `sample_height(wx, wz) -> Option<f32>` (always Some), `sample_normal(wx, wz) -> Option<(Vec3, f32)>` (always Some), `remove_chunk_tiles(cx, cy)` for unload eviction
- `HeightmapData::new(tile_size: (f32, f32))` is the constructor; inserted in `bootstrap.rs::create_terrain`
- `generate_lod_mesh` signature: `(get_height: impl Fn(i32,i32)->f32, start_x: i32, start_y: i32, chunk_width, chunk_height, tile_size, lod_level)` — no more `heights: &[f32]` or `height_scale`
- Chunk coords are `i32` everywhere: `TerrainChunk.chunk_x/y`, `TerrainChunkRegistry` keys, `ChunkData.chunk_x/y`, `ChunkIndexEntry.chunk_x/y`, `PlacedObject.chunk`, `PlacedObjectRegistry.dirty_chunks`, `StreamingState.loaded/pending`
- `world_pos_to_chunk` now returns `(i32, i32)` using `.floor() as i32` (no `.max(0.0)` clamp)
- Format versions bumped: terrain `FORMAT_VERSION = 2`, objects `CHUNK_OBJECTS_VERSION = 2`
- `physics.rs`: removed old out-of-bounds clamp; `sample_height` always returns Some now

## Key File Locations
- `src/game/world/terrain/types.rs` — `HeightmapData`, `TerrainChunk`
- `src/game/world/terrain/mesh.rs` — `generate_lod_mesh`, `LOD_LEVELS`
- `src/game/world/terrain/bootstrap.rs` — `create_terrain`, `spawn_terrain_chunk`
- `src/game/world/terrain/storage.rs` — rkyv data types, `TerrainChunkRegistry`, file I/O
- `src/game/world/terrain/save.rs` — `save_terrain`, `extract_chunk_data`
- `src/game/world/terrain/edit.rs` — brush systems; `toggle_terrain_edit_mode`, `apply_terrain_brush`
- `src/game/world/streaming.rs` — `update_streaming_ring`, `process_pending_chunks`
- `src/game/world/objects.rs` — `PlacedObject`, `world_pos_to_chunk`, `spawn_chunk_objects`

## Doctest Gotcha
- `//! ``` ... ` code blocks in module-level doc comments are treated as Rust doctests
- Use `//! ```text` for non-Rust content (file trees, pseudocode, etc.)
