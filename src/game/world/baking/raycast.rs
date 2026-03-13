//! CPU-side ray marching against the heightmap for shadow and AO computation.

use bevy::math::Vec3;

use crate::game::world::terrain::HeightmapData;

// ============================================================================
// Heightmap Cache
// ============================================================================

/// A cache-friendly flat snapshot of a `HeightmapData` region, used to avoid
/// repeated `HashMap` lookups in the hot raycast inner loops.
///
/// Extracted once per bake region, then shared (read-only) across all rayon
/// threads via `&BakeHeightCache` (which is `Send + Sync`).
pub struct BakeHeightCache {
    data: Vec<f32>,
    tile_min_x: i32,
    tile_min_z: i32,
    stride: usize,
    depth: usize,
    pub tile_size: (f32, f32),
}

impl BakeHeightCache {
    /// Extract all tiles in the rectangle `[min_wx..max_wx] × [min_wz..max_wz]`
    /// (world space) plus one-tile border padding into a contiguous `Vec<f32>`.
    pub fn extract(
        heightmap: &HeightmapData,
        min_wx: f32,
        min_wz: f32,
        max_wx: f32,
        max_wz: f32,
    ) -> Self {
        let tile_size = heightmap.tile_size;
        let tile_min_x = (min_wx / tile_size.0).floor() as i32 - 1;
        let tile_min_z = (min_wz / tile_size.1).floor() as i32 - 1;
        let tile_max_x = (max_wx / tile_size.0).ceil() as i32 + 1;
        let tile_max_z = (max_wz / tile_size.1).ceil() as i32 + 1;

        let stride = (tile_max_x - tile_min_x + 1) as usize;
        let depth = (tile_max_z - tile_min_z + 1) as usize;

        let mut data = vec![0.0f32; stride * depth];
        for tz in 0..depth {
            for tx in 0..stride {
                let gx = tile_min_x + tx as i32;
                let gz = tile_min_z + tz as i32;
                data[tz * stride + tx] = heightmap.get_tile(gx, gz);
            }
        }

        Self {
            data,
            tile_min_x,
            tile_min_z,
            stride,
            depth,
            tile_size,
        }
    }

    #[inline]
    fn get_tile(&self, x: i32, z: i32) -> f32 {
        let lx = x - self.tile_min_x;
        let lz = z - self.tile_min_z;
        if lx < 0 || lz < 0 || lx as usize >= self.stride || lz as usize >= self.depth {
            return 0.0;
        }
        self.data[lz as usize * self.stride + lx as usize]
    }

    #[inline]
    pub fn sample_height(&self, world_x: f32, world_z: f32) -> f32 {
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
        h00 * (1.0 - tx) * (1.0 - tz)
            + h10 * tx * (1.0 - tz)
            + h01 * (1.0 - tx) * tz
            + h11 * tx * tz
    }

    pub fn sample_normal(&self, world_x: f32, world_z: f32) -> (Vec3, f32) {
        let eps_x = self.tile_size.0;
        let eps_z = self.tile_size.1;
        let h_left = self.sample_height(world_x - eps_x, world_z);
        let h_right = self.sample_height(world_x + eps_x, world_z);
        let h_back = self.sample_height(world_x, world_z - eps_z);
        let h_front = self.sample_height(world_x, world_z + eps_z);
        let dx = (h_right - h_left) / (2.0 * eps_x);
        let dz = (h_front - h_back) / (2.0 * eps_z);
        let normal = Vec3::new(-dx, 1.0, -dz).normalize();
        let slope = normal.y.acos();
        (normal, slope)
    }
}

// ============================================================================
// Shadow Ray
// ============================================================================

/// Cast a shadow ray from `origin` toward `sun_dir` using DDA-style steps.
///
/// Returns `0.0` if the ray is occluded (in shadow) or `1.0` if it reaches
/// `max_dist` without hitting terrain. Uses `steps` evenly-spaced samples.
pub fn cast_shadow_ray(
    origin: Vec3,
    sun_dir: Vec3,
    cache: &BakeHeightCache,
    max_dist: f32,
    steps: u32,
) -> f32 {
    let step_size = max_dist / steps as f32;

    for i in 1..=steps {
        let t = i as f32 * step_size;
        let sample_pos = origin + sun_dir * t;
        let terrain_h = cache.sample_height(sample_pos.x, sample_pos.z);
        if terrain_h > sample_pos.y {
            return 0.0;
        }
    }

    1.0
}

// ============================================================================
// Ambient Occlusion
// ============================================================================

/// Compute ambient occlusion at `origin` by casting `n_rays` hemisphere rays
/// in the half-space defined by `normal`.
///
/// Returns the fraction of unoccluded rays ∈ [0, 1].  Uses a simple
/// deterministic cosine-weighted hemisphere distribution built from the
/// incoming normal so the result is stable across bakes.
pub fn compute_ao(
    origin: Vec3,
    normal: Vec3,
    cache: &BakeHeightCache,
    radius: f32,
    n_rays: u32,
    steps: u32,
) -> f32 {
    if n_rays == 0 {
        return 1.0;
    }

    // Build a tangent frame for the hemisphere sampling.
    let up = if normal.y.abs() < 0.99 {
        Vec3::Y
    } else {
        Vec3::X
    };
    let tangent = up.cross(normal).normalize();
    let bitangent = normal.cross(tangent).normalize();

    let mut unoccluded: u32 = 0;

    for i in 0..n_rays {
        // Stratified cosine-weighted hemisphere distribution using the
        // golden ratio sequence to avoid clustering artefacts.
        let u = (i as f32 + 0.5) / n_rays as f32;
        let angle = std::f32::consts::TAU * golden_ratio_seq(i);

        // Cosine-weighted sample in the hemisphere.
        let r = u.sqrt();
        let cos_theta = (1.0 - u).sqrt();
        let sin_theta = r;
        let local_dir = Vec3::new(sin_theta * angle.cos(), cos_theta, sin_theta * angle.sin());

        // Transform to world space using the tangent frame.
        let world_dir =
            (tangent * local_dir.x + normal * local_dir.y + bitangent * local_dir.z).normalize();

        // Only consider rays that face the correct hemisphere.
        if world_dir.dot(normal) <= 0.0 {
            continue;
        }

        let step_size = radius / steps as f32;
        let mut occluded = false;

        for s in 1..=steps {
            let t = s as f32 * step_size;
            let sample_pos = origin + world_dir * t;
            let terrain_h = cache.sample_height(sample_pos.x, sample_pos.z);
            if terrain_h > sample_pos.y {
                occluded = true;
                break;
            }
        }

        if !occluded {
            unoccluded += 1;
        }
    }

    unoccluded as f32 / n_rays as f32
}

// ============================================================================
// Helpers
// ============================================================================

/// Low-discrepancy angle sequence using the golden ratio, giving uniform
/// coverage of the hemisphere without clustering for any ray count.
#[inline]
fn golden_ratio_seq(i: u32) -> f32 {
    // Golden angle ≈ 2π × (1 − 1/φ)
    const GOLDEN: f32 = 0.618_033_9;
    (i as f32 * GOLDEN).fract()
}
