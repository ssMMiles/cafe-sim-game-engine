//! Bevy systems for saving terrain data to disk asynchronously.

use bevy::{
    prelude::*,
    tasks::{AsyncComputeTaskPool, Task, futures::check_ready},
};

use super::storage::{
    ChunkData, ChunkIndexEntry, ChunkState, HeightData, MaterialData, TerrainChunkRegistry,
    TerrainWorld, quantize_heights, save_chunk, save_world,
};
use super::{CHUNK_SIZE, HeightmapData};

// ============================================================================
// Resources
// ============================================================================

struct SaveResult {
    saved_chunks: Vec<(i32, i32)>,
    world_meta_ok: bool,
}

/// Holds the in-flight background save task (at most one at a time).
#[derive(Resource, Default)]
pub struct PendingTerrainSave(Option<Task<SaveResult>>);

// ============================================================================
// Systems
// ============================================================================

/// On Shift+S: drain dirty chunks, clone all data, and hand off to a background
/// task.  The main thread is not blocked waiting for any I/O.
pub fn save_terrain(
    input: Res<ButtonInput<KeyCode>>,
    terrain_world: Option<Res<TerrainWorld>>,
    registry: Option<ResMut<TerrainChunkRegistry>>,
    heightmap: Option<Res<HeightmapData>>,
    mut pending: ResMut<PendingTerrainSave>,
) {
    if !(input.just_pressed(KeyCode::KeyS)
        && (input.pressed(KeyCode::ShiftLeft) || input.pressed(KeyCode::ShiftRight)))
    {
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

    if registry.dirty_chunks.is_empty() {
        info!("No terrain changes to save");
        return;
    }

    let dirty: Vec<(i32, i32)> = registry.dirty_chunks.drain().collect();
    info!(
        "Saving {} dirty chunk(s) to {:?} (background)",
        dirty.len(),
        terrain_world.world_path
    );

    let chunks: Vec<ChunkData> = dirty
        .iter()
        .map(|&(cx, cy)| extract_chunk_data(&heightmap, cx, cy))
        .collect();
    let metadata_snapshot = terrain_world.metadata.clone();
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

/// Poll the background save task each frame; once complete, apply results to
/// the live `TerrainWorld` resource.
pub fn poll_terrain_save(
    mut pending: ResMut<PendingTerrainSave>,
    terrain_world: Option<ResMut<TerrainWorld>>,
) {
    let Some(task) = pending.0.as_mut() else {
        return;
    };
    let Some(result) = check_ready(task) else {
        return;
    };
    pending.0 = None;

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

fn extract_chunk_data(heightmap: &HeightmapData, chunk_x: i32, chunk_y: i32) -> ChunkData {
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
