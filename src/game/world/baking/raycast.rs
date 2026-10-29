//! CPU-side ray marching against the heightmap, plus triangle-BVH tests
//! against static scenery, for shadow and AO computation.
//!
//! Every ray takes two origins: `origin` for the terrain DDA march, which the
//! callers lift well clear of the surface (0.5 units for terrain texels) so
//! the coarse march doesn't re-hit the ground it started on, and
//! `mesh_origin`, the true surface point nudged along the face normal, which
//! the exact triangle test can use without missing thin geometry sitting
//! right on the ground.

use bevy::prelude::*;

use super::bvh::TriangleBvh;
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
    /// Tallest tile in the cache. A ray sample above this cannot be occluded
    /// by terrain, which lets every upward march stop early — see
    /// [`Self::above_terrain`].
    max_height: f32,
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
        let mut max_height = f32::NEG_INFINITY;
        for tz in 0..depth {
            for tx in 0..stride {
                let gx = tile_min_x + tx as i32;
                let gz = tile_min_z + tz as i32;
                let h = heightmap.get_tile(gx, gz);
                data[tz * stride + tx] = h;
                max_height = max_height.max(h);
            }
        }

        Self {
            data,
            tile_min_x,
            tile_min_z,
            stride,
            depth,
            max_height,
            tile_size,
        }
    }

    /// True if `y` is above every tile in the cache, so no terrain sample at
    /// this height or higher can occlude anything.
    ///
    /// Marches use this to bail out instead of stepping to their full length:
    /// for a ray with a non-negative Y component each successive sample is
    /// higher than the last, so once one clears the terrain they all do. This
    /// is **exact**, not an approximation — it skips only samples that were
    /// guaranteed to miss. AO benefits most, its hemisphere rays being aimed
    /// up by construction.
    #[inline]
    pub fn above_terrain(&self, y: f32) -> bool {
        y > self.max_height
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
// Shadow Ray
// ============================================================================

/// Cast a shadow ray toward `sun_dir`: a DDA march against the heightmap
/// from `origin`, then an exact BVH test against scenery from `mesh_origin`.
///
/// Returns `0.0` if the ray is occluded (in shadow) or `1.0` if it reaches
/// `max_dist` without hitting terrain or any triangle.
pub fn cast_shadow_ray(
    origin: Vec3,
    mesh_origin: Vec3,
    sun_dir: Vec3,
    cache: &BakeHeightCache,
    bvh: &TriangleBvh,
    max_dist: f32,
    steps: u32,
) -> f32 {
    let step_size = max_dist / steps as f32;
    let climbing = sun_dir.y >= 0.0;

    for i in 1..=steps {
        let t = i as f32 * step_size;
        let sample_pos = origin + sun_dir * t;
        // Exact early-out: once a climbing ray clears the tallest tile, no
        // later sample can be occluded by terrain either.
        if climbing && cache.above_terrain(sample_pos.y) {
            break;
        }
        let terrain_h = cache.sample_height(sample_pos.x, sample_pos.z);
        if terrain_h > sample_pos.y {
            return 0.0;
        }
    }

    if bvh.occluded(mesh_origin, sun_dir, max_dist) {
        return 0.0;
    }

    1.0
}

/// Cast a shadow ray from `origin` to a point `dist` away along `dir`,
/// stopping at that point rather than marching a fixed reach.
///
/// This is what point and spot lights need: a sun ray may as well run
/// forever, but a local light is only occluded by geometry *between* the
/// surface and the light itself.
///
/// Returns `0.0` if occluded, `1.0` if the light is visible.
pub fn cast_shadow_ray_segment(
    origin: Vec3,
    mesh_origin: Vec3,
    dir: Vec3,
    dist: f32,
    cache: &BakeHeightCache,
    bvh: &TriangleBvh,
    steps: u32,
) -> f32 {
    let steps = steps.max(1);
    let step_size = dist / steps as f32;

    // Skip the final sample: it sits on the light itself, where floating
    // point slop against terrain under the light causes false occlusion.
    let climbing = dir.y >= 0.0;
    for i in 1..steps {
        let sample_pos = origin + dir * (i as f32 * step_size);
        if climbing && cache.above_terrain(sample_pos.y) {
            break;
        }
        if cache.sample_height(sample_pos.x, sample_pos.z) > sample_pos.y {
            return 0.0;
        }
    }

    if bvh.occluded(mesh_origin, dir, dist) {
        return 0.0;
    }

    1.0
}

// ============================================================================
// Ambient Occlusion
// ============================================================================

/// Ambient visibility: `n_rays` cosine-weighted hemisphere rays around
/// `normal`, against both the heightmap (from `origin`) and scenery
/// triangles (from `mesh_origin`), out to `radius`. The latter is what gives
/// contact shadows under furniture.
///
/// **Distance-weighted.** A ray that hits something at distance `t`
/// contributes `t / radius` rather than 0, so an occluder's weight falls off
/// linearly with how far away it is: a chair leg 2 cm away is full
/// occlusion, a canopy 10 m up on a 20 m radius is half. The old binary
/// count gave both the same weight, which turned the 20 m sky-visibility
/// radius into a contact-shadow-strength blanket under anything large.
/// The terrain march runs first because it is ordered along the ray (so
/// its first hit is its nearest, and the `above_terrain` early-out stays
/// valid), then scenery is searched only *inside* that distance.
///
/// Returns mean visibility ∈ [0, 1]. The hemisphere distribution is
/// deterministic, so the result is stable across bakes. Mirrors
/// `compute_ao` in `lightmap_bake.wgsl`.
pub fn compute_ao(
    origin: Vec3,
    mesh_origin: Vec3,
    normal: Vec3,
    cache: &BakeHeightCache,
    bvh: &TriangleBvh,
    radius: f32,
    n_rays: u32,
    steps: u32,
) -> f32 {
    if n_rays == 0 {
        return 1.0;
    }

    let (tangent, bitangent) = build_tangent_frame(normal);
    let step_size = radius / steps as f32;
    let mut visibility = 0.0_f32;

    for i in 0..n_rays {
        // Every direction the generator returns is strictly above the
        // tangent plane (`local.y = sqrt(1 - u) > 0`), so no back-facing
        // check is needed; one here used to silently count such a ray as
        // occluded.
        let (world_dir, _cos_w) =
            hemisphere_dir_for_seed(i, n_rays, normal, tangent, bitangent, 0.0);

        // Nearest terrain hit. Hemisphere rays point up by construction, so
        // the early-out fires often: on gentle terrain most rays clear the
        // tallest tile within a few steps of their 32.
        let mut hit_t = radius;
        let climbing = world_dir.y >= 0.0;
        for s in 1..=steps {
            let t = s as f32 * step_size;
            let sample_pos = origin + world_dir * t;
            if climbing && cache.above_terrain(sample_pos.y) {
                break;
            }
            if cache.sample_height(sample_pos.x, sample_pos.z) > sample_pos.y {
                hit_t = t;
                break;
            }
        }
        // Scenery closer than that, if any.
        if let Some(t) = bvh.nearest_hit(mesh_origin, world_dir, hit_t) {
            hit_t = t;
        }

        visibility += hit_t / radius;
    }

    visibility / n_rays as f32
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
/// Scenery triangles absorb bounce rays (no light leaks through walls) but
/// are not themselves reflectors — only terrain bounces light. Both terrain
/// and object texels *receive* it.
///
/// `seed_offset` is a per-texel hash used to rotate the azimuth of the primary
/// rays and decorrelate the structured golden-ratio pattern across texels,
/// eliminating stripe artifacts when the result is quantized by the toon shader.
///
/// Returns the average bounce irradiance ∈ [0, ∞). Division by `n_rays` is
/// already applied — the caller multiplies by `bounce_strength`.
pub fn compute_indirect_bounce(
    origin: Vec3,
    mesh_origin: Vec3,
    normal: Vec3,
    cache: &BakeHeightCache,
    bvh: &TriangleBvh,
    sun_dir: Vec3,
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

    // Bounce marching uses the full shadow reach, not the (shorter) AO radius:
    // back-of-hill surfaces need to see sunlit terrain up to 4× further away.
    let step_size = shadow_max_dist / steps as f32;
    let mut accum = 0.0_f32;

    // Loop-invariant: the frame depends only on the surface normal.
    let (tangent, bitangent) = build_tangent_frame(normal);

    for i in 0..n_rays {
        // Bug fix: for cosine-weighted sampling the cos(θ) term cancels with the
        // PDF, so we do not need cos_w in the accumulator. The `_cos_w` variable
        // is retained only to gate the early-exit check.
        let (dir, _cos_w) =
            hemisphere_dir_for_seed(i, n_rays, normal, tangent, bitangent, azimuth_offset);
        if _cos_w <= 0.0 {
            continue;
        }

        let mut cur_pos = origin;
        let mut cur_mesh_pos = mesh_origin;
        let mut cur_dir = dir;
        let mut cur_cos_w = _cos_w;
        let mut throughput = 1.0_f32;

        for b in 0..bounce_count {
            let mut hit = false;
            let climbing = cur_dir.y >= 0.0;
            for s in 1..=steps {
                let pos = cur_pos + cur_dir * (s as f32 * step_size);
                if climbing && cache.above_terrain(pos.y) {
                    break;
                }
                let h = cache.sample_height(pos.x, pos.z);
                // Bug fix: strict > to avoid self-intersection on flat terrain.
                if h > pos.y {
                    // A wall between here and the terrain hit absorbs the
                    // path: scenery is opaque to bounce light, not a mirror.
                    if bvh.occluded(cur_mesh_pos, cur_dir, s as f32 * step_size) {
                        break;
                    }
                    // Bug fix: 0.5 unit offset (was 0.1) to clear the step size
                    // and prevent the next bounce from immediately re-hitting.
                    let hit_pos = Vec3::new(pos.x, h + 0.5, pos.z);
                    let hit_mesh_pos = Vec3::new(pos.x, h + 0.02, pos.z);
                    let (hit_normal, _) = cache.sample_normal(pos.x, pos.z);
                    let ndotl_q = hit_normal.dot(sun_dir).max(0.0);
                    let vis_q = cast_shadow_ray(
                        hit_pos,
                        hit_mesh_pos,
                        sun_dir,
                        cache,
                        bvh,
                        shadow_max_dist,
                        steps,
                    );
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
                    cur_mesh_pos = hit_mesh_pos;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::world::terrain::HeightmapData;

    fn flat_cache() -> BakeHeightCache {
        let mut hm = HeightmapData::new((0.25, 0.25));
        for tx in -200..200 {
            for tz in -200..200 {
                hm.set_tile(tx, tz, 0.0);
            }
        }
        BakeHeightCache::extract(&hm, -40.0, -40.0, 40.0, 40.0)
    }

    /// A big horizontal slab at height `y` over the origin.
    fn ceiling(y: f32) -> TriangleBvh {
        TriangleBvh::build(vec![
            [
                Vec3::new(-100.0, y, -100.0),
                Vec3::new(100.0, y, -100.0),
                Vec3::new(-100.0, y, 100.0),
            ],
            [
                Vec3::new(100.0, y, -100.0),
                Vec3::new(100.0, y, 100.0),
                Vec3::new(-100.0, y, 100.0),
            ],
        ])
    }

    /// Occlusion is weighted by distance: a near ceiling darkens far more
    /// than a far one, and a far one no longer reads as total darkness.
    /// Under the old binary count every row here was 0.0.
    #[test]
    fn ao_weights_occluders_by_distance() {
        let cache = flat_cache();
        let origin = Vec3::new(0.0, 0.5, 0.0);
        let mesh_origin = Vec3::new(0.0, 0.02, 0.0);
        let radius = 20.0;
        let ao = |bvh: &TriangleBvh| {
            compute_ao(origin, mesh_origin, Vec3::Y, &cache, bvh, radius, 16, 32)
        };

        let open = ao(&TriangleBvh::build(Vec::new()));
        assert!(
            (open - 1.0).abs() < 1e-6,
            "flat open ground is fully visible: {open}"
        );

        let near = ao(&ceiling(0.5));
        let mid = ao(&ceiling(10.0));
        let far = ao(&ceiling(19.0));
        assert!(
            near < 0.1,
            "a ceiling 0.5 m up is near-total occlusion: {near}"
        );
        assert!(
            mid > 0.4 && mid < 0.75,
            "a ceiling 10 m up on a 20 m radius is about half: {mid}"
        );
        assert!(
            far > mid && far < 1.0,
            "19 m up is lighter still but not open: {far}"
        );
        assert!(near < mid && mid < far);

        // Beyond the radius there is no occluder at all.
        let beyond = ao(&ceiling(25.0));
        assert!((beyond - 1.0).abs() < 1e-6, "{beyond}");
    }

    /// Terrain is weighted the same way as scenery — a wall of ground is an
    /// occluder like any other, and the nearest one wins.
    #[test]
    fn ao_takes_the_nearer_of_terrain_and_scenery() {
        let mut hm = HeightmapData::new((0.25, 0.25));
        for tx in -200..200 {
            for tz in -200..200 {
                // A cliff: everything at x >= 1 m is 50 m tall.
                hm.set_tile(tx, tz, if tx >= 4 { 50.0 } else { 0.0 });
            }
        }
        let cache = BakeHeightCache::extract(&hm, -40.0, -40.0, 40.0, 40.0);
        let origin = Vec3::new(0.0, 0.5, 0.0);
        let mesh_origin = Vec3::new(0.0, 0.02, 0.0);
        let radius = 20.0;

        let empty = TriangleBvh::build(Vec::new());
        let cliff_only = compute_ao(origin, mesh_origin, Vec3::Y, &cache, &empty, radius, 16, 32);
        assert!(cliff_only < 1.0 && cliff_only > 0.3, "{cliff_only}");

        // A ceiling nearer than the cliff lowers it further; one farther
        // than every cliff hit along the ray changes nothing on that ray.
        let with_near = compute_ao(
            origin,
            mesh_origin,
            Vec3::Y,
            &cache,
            &ceiling(0.3),
            radius,
            16,
            32,
        );
        assert!(with_near < cliff_only, "{with_near} vs {cliff_only}");
    }
}

// ============================================================================
// Cost measurement
// ============================================================================

/// Per-texel ray cost of the CPU terrain bake, measured rather than guessed.
///
/// Run with:
/// `cargo test --release terrain_bake_cost_breakdown -- --ignored --nocapture`
///
/// This exists because the object-bake optimisation found the bottleneck was
/// nowhere near where it was assumed to be (see memory.md). Numbers here are
/// per *texel*; multiply by `terrain_resolution²` (1024² ≈ 1.05M) for one
/// chunk, and again by the chunk count in the bake region.
#[cfg(test)]
mod cost {
    use super::*;
    use crate::game::world::baking::bvh::TriangleBvh;
    use crate::game::world::terrain::HeightmapData;
    use std::time::Instant;

    /// Rolling-hill terrain over the cache region, plus a few scenery
    /// triangles so BVH traversal is exercised rather than trivially empty.
    fn scene() -> (BakeHeightCache, TriangleBvh) {
        let mut hm = HeightmapData::new((0.25, 0.25));
        // 63.75 m chunk + 80 m ray reach each side => ~224 m => ~900 tiles.
        for tx in -400..600 {
            for tz in -400..600 {
                let x = tx as f32 * 0.25;
                let z = tz as f32 * 0.25;
                hm.set_tile(tx, tz, 3.0 * (x * 0.05).sin() * (z * 0.05).cos());
            }
        }
        let cache = BakeHeightCache::extract(&hm, -80.0, -80.0, 143.75, 143.75);

        // A handful of boxes scattered over the chunk, 12 triangles each.
        let mut tris = Vec::new();
        for i in 0..24 {
            let cx = (i % 6) as f32 * 10.0 + 2.0;
            let cz = (i / 6) as f32 * 10.0 + 2.0;
            let base = Vec3::new(cx, 4.0, cz);
            for k in 0..12 {
                let o = k as f32 * 0.3;
                tris.push([
                    base + Vec3::new(o, 0.0, 0.0),
                    base + Vec3::new(o + 1.0, 2.0, 0.0),
                    base + Vec3::new(o, 2.0, 1.0),
                ]);
            }
        }
        (cache, TriangleBvh::build(tris))
    }

    fn sample_points(n: usize, cache: &BakeHeightCache) -> Vec<(Vec3, Vec3, Vec3)> {
        (0..n)
            .map(|i| {
                let u = (i % 97) as f32 / 97.0;
                let v = (i / 97) as f32 / (n as f32 / 97.0).max(1.0);
                let wx = u * 63.75;
                let wz = v * 63.75;
                let surface = Vec3::new(wx, cache.sample_height(wx, wz), wz);
                let (normal, _) = cache.sample_normal(wx, wz);
                (surface + Vec3::Y * 0.5, surface + normal * 0.02, normal)
            })
            .collect()
    }

    #[test]
    #[ignore = "timing benchmark, not a correctness check"]
    fn terrain_bake_cost_breakdown() {
        const N: usize = 20_000;
        let (cache, bvh) = scene();
        let pts = sample_points(N, &cache);
        let sun_dir = Vec3::new(0.4, 0.8, 0.3).normalize();

        // Defaults from BakeConfig.
        let (ao_radius, shadow_steps, ao_rays, ao_steps, bounce_rays) =
            (20.0_f32, 64_u32, 16_u32, 32_u32, 8_u32);
        let reach = ao_radius * 4.0;

        let t = Instant::now();
        let mut sink = 0.0_f32;
        for &(o, mo, _) in &pts {
            sink += cast_shadow_ray(o, mo, sun_dir, &cache, &bvh, reach, shadow_steps);
        }
        let sun = t.elapsed();

        let t = Instant::now();
        for &(o, mo, n) in &pts {
            sink += compute_ao(o, mo, n, &cache, &bvh, ao_radius, ao_rays, ao_steps);
        }
        let ao = t.elapsed();

        // AO against an empty BVH isolates terrain marching from BVH traversal.
        let empty = TriangleBvh::build(Vec::new());
        let t = Instant::now();
        for &(o, mo, n) in &pts {
            sink += compute_ao(o, mo, n, &cache, &empty, ao_radius, ao_rays, ao_steps);
        }
        let ao_no_bvh = t.elapsed();

        let t = Instant::now();
        for (i, &(o, mo, n)) in pts.iter().enumerate() {
            sink += compute_indirect_bounce(
                o,
                mo,
                n,
                &cache,
                &bvh,
                sun_dir,
                reach,
                bounce_rays,
                shadow_steps,
                0.5,
                1,
                i as u32,
            );
        }
        let bounce = t.elapsed();

        let per = |d: std::time::Duration| d.as_secs_f64() * 1e9 / N as f64;
        let total = per(sun) + per(ao) + per(bounce);
        // 1024² texels per chunk.
        let chunk_ms = total * 1024.0 * 1024.0 / 1e6;
        println!("\n--- CPU terrain bake, per texel (sink={sink:.1}) ---");
        println!(
            "  sun shadow : {:8.0} ns  ({:5.1}%)",
            per(sun),
            100.0 * per(sun) / total
        );
        println!(
            "  ao         : {:8.0} ns  ({:5.1}%)",
            per(ao),
            100.0 * per(ao) / total
        );
        println!(
            "  bounce     : {:8.0} ns  ({:5.1}%)",
            per(bounce),
            100.0 * per(bounce) / total
        );
        println!(
            "    (ao without any BVH: {:.0} ns => BVH traversal is {:.0} ns of ao)",
            per(ao_no_bvh),
            per(ao) - per(ao_no_bvh)
        );
        println!("  total      : {total:8.0} ns");
        println!("  => {chunk_ms:.0} ms per 1024² chunk, single-threaded");
        println!(
            "  => {:.1} s for a 3x3 bake region, single-threaded",
            chunk_ms * 9.0 / 1000.0
        );

        // What the split-pass bake costs: sun every texel, ao+bounce on a
        // grid `d` times coarser (so 1/d² as many of them).
        for d in [2.0_f64, 4.0, 8.0] {
            let projected = per(sun) + (per(ao) + per(bounce)) / (d * d);
            let ms = projected * 1024.0 * 1024.0 / 1e6;
            println!(
                "  downsample {d:.0}: {projected:6.0} ns/texel => {ms:5.0} ms/chunk, \
                 {:.1} s for 3x3 ({:.1}x faster)",
                ms * 9.0 / 1000.0,
                total / projected
            );
        }
    }
}
