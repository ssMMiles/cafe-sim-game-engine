//! Bevy systems for saving terrain data to disk asynchronously.

use bevy::prelude::*;

use super::storage::{
    ChunkData, ChunkIndexEntry, ChunkState, HeightData, MaterialData, TerrainChunkRegistry,
    TerrainWorld, quantize_heights,
};
use super::{CHUNK_SIZE, HeightmapData};

/// Sent by the editor (Shift+S) to request a terrain save.
#[derive(Message)]
pub struct SaveTerrainMessage;

// ============================================================================
// Resources
// ============================================================================

struct SaveResult {
    saved_chunks: Vec<(i32, i32)>,
    world_meta_ok: bool,
}

/// Holds the in-flight background save task (at most one at a time).
///
/// On native (and wasm32 without the `opfs` feature): wraps a `Task<SaveResult>`
/// from `AsyncComputeTaskPool`.
/// On wasm32+opfs: wraps an `Arc<Mutex<Option<SaveResult>>>` polled from a
/// `spawn_local` future (OPFS futures are `!Send`).
#[cfg(not(all(target_arch = "wasm32", feature = "opfs")))]
#[derive(Resource, Default)]
pub struct PendingTerrainSave(Option<bevy::tasks::Task<SaveResult>>);

#[cfg(all(target_arch = "wasm32", feature = "opfs"))]
#[derive(Resource, Default)]
pub struct PendingTerrainSave(Option<std::sync::Arc<std::sync::Mutex<Option<SaveResult>>>>);

// ============================================================================
// Systems
// ============================================================================

/// On `SaveTerrainMessage`: drain dirty chunks, clone all data, and hand off to
/// a background task (native) or a `spawn_local` future (wasm+opfs).
/// The main thread is not blocked waiting for any I/O.
pub fn save_terrain(
    mut messages: MessageReader<SaveTerrainMessage>,
    terrain_world: Option<Res<TerrainWorld>>,
    registry: Option<ResMut<TerrainChunkRegistry>>,
    heightmap: Option<Res<HeightmapData>>,
    mut pending: ResMut<PendingTerrainSave>,
    #[cfg(all(target_arch = "wasm32", feature = "opfs"))] _storage: Res<crate::storage::Storage>,
) {
    if messages.read().count() == 0 {
        return;
    }

    if pending.0.is_some() {
        warn!("Terrain save already in progress — try again in a moment");
        return;
    }

    let (Some(terrain_world), Some(mut registry), Some(heightmap)) =
        (terrain_world, registry, heightmap)
    else {
        warn!("Cannot save terrain: world not loaded yet");
        return;
    };

    // Note: an empty dirty set is *not* an early return. `world.meta` carries
    // the level environment and per-plugin data, which can change without any
    // chunk being touched, so every save request rewrites it.
    let dirty: Vec<(i32, i32)> = registry.dirty_chunks.drain().collect();
    if dirty.is_empty() {
        info!("No terrain changes; writing world metadata only");
    } else {
        info!("Saving {} dirty chunk(s) (background)", dirty.len());
    }

    let chunks: Vec<ChunkData> = dirty
        .iter()
        .map(|&(cx, cy)| extract_chunk_data(&heightmap, cx, cy))
        .collect();
    let metadata_snapshot = terrain_world.metadata.clone();

    #[cfg(not(all(target_arch = "wasm32", feature = "opfs")))]
    {
        use super::storage::{save_chunk, save_world};
        use bevy::tasks::AsyncComputeTaskPool;

        let world_path = terrain_world.world_path.clone();

        let task = AsyncComputeTaskPool::get().spawn(async move {
            let mut saved_chunks = Vec::new();
            let mut meta = metadata_snapshot;

            for chunk in &chunks {
                let cx = chunk.chunk_x;
                let cy = chunk.chunk_y;
                match save_chunk(chunk, &world_path) {
                    Ok(()) => {
                        if let Some(entry) = meta
                            .chunk_index
                            .iter_mut()
                            .find(|e| e.chunk_x == cx && e.chunk_y == cy)
                        {
                            entry.state = ChunkState::FullyOverwritten;
                        } else {
                            meta.chunk_index.push(ChunkIndexEntry {
                                chunk_x: cx,
                                chunk_y: cy,
                                state: ChunkState::FullyOverwritten,
                            });
                        }
                        saved_chunks.push((cx, cy));
                    }
                    Err(e) => error!("Failed to save chunk ({cx}, {cy}): {e}"),
                }
            }

            meta.last_saved = 0;

            let world_meta_ok = match save_world(&meta, &world_path) {
                Ok(()) => true,
                Err(e) => {
                    error!("Failed to save world metadata: {e}");
                    false
                }
            };

            SaveResult {
                saved_chunks,
                world_meta_ok,
            }
        });

        pending.0 = Some(task);
    }

    #[cfg(all(target_arch = "wasm32", feature = "opfs"))]
    {
        use super::storage::{serialize_chunk, serialize_world};
        use std::sync::{Arc, Mutex};
        use wasm_bindgen_futures::spawn_local;

        let world_id = terrain_world.metadata.world_id.clone();

        // Serialize synchronously on the main thread (sub-ms per chunk), then fire-and-forget.
        let mut serialized: Vec<(i32, i32, Vec<u8>)> = Vec::new();
        let mut meta = metadata_snapshot;

        for chunk in &chunks {
            let cx = chunk.chunk_x;
            let cy = chunk.chunk_y;
            match serialize_chunk(chunk) {
                Ok(bytes) => {
                    if let Some(entry) = meta
                        .chunk_index
                        .iter_mut()
                        .find(|e| e.chunk_x == cx && e.chunk_y == cy)
                    {
                        entry.state = ChunkState::FullyOverwritten;
                    } else {
                        meta.chunk_index.push(ChunkIndexEntry {
                            chunk_x: cx,
                            chunk_y: cy,
                            state: ChunkState::FullyOverwritten,
                        });
                    }
                    serialized.push((cx, cy, bytes));
                }
                Err(e) => error!("Failed to serialize chunk ({cx}, {cy}): {e}"),
            }
        }

        meta.last_saved = 0;
        let meta_bytes = match serialize_world(&meta) {
            Ok(b) => Some(b),
            Err(e) => {
                error!("Failed to serialize world metadata: {e}");
                None
            }
        };

        // saved_chunks will be the successful chunk coords.
        let saved_chunk_coords: Vec<(i32, i32)> =
            serialized.iter().map(|&(cx, cy, _)| (cx, cy)).collect();

        let slot: Arc<Mutex<Option<SaveResult>>> = Arc::new(Mutex::new(None));
        pending.0 = Some(slot.clone());

        let s = crate::storage::wasm::WasmStorage;
        spawn_local(async move {
            let mut saved_chunks = Vec::new();

            for (cx, cy, bytes) in serialized {
                let key = format!("worlds/{}/chunks/chunk_{}_{}.terrain", world_id, cx, cy);
                if let Err(e) = s.save(&key, &bytes).await {
                    log::error!("OPFS terrain save failed for chunk ({cx}, {cy}): {e}");
                } else {
                    saved_chunks.push((cx, cy));
                }
            }

            let world_meta_ok = if let Some(bytes) = meta_bytes {
                let key = format!("worlds/{}/world.meta", world_id);
                match s.save(&key, &bytes).await {
                    Ok(()) => true,
                    Err(e) => {
                        log::error!("OPFS world.meta save failed: {e}");
                        false
                    }
                }
            } else {
                false
            };

            *slot.lock().unwrap() = Some(SaveResult {
                saved_chunks,
                world_meta_ok,
            });
        });
    }
}

