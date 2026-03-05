use super::{CHUNK_SIZE, HEIGHT_SCALE, storage};
use bevy::prelude::*;

/// Apply stored chunk modifications from disk onto the raw un-scaled height grid.
///
/// Stored heights are pre-scaled (0.0–HEIGHT_SCALE); we divide by HEIGHT_SCALE
/// to convert them back to the un-scaled (0.0–1.0) form used by `heights`.
pub fn apply_stored_modifications(
    heights: &mut Vec<f32>,
    metadata: &storage::WorldMetadata,
    grid_w: usize,
    grid_h: usize,
    world_path: &std::path::Path,
) {
    for entry in &metadata.chunk_index {
        if entry.state == storage::ChunkState::Unmodified {
            continue;
        }
        match storage::load_chunk(entry.chunk_x, entry.chunk_y, world_path) {
            Ok(chunk_data) => apply_chunk_to_heights(heights, &chunk_data, grid_w, grid_h),
            Err(e) => warn!(
                "Failed to load saved chunk ({}, {}): {}",
                entry.chunk_x, entry.chunk_y, e
            ),
        }
    }
}

/// Overwrite heights from a single stored `ChunkData`.
fn apply_chunk_to_heights(
    heights: &mut Vec<f32>,
    chunk_data: &storage::ChunkData,
    grid_w: usize,
    grid_h: usize,
) {
    let start_x = chunk_data.chunk_x as usize * (CHUNK_SIZE - 1);
    let start_y = chunk_data.chunk_y as usize * (CHUNK_SIZE - 1);

    match &chunk_data.height_data {
        storage::HeightData::UseBase => {}
        storage::HeightData::Delta(mods) => {
            for m in mods {
                let gx = start_x + m.local_x() as usize;
                let gy = start_y + m.local_y() as usize;
                if gx < grid_w && gy < grid_h {
                    // Deltas are stored pre-scaled; convert to un-scaled
                    heights[gx * grid_h + gy] += m.delta / HEIGHT_SCALE;
                }
            }
        }
        storage::HeightData::Full {
            heights: stored,
            height_min,
            height_max,
        } => {
            let dequantized = storage::dequantize_heights(stored, *height_min, *height_max);
            let ch = chunk_data.height as usize;
            for lx in 0..chunk_data.width as usize {
                for ly in 0..ch {
                    let gx = start_x + lx;
                    let gy = start_y + ly;
                    if gx < grid_w && gy < grid_h {
                        // Stored values are pre-scaled; convert to un-scaled
                        heights[gx * grid_h + gy] = dequantized[lx * ch + ly] / HEIGHT_SCALE;
                    }
                }
            }
        }
    }
}
