//! CPU-side ray marching against the heightmap for shadow and AO computation.

use bevy::{
    asset::Assets,
    ecs::entity::Entity,
    mesh::{Mesh, Mesh3d, VertexAttributeValues},
    prelude::*,
};
use parry3d::na::Point3;

use crate::game::world::{objects::PlacedObject, terrain::HeightmapData};

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

    // ── Accessors used by the GPU baking path ────────────────────────────────

    /// World-space X coordinate of the leftmost tile edge in the cache.
    #[inline]
    pub fn cache_min_x(&self) -> f32 {
        self.tile_min_x as f32 * self.tile_size.0
    }

    /// World-space Z coordinate of the topmost tile edge in the cache.
    #[inline]
    pub fn cache_min_z(&self) -> f32 {
        self.tile_min_z as f32 * self.tile_size.1
    }

    /// Tile-space minimum X (signed).
    #[inline]
    pub fn tile_min_x(&self) -> i32 {
        self.tile_min_x
    }

    /// Tile-space minimum Z (signed).
    #[inline]
    pub fn tile_min_z(&self) -> i32 {
        self.tile_min_z
    }

    /// Number of tile columns in the cache.
    #[inline]
    pub fn stride(&self) -> usize {
        self.stride
    }

    /// Number of tile rows in the cache.
    #[inline]
    pub fn depth(&self) -> usize {
        self.depth
    }

    /// Consume the cache and return the raw `Vec<f32>` height data.
    pub fn into_data(self) -> Vec<f32> {
        self.data
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
// Convex Hull Shadow Proxies
// ============================================================================

/// Collect convex hull triangles (world space) for all placed objects whose
/// bounding box overlaps the bake region. `exclude` skips the specified entity
/// to prevent self-shadowing artifacts when baking an object's own lightmap.
///
/// Returns a flat list of world-space triangles: each `[Vec3; 3]` is one face
/// of a convex hull approximation of a placed object.
pub fn collect_shadow_hulls(
    placed_query: &Query<(Entity, &PlacedObject, &GlobalTransform)>,
    children_query: &Query<&Children>,
    mesh_query: &Query<(&Mesh3d, &GlobalTransform)>,
    meshes: &Assets<Mesh>,
    min_x: f32,
    max_x: f32,
    min_z: f32,
    max_z: f32,
    exclude: Option<Entity>,
) -> Vec<[Vec3; 3]> {
    let mut all_tris = Vec::new();

    for (entity, _placed, _root_gt) in placed_query.iter() {
        if Some(entity) == exclude {
            continue;
        }

        // Collect all world-space positions from mesh descendants.
        let mut positions: Vec<Vec3> = Vec::new();
        collect_world_positions(entity, children_query, mesh_query, meshes, &mut positions);

        if positions.len() < 4 {
            continue;
        }

        // Compute AABB and skip if outside the bake region.
        let mut obj_min_x = f32::MAX;
        let mut obj_max_x = f32::MIN;
        let mut obj_min_z = f32::MAX;
        let mut obj_max_z = f32::MIN;
        for p in &positions {
            obj_min_x = obj_min_x.min(p.x);
            obj_max_x = obj_max_x.max(p.x);
            obj_min_z = obj_min_z.min(p.z);
            obj_max_z = obj_max_z.max(p.z);
        }
        if obj_max_x < min_x || obj_min_x > max_x || obj_max_z < min_z || obj_min_z > max_z {
            continue;
        }

        // Convert to nalgebra points for parry3d.
        let na_points: Vec<Point3<f32>> = positions
            .iter()
            .map(|p| Point3::new(p.x, p.y, p.z))
            .collect();

        // Compute convex hull. Returns (vertices, triangle_indices).
        let Ok((verts, tri_indices)) = parry3d::transformation::try_convex_hull(&na_points) else {
            continue;
        };

        // Convert hull triangles to [Vec3; 3].
        for tri_idx in &tri_indices {
            let v0 = verts[tri_idx[0] as usize];
            let v1 = verts[tri_idx[1] as usize];
            let v2 = verts[tri_idx[2] as usize];
            all_tris.push([
                Vec3::new(v0.x, v0.y, v0.z),
                Vec3::new(v1.x, v1.y, v1.z),
                Vec3::new(v2.x, v2.y, v2.z),
            ]);
        }
    }

    all_tris
}

/// Recursively collect world-space vertex positions from an entity's subtree.
fn collect_world_positions(
    entity: Entity,
    children_query: &Query<&Children>,
    mesh_query: &Query<(&Mesh3d, &GlobalTransform)>,
    meshes: &Assets<Mesh>,
    out: &mut Vec<Vec3>,
) {
    if let Ok((mesh_handle, gt)) = mesh_query.get(entity) {
        if let Some(mesh) = meshes.get(mesh_handle.id()) {
            if let Some(VertexAttributeValues::Float32x3(positions)) =
                mesh.attribute(Mesh::ATTRIBUTE_POSITION)
            {
                let mat = gt.to_matrix();
                for pos in positions {
                    let wp = (mat * Vec3::from(*pos).extend(1.0)).truncate();
                    out.push(wp);
                }
            }
        }
    }
    if let Ok(children) = children_query.get(entity) {
        let child_entities: Vec<Entity> = children.to_vec();
        for child in child_entities {
            collect_world_positions(child, children_query, mesh_query, meshes, out);
        }
    }
}

// ============================================================================
// Shadow Ray
// ============================================================================

/// Möller–Trumbore ray-triangle intersection. Returns `Some(t)` if the ray
/// hits the triangle at parameter `t > 0.001`, or `None` otherwise.
#[inline]
fn moller_trumbore(origin: Vec3, dir: Vec3, v0: Vec3, v1: Vec3, v2: Vec3) -> Option<f32> {
    let e1 = v1 - v0;
    let e2 = v2 - v0;
    let h = dir.cross(e2);
    let a = e1.dot(h);
    if a.abs() < 1e-6 {
        return None;
    }
    let f = 1.0 / a;
    let s = origin - v0;
    let u = f * s.dot(h);
    if !(0.0..=1.0).contains(&u) {
        return None;
    }
    let q = s.cross(e1);
    let v = f * dir.dot(q);
    if v < 0.0 || u + v > 1.0 {
        return None;
    }
    let t = f * e2.dot(q);
    if t > 0.001 { Some(t) } else { None }
}

/// Cast a shadow ray from `origin` toward `sun_dir` using DDA-style steps,
/// then test convex hull shadow proxies for placed objects.
///
/// Returns `0.0` if the ray is occluded (in shadow) or `1.0` if it reaches
/// `max_dist` without hitting terrain or any hull triangle.
pub fn cast_shadow_ray(
    origin: Vec3,
    sun_dir: Vec3,
    cache: &BakeHeightCache,
    hull_tris: &[[Vec3; 3]],
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

    // Check convex hull shadow proxies for placed objects.
    for tri in hull_tris {
        if let Some(t) = moller_trumbore(origin, sun_dir, tri[0], tri[1], tri[2]) {
            if t < max_dist {
                return 0.0;
            }
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

    let (tangent, bitangent) = build_tangent_frame(normal);
    let step_size = radius / steps as f32;
    let mut unoccluded: u32 = 0;

    for i in 0..n_rays {
        let (world_dir, cos_w) =
            hemisphere_dir_for_seed(i, n_rays, normal, tangent, bitangent, 0.0);
        if cos_w <= 0.0 {
            continue;
        }

        let mut occluded = false;
        for s in 1..=steps {
            let sample_pos = origin + world_dir * (s as f32 * step_size);
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
// Indirect Bounce
// ============================================================================

/// Multi-bounce indirect lighting: cast `n_rays` hemisphere rays from `origin`,
/// following each ray through up to `bounce_count` terrain reflections.
///
/// Each bounce accumulates the direct-lit contribution at the hit point
/// attenuated by the current path throughput, then multiplies throughput by
/// `terrain_albedo` so successive bounces contribute diminishing amounts.
///
/// `seed_offset` is a per-texel hash used to rotate the azimuth of the primary
/// rays and decorrelate the structured golden-ratio pattern across texels,
/// eliminating stripe artifacts when the result is quantized by the toon shader.
///
/// Returns the average bounce irradiance ∈ [0, ∞). Division by `n_rays` is
/// already applied — the caller multiplies by `bounce_strength`.
pub fn compute_indirect_bounce(
    origin: Vec3,
    normal: Vec3,
    cache: &BakeHeightCache,
    hull_tris: &[[Vec3; 3]],
    sun_dir: Vec3,
    ao_radius: f32,
    shadow_max_dist: f32,
    n_rays: u32,
    steps: u32,
    terrain_albedo: f32,
    bounce_count: u32,
    seed_offset: u32,
) -> f32 {
    if n_rays == 0 || bounce_count == 0 {
        return 0.0;
    }

    // Rotate the entire hemisphere sample set by a per-texel azimuth so adjacent
    // texels don't share the same 8 directions (prevents correlated stripe patterns).
    let azimuth_offset = float01(hash_u32(seed_offset)) * std::f32::consts::TAU;

    // Use the full shadow reach for bounce marching, not just ao_radius.
    // Back-of-hill surfaces need to see sunlit terrain that can be 4× further away.
    let step_size = shadow_max_dist / steps as f32;
    let mut accum = 0.0_f32;

    for i in 0..n_rays {
        let (tangent, bitangent) = build_tangent_frame(normal);
        // Bug fix: for cosine-weighted sampling the cos(θ) term cancels with the
        // PDF, so we do not need cos_w in the accumulator. The `_cos_w` variable
        // is retained only to gate the early-exit check.
        let (dir, _cos_w) =
            hemisphere_dir_for_seed(i, n_rays, normal, tangent, bitangent, azimuth_offset);
        if _cos_w <= 0.0 {
            continue;
        }

        let mut cur_pos = origin;
        let mut cur_dir = dir;
        let mut cur_cos_w = _cos_w;
        let mut throughput = 1.0_f32;

        for b in 0..bounce_count {
            let mut hit = false;
            for s in 1..=steps {
                let pos = cur_pos + cur_dir * (s as f32 * step_size);
                let h = cache.sample_height(pos.x, pos.z);
                // Bug fix: strict > to avoid self-intersection on flat terrain.
                if h > pos.y {
                    // Bug fix: 0.5 unit offset (was 0.1) to clear the step size
                    // and prevent the next bounce from immediately re-hitting.
                    let hit_pos = Vec3::new(pos.x, h + 0.5, pos.z);
                    let (hit_normal, _) = cache.sample_normal(pos.x, pos.z);
                    let ndotl_q = hit_normal.dot(sun_dir).max(0.0);
                    let vis_q =
                        cast_shadow_ray(hit_pos, sun_dir, cache, hull_tris, shadow_max_dist, steps);
                    // Bug fix: no cur_cos_w here — it cancels with the PDF for
                    // cosine-weighted hemisphere sampling.
                    accum += throughput * ndotl_q * vis_q * terrain_albedo;
                    throughput *= terrain_albedo;

                    // Resample direction at the hit point using a proper per-bounce
                    // hash rather than a cyclic seed shift — avoids correlated directions.
                    let bounce_seed = hash_u32(
                        seed_offset
                            .wrapping_add(i.wrapping_mul(1_000_003))
                            .wrapping_add(b.wrapping_mul(2_654_435_761)),
                    );
                    let bounce_az =
                        float01(hash_u32(bounce_seed.wrapping_add(1))) * std::f32::consts::TAU;
                    let bounce_idx = (float01(bounce_seed) * n_rays as f32) as u32 % n_rays;
                    let (t, bt) = build_tangent_frame(hit_normal);
                    let (new_dir, new_cos_w) =
                        hemisphere_dir_for_seed(bounce_idx, n_rays, hit_normal, t, bt, bounce_az);
                    cur_pos = hit_pos;
                    cur_dir = new_dir;
                    cur_cos_w = new_cos_w;
                    hit = true;
                    break;
                }
            }
            if !hit || cur_cos_w <= 0.0 {
                break;
            }
        }
    }

    accum / n_rays as f32
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

/// MurmurHash3 finalizer — maps any u32 to a well-distributed u32.
#[inline]
fn hash_u32(mut x: u32) -> u32 {
    x ^= x >> 16;
    x = x.wrapping_mul(0x45d9f3b);
    x ^= x >> 16;
    x = x.wrapping_mul(0x45d9f3b);
    x ^= x >> 16;
    x
}

/// Map a u32 hash to f32 in [0, 1).
#[inline]
fn float01(h: u32) -> f32 {
    f32::from_bits(0x3f80_0000 | (h >> 9)) - 1.0
}

/// Build an orthonormal tangent frame for the given `normal`.
///
/// Returns `(tangent, bitangent)` such that `(tangent, normal, bitangent)` form
/// a right-handed basis aligned with the hemisphere above `normal`.
#[inline]
fn build_tangent_frame(normal: Vec3) -> (Vec3, Vec3) {
    let up = if normal.y.abs() < 0.99 {
        Vec3::Y
    } else {
        Vec3::X
    };
    let tangent = up.cross(normal).normalize();
    let bitangent = normal.cross(tangent).normalize();
    (tangent, bitangent)
}

/// Sample a cosine-weighted hemisphere direction for the given `seed` index
/// out of `n_total` rays, using the golden-ratio sequence for the azimuth.
///
/// `azimuth_offset` is added to the azimuth angle; pass `0.0` for the
/// standard deterministic sequence, or a per-texel hash to decorrelate
/// structured patterns across texels.
///
/// Returns `(world_dir, cos_w)` where `cos_w = dot(dir, normal)`.
#[inline]
fn hemisphere_dir_for_seed(
    seed: u32,
    n_total: u32,
    normal: Vec3,
    tangent: Vec3,
    bitangent: Vec3,
    azimuth_offset: f32,
) -> (Vec3, f32) {
    let u = (seed as f32 + 0.5) / n_total as f32;
    let angle = std::f32::consts::TAU * golden_ratio_seq(seed) + azimuth_offset;
    let r = u.sqrt();
    let local = Vec3::new(r * angle.cos(), (1.0 - u).sqrt(), r * angle.sin());
    let dir = (tangent * local.x + normal * local.y + bitangent * local.z).normalize();
    let cos_w = dir.dot(normal).max(0.0);
    (dir, cos_w)
}
