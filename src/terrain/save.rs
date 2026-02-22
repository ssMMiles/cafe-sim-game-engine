//! Bevy systems for saving and loading terrain data.

use bevy::prelude::*;

use super::storage::{
    ChunkData, ChunkState, HeightData, MaterialData, TerrainChunkRegistry, TerrainWorld,
    quantize_heights, save_chunk, save_world,
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

    let dirty: Vec<(u32, u32)> = registry.dirty_chunks.drain().collect();
    info!(
        "Saving {} dirty chunk(s) to {:?}",
        dirty.len(),
        terrain_world.world_path
    );

    for (cx, cy) in dirty {
        let chunk_data = extract_chunk_data(&heightmap, cx, cy);
        match save_chunk(&chunk_data, &terrain_world.world_path) {
            Ok(()) => {
                if let Some(entry) = terrain_world
                    .metadata
                    .chunk_index
                    .iter_mut()
                    .find(|e| e.chunk_x == cx && e.chunk_y == cy)
                {
                    entry.state = ChunkState::FullyOverwritten;
                }
            }
            Err(e) => error!("Failed to save chunk ({cx}, {cy}): {e}"),
        }
    }

    terrain_world.metadata.last_saved = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    match save_world(&terrain_world.metadata, &terrain_world.world_path) {
        Ok(()) => info!("Terrain saved successfully"),
        Err(e) => error!("Failed to save world metadata: {e}"),
    }
}

/// Extract the heights for a single chunk from `HeightmapData` and package them
/// as a `ChunkData` using full quantized storage.
///
/// Heights are stored pre-scaled (matching `HeightmapData.heights`).
fn extract_chunk_data(heightmap: &HeightmapData, chunk_x: u32, chunk_y: u32) -> ChunkData {
    let start_x = chunk_x as usize * (CHUNK_SIZE - 1);
    let start_y = chunk_y as usize * (CHUNK_SIZE - 1);
    let end_x = (start_x + CHUNK_SIZE).min(heightmap.grid_w);
    let end_y = (start_y + CHUNK_SIZE).min(heightmap.grid_h);
    let width = (end_x - start_x) as u32;
    let height = (end_y - start_y) as u32;

    // Collect pre-scaled heights in column-major order (matches HeightmapData layout)
    let mut chunk_heights = Vec::with_capacity((width * height) as usize);
    for lx in 0..width as usize {
        for ly in 0..height as usize {
            let h = heightmap.heights[(start_x + lx) * heightmap.grid_h + (start_y + ly)];
            chunk_heights.push(h);
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
