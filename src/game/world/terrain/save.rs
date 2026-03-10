//! Bevy systems for saving and loading terrain data.

use bevy::prelude::*;

use super::storage::{
    ChunkData, ChunkIndexEntry, ChunkState, HeightData, MaterialData, TerrainChunkRegistry,
    TerrainWorld, quantize_heights, save_chunk, save_world,
};
use super::{CHUNK_SIZE, HeightmapData};

/// System: save all dirty terrain chunks to disk on Shift+S.
pub fn save_terrain(
    input: Res<ButtonInput<KeyCode>>,
    terrain_world: Option<ResMut<TerrainWorld>>,
    registry: Option<ResMut<TerrainChunkRegistry>>,
    heightmap: Option<Res<HeightmapData>>,
) {
    if !(input.just_pressed(KeyCode::KeyS)
        && (input.pressed(KeyCode::ShiftLeft) || input.pressed(KeyCode::ShiftRight)))
    {
        return;
    }

    let (Some(mut terrain_world), Some(mut registry), Some(heightmap)) =
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
        "Saving {} dirty chunk(s) to {:?}",
        dirty.len(),
        terrain_world.world_path
    );

    for (cx, cy) in dirty {
        let chunk_data = extract_chunk_data(&heightmap, cx, cy);
        match save_chunk(&chunk_data, &terrain_world.world_path) {
            Ok(()) => {
                // Update existing index entry or push a new one (for infinite chunks
                // that were not in the original bounded seed index).
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
            Err(e) => error!("Failed to save chunk ({cx}, {cy}): {e}"),
        }
    }

    terrain_world.metadata.last_saved = 0;

    match save_world(&terrain_world.metadata, &terrain_world.world_path) {
        Ok(()) => info!("Terrain saved successfully"),
        Err(e) => error!("Failed to save world metadata: {e}"),
    }
}

/// Extract the heights for a single chunk from `HeightmapData` and package them
/// as a `ChunkData` using full quantized storage.
///
/// Heights are stored pre-scaled (matching `HeightmapData` values).
fn extract_chunk_data(heightmap: &HeightmapData, chunk_x: i32, chunk_y: i32) -> ChunkData {
    let cs = (CHUNK_SIZE - 1) as i32;
    let start_x = chunk_x * cs;
    let start_y = chunk_y * cs;

    let width = CHUNK_SIZE as u32;
    let height = CHUNK_SIZE as u32;

    // Collect pre-scaled heights in column-major order (matches generate_lod_mesh layout)
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