/// Poll the background save task/slot each frame; once complete, apply results to
/// the live `TerrainWorld` resource.
pub fn poll_terrain_save(
    mut pending: ResMut<PendingTerrainSave>,
    terrain_world: Option<ResMut<TerrainWorld>>,
) {
    #[cfg(not(all(target_arch = "wasm32", feature = "opfs")))]
    {
        use bevy::tasks::futures::check_ready;

        let Some(task) = pending.0.as_mut() else {
            return;
        };
        let Some(result) = check_ready(task) else {
            return;
        };
        pending.0 = None;
        apply_save_result(result, terrain_world);
    }

    #[cfg(all(target_arch = "wasm32", feature = "opfs"))]
    {
        let Some(slot) = pending.0.as_ref() else {
            return;
        };
        let result = slot.lock().unwrap().take();
        let Some(result) = result else {
            return;
        };
        pending.0 = None;
        apply_save_result(result, terrain_world);
    }
}

fn apply_save_result(result: SaveResult, terrain_world: Option<ResMut<TerrainWorld>>) {
    let Some(mut terrain_world) = terrain_world else {
        return;
    };

    for (cx, cy) in &result.saved_chunks {
        let (cx, cy) = (*cx, *cy);
        terrain_world
            .chunk_state_map
            .insert((cx, cy), ChunkState::FullyOverwritten);
        if let Some(entry) = terrain_world
            .metadata
            .chunk_index
            .iter_mut()
            .find(|e| e.chunk_x == cx && e.chunk_y == cy)
        {
            entry.state = ChunkState::FullyOverwritten;
        } else {
            terrain_world.metadata.chunk_index.push(ChunkIndexEntry {
                chunk_x: cx,
                chunk_y: cy,
                state: ChunkState::FullyOverwritten,
            });
        }
    }

    if result.world_meta_ok {
        terrain_world.metadata.last_saved = 0;
        info!("Terrain saved ({} chunks)", result.saved_chunks.len());
    }
}

// ============================================================================
// Helpers
// ============================================================================

pub(super) fn extract_chunk_data(
    heightmap: &HeightmapData,
    chunk_x: i32,
    chunk_y: i32,
) -> ChunkData {
    let cs = (CHUNK_SIZE - 1) as i32;
    let start_x = chunk_x * cs;
    let start_y = chunk_y * cs;

    let width = CHUNK_SIZE as u32;
    let height = CHUNK_SIZE as u32;

    let mut chunk_heights = Vec::with_capacity((width * height) as usize);
    for lx in 0..width as i32 {
        for ly in 0..height as i32 {
            chunk_heights.push(heightmap.get_tile(start_x + lx, start_y + ly));
        }
    }

    let (quantized, height_min, height_max) = quantize_heights(&chunk_heights);

    ChunkData {
        chunk_x,
        chunk_y,
        width,
        height,
        height_data: HeightData::Full {
            heights: quantized,
            height_min,
            height_max,
        },
        material_data: MaterialData::Uniform,
    }
}
