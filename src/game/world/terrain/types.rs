use bevy::prelude::*;
use std::collections::HashMap;

use super::{CHUNK_SIZE, mesh::LOD_LEVELS};

/// Marker component for terrain chunk entities
#[derive(Component)]
pub struct TerrainChunk {
    #[allow(dead_code)]
    pub chunk_x: i32,
    #[allow(dead_code)]
    pub chunk_y: i32,
    pub lod_meshes: [Handle<Mesh>; LOD_LEVELS],
    pub current_lod: usize,
}

/// Runtime-queryable heightmap resource — sparse, infinite, bilinear-interpolated.
///
/// Heights are stored pre-scaled (world units). Any tile not explicitly set
/// is implicitly 0.0 (flat ground). Tiles are keyed by signed integer grid
/// coordinates so negative-X/Z terrain is supported.
#[derive(Debug, Resource, Default)]
pub struct HeightmapData {
    heights: HashMap<(i32, i32), f32>,
    pub tile_size: (f32, f32),
}

impl HeightmapData {
    pub fn new(tile_size: (f32, f32)) -> Self {
        Self {
            heights: HashMap::new(),
            tile_size,
        }
    }

    /// Get the pre-scaled height at tile coordinates (x, y). Returns 0.0 if not set.
    pub fn get_tile(&self, x: i32, y: i32) -> f32 {
        self.heights.get(&(x, y)).copied().unwrap_or(0.0)
    }

    /// Set the pre-scaled height at tile coordinates (x, y).
    pub fn set_tile(&mut self, x: i32, y: i32, value: f32) {
        if value == 0.0 {
            self.heights.remove(&(x, y));
        } else {
            self.heights.insert((x, y), value);
        }
    }

    /// Remove all heightmap tiles belonging to chunk (cx, cy).
    /// Call this when a chunk is unloaded to prevent unbounded memory growth.
    pub fn remove_chunk_tiles(&mut self, cx: i32, cy: i32) {
        let cs = (CHUNK_SIZE - 1) as i32;
        let start_x = cx * cs;
        let start_y = cy * cs;
        for tx in start_x..=start_x + cs {
            for ty in start_y..=start_y + cs {
                self.heights.remove(&(tx, ty));
            }
        }
    }

    /// Bilinear interpolation of terrain height at a world-space (x, z) position.
    /// Returns `Some(h)` — including `Some(0.0)` for tiles that haven't been loaded
    /// yet (flat ground).
    pub fn sample_height(&self, world_x: f32, world_z: f32) -> Option<f32> {
        let fx = world_x / self.tile_size.0;
        let fz = world_z / self.tile_size.1;

        let ix = fx.floor() as i32;
        let iz = fz.floor() as i32;

        let tx = fx - ix as f32;
        let tz = fz - iz as f32;

        let h00 = self.get_tile(ix, iz);
        let h10 = self.get_tile(ix + 1, iz);
        let h01 = self.get_tile(ix, iz + 1);
        let h11 = self.get_tile(ix + 1, iz + 1);

        let h = h00 * (1.0 - tx) * (1.0 - tz)
            + h10 * tx * (1.0 - tz)
            + h01 * (1.0 - tx) * tz
            + h11 * tx * tz;

        Some(h)
    }

    /// Sample terrain surface normal and slope angle (radians) at a world-space position.
    /// Uses central differences with a one-tile epsilon.
    pub fn sample_normal(&self, world_x: f32, world_z: f32) -> Option<(Vec3, f32)> {
        let eps_x = self.tile_size.0;
        let eps_z = self.tile_size.1;

        let h_left = self.sample_height(world_x - eps_x, world_z)?;
        let h_right = self.sample_height(world_x + eps_x, world_z)?;
        let h_back = self.sample_height(world_x, world_z - eps_z)?;
        let h_front = self.sample_height(world_x, world_z + eps_z)?;

        let dx = (h_right - h_left) / (2.0 * eps_x);
        let dz = (h_front - h_back) / (2.0 * eps_z);

        let normal = Vec3::new(-dx, 1.0, -dz).normalize();
        let slope = normal.y.acos();

        Some((normal, slope))
    }
}
