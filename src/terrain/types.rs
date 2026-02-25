use bevy::prelude::*;

use super::mesh::LOD_LEVELS;

/// Marker component for terrain chunk entities
#[derive(Component)]
pub struct TerrainChunk {
    pub chunk_x: usize,
    pub chunk_y: usize,
    pub lod_meshes: [Handle<Mesh>; LOD_LEVELS],
    pub current_lod: usize,
}

/// Runtime-queryable heightmap resource with bilinear interpolation.
/// Heights are stored pre-scaled in column-major order: `heights[x * grid_h + y]`.
#[derive(Debug, Resource)]
pub struct HeightmapData {
    pub heights: Vec<f32>,
    pub grid_w: usize,
    pub grid_h: usize,
    pub tile_size: (f32, f32),
}

impl HeightmapData {
    /// Get the pre-scaled height at grid coordinates (x, y).
    pub fn get_tile(&self, x: usize, y: usize) -> Option<f32> {
        if x < self.grid_w && y < self.grid_h {
            Some(self.heights[x * self.grid_h + y])
        } else {
            None
        }
    }

    /// Set the pre-scaled height at grid coordinates (x, y).
    pub fn set_tile(&mut self, x: usize, y: usize, value: f32) {
        if x < self.grid_w && y < self.grid_h {
            self.heights[x * self.grid_h + y] = value;
        }
    }

    /// Bilinear interpolation of terrain height at a world-space (x, z) position.
    /// Returns `None` if the position is outside the heightmap bounds.
    pub fn sample_height(&self, world_x: f32, world_z: f32) -> Option<f32> {
        let fx = world_x / self.tile_size.0;
        let fz = world_z / self.tile_size.1;

        // Need at least one cell of margin for interpolation
        if fx < 0.0 || fz < 0.0 {
            return None;
        }

        let ix = fx as usize;
        let iz = fz as usize;

        if ix + 1 >= self.grid_w || iz + 1 >= self.grid_h {
            // Allow sampling exactly on the last grid line
            if ix < self.grid_w
                && iz < self.grid_h
                && fx.fract() < f32::EPSILON
                && fz.fract() < f32::EPSILON
            {
                return self.get_tile(ix, iz);
            }
            return None;
        }

        let tx = fx - ix as f32;
        let tz = fz - iz as f32;

        let h00 = self.heights[ix * self.grid_h + iz];
        let h10 = self.heights[(ix + 1) * self.grid_h + iz];
        let h01 = self.heights[ix * self.grid_h + (iz + 1)];
        let h11 = self.heights[(ix + 1) * self.grid_h + (iz + 1)];

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
