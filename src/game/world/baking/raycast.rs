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
use super::lightmap::BakeLighting;
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
    /// What each tile sends back as bounce light, packed linear RGB
    /// (`pack_albedo`) on exactly the grid `data` uses. Filled from the
    /// paint map by [`Self::with_paint`]; a bare [`Self::extract`] leaves
    /// every tile on the neutral 0.5 grey the bakers assumed before
    /// painting existed (and that unit tests still assume).
    albedo: Vec<u32>,
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
            albedo: vec![pack_albedo(Vec3::splat(0.5)); data.len()],
            data,
            tile_min_x,
            tile_min_z,
            stride,
            depth,
            max_height,
            tile_size,
        }
    }

    /// Fill the albedo grid from the paint map: each tile becomes its base
    /// and overlay layers' bake albedos (palette-resolved, so removed slots
    /// show their fallback), lerped by the tile's blend byte. Tiles outside
    /// every loaded chunk read as the default sample — layer 0's albedo —
    /// which matches what the renderer would show there after streaming.
    ///
    /// Chunk-major, not tile-major: a cache covers ~a million tiles and this
    /// runs on the main thread (per object job on wasm), so one map lookup
    /// per *chunk* and straight slice walks, never a per-tile `sample_at`.
    pub fn with_paint(
        mut self,
        paint: &crate::game::world::terrain::paint::PaintmapData,
        palette: &crate::game::world::terrain::paint::TerrainPalette,
    ) -> Self {
        use crate::game::world::terrain::paint::{
            PAINT_SIZE, paint_base, paint_blend, paint_index, paint_overlay,
        };
        let albedos = palette.resolved_bake_albedos();
        let layer = |id: u8| albedos[(id as usize).min(albedos.len() - 1)];
        // Tiles no loaded chunk holds read as the default sample: layer 0,
        // no overlay — which is also what streaming would show there.
        self.albedo.fill(pack_albedo(layer(0)));

        // Chunks sharing an edge tile store the same value, so overlap rows
        // being written twice is harmless.
        let cs = (PAINT_SIZE - 1) as i32;
        let max_tx = self.tile_min_x + self.stride as i32 - 1;
        let max_tz = self.tile_min_z + self.depth as i32 - 1;
        for cy in (self.tile_min_z - cs).div_euclid(cs)..=max_tz.div_euclid(cs) {
            for cx in (self.tile_min_x - cs).div_euclid(cs)..=max_tx.div_euclid(cs) {
                let Some(samples) = paint.chunk_samples((cx, cy)) else {
                    continue;
                };
                // The chunk's local tiles that fall inside the cache rect.
                let lx0 = (self.tile_min_x - cx * cs).max(0);
                let lx1 = (max_tx - cx * cs).min(cs);
                let ly0 = (self.tile_min_z - cy * cs).max(0);
                let ly1 = (max_tz - cy * cs).min(cs);
                for lx in lx0..=lx1 {
                    let tx = (cx * cs + lx - self.tile_min_x) as usize;
                    for ly in ly0..=ly1 {
                        let tz = (cy * cs + ly - self.tile_min_z) as usize;
                        let sample = samples[paint_index(lx as usize, ly as usize)];
                        let blend = paint_blend(sample) as f32 / 255.0;
                        let albedo =
                            layer(paint_base(sample)).lerp(layer(paint_overlay(sample)), blend);
                        self.albedo[tz * self.stride + tx] = pack_albedo(albedo);
                    }
                }
            }
        }
        self
    }

    /// Every tile on one albedo. Tests use it to switch bounce light off
    /// (`Vec3::ZERO`) or pin it, the way the old scalar `terrain_albedo`
    /// config knob did.
    #[cfg(test)]
    pub fn with_uniform_albedo(mut self, albedo: Vec3) -> Self {
        self.albedo.fill(pack_albedo(albedo));
        self
    }

    /// The bounce albedo of the terrain at a world position: the paint
    /// map's tile under it (nearest tile — bounce light has no use for
    /// bilinear paint). The GPU bake mirrors this exactly
    /// (`terrain_albedo_at` in `lightmap_bake.wgsl`), decoding the same
    /// packed bytes, so the two paths quantise identically.
    #[inline]
    pub fn albedo_at(&self, world_x: f32, world_z: f32) -> Vec3 {
        let ix = (world_x / self.tile_size.0).floor() as i32 - self.tile_min_x;
        let iz = (world_z / self.tile_size.1).floor() as i32 - self.tile_min_z;
        if ix < 0 || iz < 0 || ix as usize >= self.stride || iz as usize >= self.depth {
            return Vec3::splat(0.5);
        }
        unpack_albedo(self.albedo[iz as usize * self.stride + ix as usize])
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

    /// Consume the cache and return the raw height and packed-albedo grids,
    /// in that order — what the GPU bake uploads as its two terrain buffers.
    pub fn into_parts(self) -> (Vec<f32>, Vec<u32>) {
        (self.data, self.albedo)
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

/// Linear RGB → the byte-packed form the albedo grid stores (r in the low
/// byte). 8 bits per channel is plenty for a bounce albedo, and the packed
/// grid is uploaded to the GPU bake verbatim, so CPU and GPU read the same
/// quantised values.
#[inline]
pub fn pack_albedo(albedo: Vec3) -> u32 {
    let q = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u32;
    q(albedo.x) | (q(albedo.y) << 8) | (q(albedo.z) << 16)
}

#[inline]
fn unpack_albedo(packed: u32) -> Vec3 {
    Vec3::new(
        (packed & 0xff) as f32,
        ((packed >> 8) & 0xff) as f32,
        ((packed >> 16) & 0xff) as f32,
    ) / 255.0
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
    seed_offset: u32,
) -> f32 {
    if n_rays == 0 {
        return 1.0;
    }

    // Rotate the sample set by a per-texel azimuth, exactly as
    // `compute_indirect_bounce` does. Without it every texel sharing a
    // normal marches the *same* `n_rays` directions, so the estimate is not
    // noisy but structured: it steps by ~1/n_rays wherever one of those
    // fixed directions crosses an occluder's silhouette, tracing curves
    // across the surface that follow the sample pattern rather than the
    // geometry. The toon quantiser then draws them. Decorrelating turns
    // that structure into white noise, which the blur averages away.
    let azimuth_offset = float01(hash_u32(seed_offset)) * std::f32::consts::TAU;
    let (tangent, bitangent) = build_tangent_frame(normal);
    let step_size = radius / steps as f32;
    let mut visibility = 0.0_f32;

    for i in 0..n_rays {
        // Every direction the generator returns is strictly above the
        // tangent plane (`local.y = sqrt(1 - u) > 0`), so no back-facing
        // check is needed; one here used to silently count such a ray as
        // occluded.
        let (world_dir, _cos_w) =
            hemisphere_dir_for_seed(i, n_rays, normal, tangent, bitangent, azimuth_offset);

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
/// following each ray through up to `bounce_count` reflections.
///
/// Both terrain and scenery re-emit, each with its own albedo, through the
/// shared [`BakeLighting::reemitted`]. Each bounce accumulates that hit's
/// re-emission attenuated by the current path throughput, then multiplies
/// throughput by the hit's albedo so successive bounces contribute
/// diminishing amounts. Terrain and object texels both *receive* it.
///
/// `seed_offset` is a per-texel hash used to rotate the azimuth of the primary
/// rays and decorrelate the structured golden-ratio pattern across texels,
/// eliminating stripe artifacts when the result is quantized by the toon shader.
///
/// Returns the average bounce radiance ∈ [0, ∞). Division by `n_rays` is
/// already applied — the caller multiplies by `bounce_strength`.
pub fn compute_indirect_bounce(
    origin: Vec3,
    mesh_origin: Vec3,
    normal: Vec3,
    cache: &BakeHeightCache,
    bvh: &TriangleBvh,
    lighting: &BakeLighting,
    shadow_max_dist: f32,
    n_rays: u32,
    steps: u32,
    scenery_albedo: f32,
    bounce_count: u32,
    light_shadow_steps: impl Fn(f32) -> u32,
    seed_offset: u32,
) -> Vec3 {
    if n_rays == 0 || bounce_count == 0 {
        return Vec3::ZERO;
    }
    let sun_dir = lighting.sun_dir;

    // Rotate the entire hemisphere sample set by a per-texel azimuth so adjacent
    // texels don't share the same 8 directions (prevents correlated stripe patterns).
    let azimuth_offset = float01(hash_u32(seed_offset)) * std::f32::consts::TAU;

    // Bounce marching uses the full shadow reach, not the (shorter) AO radius:
    // back-of-hill surfaces need to see sunlit terrain up to 4× further away.
    let step_size = shadow_max_dist / steps as f32;
    let mut accum = Vec3::ZERO;

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
        // A colour, not a scalar: pale sand passes more light down the path
        // than dark rock, per channel (paint-aware albedo, painting Phase 4).
        let mut throughput = Vec3::ONE;

        for b in 0..bounce_count {
            // Nearest terrain hit along the ray, by DDA.
            let mut terrain_hit: Option<(f32, Vec3)> = None;
            let climbing = cur_dir.y >= 0.0;
            for s in 1..=steps {
                let t = s as f32 * step_size;
                let pos = cur_pos + cur_dir * t;
                if climbing && cache.above_terrain(pos.y) {
                    break;
                }
                // Strict > to avoid self-intersection on flat terrain.
                if cache.sample_height(pos.x, pos.z) > pos.y {
                    terrain_hit = Some((t, pos));
                    break;
                }
            }

            // Scenery nearer than the terrain re-emits rather than simply
            // absorbing. It used to `break` here, which left an enclosed
            // interior with no indirect light at all: every bounce ray from
            // inside a building hits the building.
            let bound = terrain_hit.map_or(shadow_max_dist, |(t, _)| t);
            let scenery = bvh.nearest_hit_normal(cur_mesh_pos, cur_dir, bound);

            let (hit_pos, hit_mesh_pos, hit_normal, albedo) = match (scenery, terrain_hit) {
                (Some((t, n)), _) => {
                    // Lift the *terrain* march off the surface the way a
                    // terrain hit does: a wall's base sits at ground level,
                    // where the bilinear heightmap can read as above it and
                    // kill the sun ray outright.
                    let p = cur_mesh_pos + cur_dir * t + n * 0.02;
                    (p + Vec3::Y * 0.5, p, n, Vec3::splat(scenery_albedo))
                }
                (None, Some((_, pos))) => {
                    let h = cache.sample_height(pos.x, pos.z);
                    let (n, _) = cache.sample_normal(pos.x, pos.z);
                    // 0.5 clears the step size so the next bounce cannot
                    // immediately re-hit the ground it left.
                    (
                        Vec3::new(pos.x, h + 0.5, pos.z),
                        Vec3::new(pos.x, h + 0.02, pos.z),
                        n,
                        // What the ground re-emits is what is painted there.
                        cache.albedo_at(pos.x, pos.z),
                    )
                }
                // Escaped to the sky: nothing to re-emit.
                (None, None) => break,
            };

            let vis_q = cast_shadow_ray(
                hit_pos,
                hit_mesh_pos,
                sun_dir,
                cache,
                bvh,
                shadow_max_dist,
                steps,
            );
            // What the hit surface sends back: sun, placed lights and sky,
            // times its albedo. `BakeLighting::reemitted` is shared with the
            // probe gather so the static and dynamic halves of the world
            // agree about how bright a lit wall is.
            //
            // No cur_cos_w here — it cancels with the PDF for cosine-weighted
            // hemisphere sampling.
            accum += lighting.reemitted(hit_mesh_pos, hit_normal, albedo, vis_q, |dir, dist| {
                cast_shadow_ray_segment(
                    hit_pos,
                    hit_mesh_pos,
                    dir,
                    dist,
                    cache,
                    bvh,
                    light_shadow_steps(dist),
                )
            }) * throughput;
            throughput *= albedo;

            // Resample direction at the hit point using a proper per-bounce
            // hash rather than a cyclic seed shift — avoids correlated directions.
            let bounce_seed = hash_u32(
                seed_offset
                    .wrapping_add(i.wrapping_mul(1_000_003))
                    .wrapping_add(b.wrapping_mul(2_654_435_761)),
            );
            let bounce_az = float01(hash_u32(bounce_seed.wrapping_add(1))) * std::f32::consts::TAU;
            let bounce_idx = (float01(bounce_seed) * n_rays as f32) as u32 % n_rays;
            let (t, bt) = build_tangent_frame(hit_normal);
            let (new_dir, new_cos_w) =
                hemisphere_dir_for_seed(bounce_idx, n_rays, hit_normal, t, bt, bounce_az);
            cur_pos = hit_pos;
            cur_mesh_pos = hit_mesh_pos;
            cur_dir = new_dir;
            if new_cos_w <= 0.0 {
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
mod indoor {
    use super::*;
    use crate::game::world::baking::lightmap::BakedLight;
    use crate::game::world::terrain::HeightmapData;

    /// Sun only, no sky and no placed lights, so a test reads off one term.
    fn sun_only(dir: Vec3) -> BakeLighting {
        BakeLighting {
            sun_dir: dir,
            sun_rgb: Vec3::ONE,
            ambient_rgb: Vec3::ZERO,
            lights: Vec::new(),
        }
    }

    /// A room with a doorway: the case where AO carries all the shading,
    /// because the sun is blocked and only terrain re-emits bounce.
    fn room() -> TriangleBvh {
        let (hx, hz, h) = (10.0f32, 6.0f32, 3.0f32);
        let mut tris: Vec<[Vec3; 3]> = Vec::new();
        let mut quad = |a: Vec3, b: Vec3, c: Vec3, d: Vec3| {
            tris.push([a, b, c]);
            tris.push([a, c, d]);
        };
        quad(
            Vec3::new(-hx, h, -hz),
            Vec3::new(hx, h, -hz),
            Vec3::new(hx, h, hz),
            Vec3::new(-hx, h, hz),
        );
        quad(
            Vec3::new(-hx, 0.0, -hz),
            Vec3::new(hx, 0.0, -hz),
            Vec3::new(hx, h, -hz),
            Vec3::new(-hx, h, -hz),
        );
        quad(
            Vec3::new(-hx, 0.0, -hz),
            Vec3::new(-hx, 0.0, hz),
            Vec3::new(-hx, h, hz),
            Vec3::new(-hx, h, -hz),
        );
        quad(
            Vec3::new(hx, 0.0, -hz),
            Vec3::new(hx, 0.0, hz),
            Vec3::new(hx, h, hz),
            Vec3::new(hx, h, -hz),
        );
        quad(
            Vec3::new(-hx, 0.0, hz),
            Vec3::new(-0.75, 0.0, hz),
            Vec3::new(-0.75, h, hz),
            Vec3::new(-hx, h, hz),
        );
        quad(
            Vec3::new(0.75, 0.0, hz),
            Vec3::new(hx, 0.0, hz),
            Vec3::new(hx, h, hz),
            Vec3::new(0.75, h, hz),
        );
        TriangleBvh::build(tris)
    }

    fn ground() -> BakeHeightCache {
        let mut hm = HeightmapData::new((0.25, 0.25));
        for tx in -200..200 {
            for tz in -200..200 {
                hm.set_tile(tx, tz, 0.0);
            }
        }
        BakeHeightCache::extract(&hm, -40.0, -40.0, 40.0, 40.0)
    }

    /// The paint map is what terrain re-emits: a painted tile bounces its
    /// layers' bake albedo (blended), an unloaded tile the default layer's,
    /// and the packed bytes survive the round trip the GPU bake shares.
    #[test]
    fn the_albedo_grid_follows_the_paint() {
        use crate::game::world::terrain::paint::{
            ChunkPaint, PAINT_SIZE, PaintmapData, TerrainLayerData, TerrainPalette, paint_index,
            paint_sample,
        };

        let mut palette = TerrainPalette::default();
        palette.layers[0].bake_albedo = [0.1, 0.1, 0.1];
        let mut red = TerrainLayerData::new("red", "textures/red.png");
        red.bake_albedo = [0.8, 0.0, 0.0];
        palette.add(red);

        // Chunk (0, 0) painted red over local tiles x < 8, half-blended at
        // x == 8, default elsewhere.
        let mut samples = vec![paint_sample(0, 0, 0, 0); PAINT_SIZE * PAINT_SIZE];
        for ly in 0..PAINT_SIZE {
            for lx in 0..8 {
                samples[paint_index(lx, ly)] = paint_sample(0, 1, 255, 0);
            }
            samples[paint_index(8, ly)] = paint_sample(0, 1, 128, 0);
        }
        let mut paint = PaintmapData::default();
        paint.insert(
            (0, 0),
            ChunkPaint {
                samples,
                image: Handle::default(),
            },
        );

        let cache = ground().with_paint(&paint, &palette);
        let a = |wx: f32| cache.albedo_at(wx, 1.0);
        assert!(
            (a(0.5) - Vec3::new(0.8, 0.0, 0.0)).length() < 0.01,
            "painted: {}",
            a(0.5)
        );
        let half = Vec3::new(0.1, 0.1, 0.1).lerp(Vec3::new(0.8, 0.0, 0.0), 128.0 / 255.0);
        assert!(
            (a(8.0 * 0.25 + 0.1) - half).length() < 0.01,
            "blended: {}",
            a(2.1)
        );
        assert!(
            (a(20.0) - Vec3::splat(0.1)).length() < 0.01,
            "unpainted ground is layer 0: {}",
            a(20.0)
        );
        // Beyond every loaded chunk the default sample is also layer 0.
        assert!(
            (a(-20.0) - Vec3::splat(0.1)).length() < 0.01,
            "unloaded: {}",
            a(-20.0)
        );

        // And the bounce picks it up: light landing on red ground comes back
        // red. The receiver faces *down* (an object's underside) so its
        // hemisphere rays actually reach the ground.
        let lighting = sun_only(Vec3::Y);
        let o = Vec3::new(1.0, 1.0, 1.0);
        let bvh = TriangleBvh::build(Vec::new());
        let bounce = compute_indirect_bounce(
            o,
            o,
            Vec3::NEG_Y,
            &cache,
            &bvh,
            &lighting,
            10.0,
            16,
            64,
            0.0,
            1,
            |_| 16,
            3,
        );
        assert!(
            bounce.x > bounce.y * 4.0 && bounce.x > 0.0,
            "bounce off red paint must be red: {bounce}"
        );
    }

    /// Box-blur a square field, standing in for the bake's own blur pass.
    fn blur(field: &[f32], n: usize, r: usize) -> Vec<f32> {
        let mut out = vec![0.0; field.len()];
        for y in 0..n {
            for x in 0..n {
                let (mut sum, mut cnt) = (0.0, 0);
                for yy in y.saturating_sub(r)..(y + r + 1).min(n) {
                    for xx in x.saturating_sub(r)..(x + r + 1).min(n) {
                        sum += field[yy * n + xx];
                        cnt += 1;
                    }
                }
                out[y * n + x] = sum / cnt as f32;
            }
        }
        out
    }

    /// How much Monte-Carlo noise survives the bake's blur, per term, for
    /// the *object* bake — which evaluates AO and bounce per texel, where
    /// the terrain bake evaluates them on a grid `cpu_lowfreq_downsample`
    /// times coarser and interpolates.
    ///
    /// `cargo test --profile perf object_bake_residual_noise -- --ignored --nocapture`
    #[test]
    #[ignore = "measurement, not a correctness check"]
    fn object_bake_residual_noise() {
        let bvh = room();
        let cache = ground();
        let n = 64usize;
        let d = 1.0 / 11.0;
        let (x0, z0) = (-5.0f32, -2.0f32);
        let sun = Vec3::new(0.3, 0.6, 0.74).normalize();

        let at = |i: usize, j: usize| Vec3::new(x0 + i as f32 * d, 0.02, z0 + j as f32 * d);

        let mut ao16 = vec![0.0; n * n];
        let mut ao_ref = vec![0.0; n * n];
        let mut bo8 = vec![0.0; n * n];
        let mut bo_ref = vec![0.0; n * n];
        for j in 0..n {
            for i in 0..n {
                let o = at(i, j);
                let seed = (j * n + i) as u32;
                ao16[j * n + i] = compute_ao(o, o, Vec3::Y, &cache, &bvh, 20.0, 16, 32, seed);
                ao_ref[j * n + i] = compute_ao(o, o, Vec3::Y, &cache, &bvh, 20.0, 512, 32, seed);
                let lighting = sun_only(sun);
                let bounce = |rays: u32| {
                    compute_indirect_bounce(
                        o,
                        o,
                        Vec3::Y,
                        &cache,
                        &bvh,
                        &lighting,
                        80.0,
                        rays,
                        64,
                        0.5,
                        1,
                        |_| 48,
                        seed,
                    )
                    .x
                };
                bo8[j * n + i] = bounce(8);
                bo_ref[j * n + i] = bounce(256);
            }
        }

        let report = |label: &str, got: &[f32], reference: &[f32]| {
            let b = blur(got, n, 4);
            let r = blur(reference, n, 4);
            let mean = b.iter().zip(&r).map(|(a, c)| (a - c).abs()).sum::<f32>() / b.len() as f32;
            let max = b
                .iter()
                .zip(&r)
                .map(|(a, c)| (a - c).abs())
                .fold(0.0f32, f32::max);
            let signal = r.iter().cloned().fold(0.0f32, f32::max)
                - r.iter().cloned().fold(f32::MAX, f32::min);
            println!(
                "{label:<8} after blur: mean err {mean:.4} max {max:.4} | signal range {signal:.4} \
                 | max err is {:.0}% of a toon band (0.2)",
                100.0 * max / 0.2
            );
        };
        report("ao x16", &ao16, &ao_ref);
        report("bounce x8", &bo8, &bo_ref);
    }

    /// Scenery must re-emit bounce light, not just absorb it.
    ///
    /// A sunlit wall is the only indirect light an enclosed interior ever
    /// gets: every bounce ray from inside a building hits the building, and
    /// the path used to end there. Terrain albedo is zeroed in both cases so
    /// only the scenery term can contribute.
    #[test]
    fn a_sunlit_wall_bounces_light_onto_a_shaded_surface() {
        // One wall at x = +5, facing us at the origin; the sun is behind us
        // so the face we can see is the lit one.
        let wall = TriangleBvh::build(vec![
            [
                Vec3::new(5.0, 0.0, -6.0),
                Vec3::new(5.0, 6.0, -6.0),
                Vec3::new(5.0, 0.0, 6.0),
            ],
            [
                Vec3::new(5.0, 6.0, -6.0),
                Vec3::new(5.0, 6.0, 6.0),
                Vec3::new(5.0, 0.0, 6.0),
            ],
        ]);
        // Terrain albedo comes from the cache now; zero it so this test
        // isolates what the *wall* re-emits.
        let cache = ground().with_uniform_albedo(Vec3::ZERO);
        let sun_dir = Vec3::new(-1.0, 0.5, 0.0).normalize();
        let o = Vec3::new(0.0, 1.0, 0.0);
        let n = Vec3::X;

        let lighting = sun_only(sun_dir);
        let bounce = |scenery_albedo: f32| {
            compute_indirect_bounce(
                o,
                o,
                n,
                &cache,
                &wall,
                &lighting,
                80.0,
                16,
                64,
                scenery_albedo,
                1,
                |_| 48,
                7,
            )
            .x
        };
        let absorbed = bounce(0.0);
        let reemitted = bounce(0.5);
        assert_eq!(absorbed, 0.0, "nothing else may contribute to this test");
        assert!(
            reemitted > 0.01,
            "a sunlit wall should bounce light onto a surface facing it, got {reemitted}"
        );
    }

    /// A placed lamp must light the room through the surfaces it falls on,
    /// not only the ones it shines on directly.
    ///
    /// Re-emission used to carry the sun and sky and nothing else, so a
    /// windowless interior lit by lamps had no indirect light at all. The
    /// sun is zeroed here, so the only thing that can produce a non-zero
    /// bounce is the lamp lighting the wall and the wall lighting us.
    #[test]
    fn a_lamp_lights_the_wall_it_faces_and_the_wall_lights_the_room() {
        let wall = TriangleBvh::build(vec![
            [
                Vec3::new(5.0, 0.0, -6.0),
                Vec3::new(5.0, 6.0, -6.0),
                Vec3::new(5.0, 0.0, 6.0),
            ],
            [
                Vec3::new(5.0, 6.0, -6.0),
                Vec3::new(5.0, 6.0, 6.0),
                Vec3::new(5.0, 0.0, 6.0),
            ],
        ]);
        let cache = ground();
        let o = Vec3::new(0.0, 1.0, 0.0);
        let n = Vec3::X;

        // No sun and no sky: the lamp is the only light in the world.
        let dark = BakeLighting {
            sun_dir: Vec3::Y,
            sun_rgb: Vec3::ZERO,
            ambient_rgb: Vec3::ZERO,
            lights: Vec::new(),
        };
        let lamplit = BakeLighting {
            lights: vec![BakedLight::point(
                Vec3::new(3.5, 1.5, 0.0),
                Vec3::ONE,
                4.0,
                8.0,
            )],
            ..dark.clone()
        };

        let bounce = |lighting: &BakeLighting| {
            compute_indirect_bounce(
                o,
                o,
                n,
                &cache,
                &wall,
                lighting,
                80.0,
                16,
                64,
                0.5,
                1,
                |_| 48,
                11,
            )
        };
        assert_eq!(
            bounce(&dark),
            Vec3::ZERO,
            "with no light at all there is nothing to bounce"
        );
        assert!(
            bounce(&lamplit).length() > 0.01,
            "a lamp facing a wall should light the room off it, got {:?}",
            bounce(&lamplit)
        );
    }

    /// A wall lit by nothing but the sky still re-emits, and the lightmap
    /// bounce picks it up.
    ///
    /// Through v11 `compute_indirect_bounce` re-emitted only the sun and the
    /// placed lights, while the probe gather beside it re-emitted the sky as
    /// well. In overcast or shaded conditions — the whole of an interior,
    /// and every north face outdoors — the sky *is* the light, so the two
    /// halves of the world disagreed about how bright a wall is: a dynamic
    /// object picked up fill the static floor underneath it never got. Both
    /// now go through `BakeLighting::reemitted`.
    #[test]
    fn a_wall_bounces_sky_onto_a_surface_the_sun_never_reaches() {
        let wall = TriangleBvh::build(vec![
            [
                Vec3::new(5.0, 0.0, -6.0),
                Vec3::new(5.0, 6.0, -6.0),
                Vec3::new(5.0, 0.0, 6.0),
            ],
            [
                Vec3::new(5.0, 6.0, -6.0),
                Vec3::new(5.0, 6.0, 6.0),
                Vec3::new(5.0, 0.0, 6.0),
            ],
        ]);
        let cache = ground();
        let o = Vec3::new(0.0, 1.0, 0.0);

        // Sky only: no sun, no lamps. Everything that arrives did so by
        // bouncing off something the sky lit.
        let overcast = BakeLighting {
            sun_dir: Vec3::Y,
            sun_rgb: Vec3::ZERO,
            ambient_rgb: Vec3::splat(0.3),
            lights: Vec::new(),
        };
        let bounce = compute_indirect_bounce(
            o,
            o,
            Vec3::X,
            &cache,
            &wall,
            &overcast,
            80.0,
            16,
            64,
            0.5,
            1,
            |_| 48,
            11,
        );
        assert!(
            bounce.min_element() > 0.01,
            "a sky-lit wall should fill the surface facing it, got {bounce:?}"
        );
        // Bounded above by the wall's own outgoing radiance, `ambient x
        // albedo`: a one-bounce gather cannot exceed what it gathers from.
        assert!(bounce.max_element() <= 0.3 * 0.5 + 1e-4, "{bounce:?}");
    }

    /// AO must rotate its sample set per texel, or the blur cannot clean up
    /// after it.
    ///
    /// Sixteen *fixed* directions are a deterministic quadrature: its error
    /// is a smooth function of position, so blurring 81 neighbours leaves it
    /// almost untouched, and what survives is a pattern that follows the
    /// sample directions rather than the geometry. Indoors — where the sun
    /// is blocked and only terrain re-emits bounce, so `ambient x ao` is the
    /// whole of the shading — that bias is comparable to the entire spread
    /// of the field, and the toon quantiser draws its contour wherever the
    /// bias puts it. Decorrelating makes the error zero-mean so the blur
    /// averages it away.
    ///
    /// Measured against a 512-ray reference: ~6x less mean error.
    #[test]
    fn ao_sample_rotation_survives_the_bake_blur() {
        let bvh = room();
        let cache = ground();
        let n = 20usize;
        let d = 1.0 / 11.0;
        let (x0, z0) = (-3.0f32, 1.5f32);

        let mut reference = vec![0.0; n * n];
        let mut fixed = vec![0.0; n * n];
        let mut rotated = vec![0.0; n * n];
        for j in 0..n {
            for i in 0..n {
                let o = Vec3::new(x0 + i as f32 * d, 0.02, z0 + j as f32 * d);
                let seed = (j * n + i) as u32;
                reference[j * n + i] = compute_ao(o, o, Vec3::Y, &cache, &bvh, 20.0, 512, 32, seed);
                // What the bake did before: the same 16 directions everywhere.
                fixed[j * n + i] = compute_ao(o, o, Vec3::Y, &cache, &bvh, 20.0, 16, 32, 0);
                rotated[j * n + i] = compute_ao(o, o, Vec3::Y, &cache, &bvh, 20.0, 16, 32, seed);
            }
        }

        let blurred_reference = blur(&reference, n, 4);
        let mean_error = |f: &[f32]| {
            let b = blur(f, n, 4);
            b.iter()
                .zip(&blurred_reference)
                .map(|(a, r)| (a - r).abs())
                .sum::<f32>()
                / b.len() as f32
        };
        let (fixed_err, rotated_err) = (mean_error(&fixed), mean_error(&rotated));
        assert!(
            rotated_err * 2.0 < fixed_err,
            "rotating the sample set should more than halve the blurred AO \
             error, got {rotated_err:.5} vs {fixed_err:.5}"
        );
    }
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
            compute_ao(origin, mesh_origin, Vec3::Y, &cache, bvh, radius, 16, 32, 0)
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
        let cliff_only = compute_ao(
            origin,
            mesh_origin,
            Vec3::Y,
            &cache,
            &empty,
            radius,
            16,
            32,
            0,
        );
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
            0,
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
/// `cargo test --profile perf terrain_bake_cost_breakdown -- --ignored --nocapture`
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

    fn bench_lighting(sun_dir: Vec3) -> BakeLighting {
        BakeLighting {
            sun_dir,
            sun_rgb: Vec3::ONE,
            ambient_rgb: Vec3::ZERO,
            lights: Vec::new(),
        }
    }

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

    /// An enclosed room: four walls, a ceiling and a doorway.
    fn room_scene() -> (BakeHeightCache, TriangleBvh) {
        let mut hm = HeightmapData::new((0.25, 0.25));
        for tx in -400..400 {
            for tz in -400..400 {
                hm.set_tile(tx, tz, 0.0);
            }
        }
        let cache = BakeHeightCache::extract(&hm, -90.0, -90.0, 90.0, 90.0);

        let (hx, hz, h) = (10.0f32, 6.0f32, 3.0f32);
        let mut tris: Vec<[Vec3; 3]> = Vec::new();
        let mut quad = |a: Vec3, b: Vec3, c: Vec3, d: Vec3| {
            tris.push([a, b, c]);
            tris.push([a, c, d]);
        };
        quad(
            Vec3::new(-hx, h, -hz),
            Vec3::new(hx, h, -hz),
            Vec3::new(hx, h, hz),
            Vec3::new(-hx, h, hz),
        );
        for (a, b) in [
            (Vec3::new(-hx, 0.0, -hz), Vec3::new(hx, 0.0, -hz)),
            (Vec3::new(-hx, 0.0, -hz), Vec3::new(-hx, 0.0, hz)),
            (Vec3::new(hx, 0.0, -hz), Vec3::new(hx, 0.0, hz)),
            (Vec3::new(0.75, 0.0, hz), Vec3::new(hx, 0.0, hz)),
            (Vec3::new(-hx, 0.0, hz), Vec3::new(-0.75, 0.0, hz)),
        ] {
            quad(a, b, b + Vec3::Y * h, a + Vec3::Y * h);
        }
        (cache, TriangleBvh::build(tris))
    }

    /// Per-texel cost *inside* a building, which is where scenery
    /// re-emission changes the arithmetic: a scenery hit used to end the
    /// path immediately, and now casts a sun ray and keeps bouncing.
    ///
    /// `cargo test --profile perf interior_bake_cost -- --ignored --nocapture`
    #[test]
    #[ignore = "timing benchmark, not a correctness check"]
    fn interior_bake_cost() {
        const N: usize = 20_000;
        let (cache, bvh) = room_scene();
        let sun_dir = Vec3::new(0.4, 0.8, 0.3).normalize();
        let (ao_radius, shadow_steps, ao_rays, ao_steps, bounce_rays) =
            (20.0_f32, 64_u32, 16_u32, 32_u32, 8_u32);
        let reach = ao_radius * 4.0;

        // Points spread over the room's floor.
        let pts: Vec<Vec3> = (0..N)
            .map(|i| {
                let u = (i % 137) as f32 / 137.0;
                let v = (i / 137) as f32 / (N as f32 / 137.0);
                Vec3::new(-9.5 + u * 19.0, 0.02, -5.5 + v * 11.0)
            })
            .collect();

        let mut sink = 0.0f32;
        let t = Instant::now();
        for &o in &pts {
            sink += cast_shadow_ray(o, o, sun_dir, &cache, &bvh, reach, shadow_steps);
        }
        let sun = t.elapsed();

        let t = Instant::now();
        for (i, &o) in pts.iter().enumerate() {
            sink += compute_ao(
                o,
                o,
                Vec3::Y,
                &cache,
                &bvh,
                ao_radius,
                ao_rays,
                ao_steps,
                i as u32,
            );
        }
        let ao = t.elapsed();

        let t = Instant::now();
        for (i, &o) in pts.iter().enumerate() {
            sink += compute_indirect_bounce(
                o,
                o,
                Vec3::Y,
                &cache,
                &bvh,
                &bench_lighting(sun_dir),
                reach,
                bounce_rays,
                shadow_steps,
                0.5,
                1,
                |_| 48,
                i as u32,
            )
            .x;
        }
        let bounce = t.elapsed();

        let per = |d: std::time::Duration| d.as_secs_f64() * 1e9 / N as f64;
        println!(
            "interior, per texel: sun {:.0} ns | ao {:.0} ns | bounce {:.0} ns",
            per(sun),
            per(ao),
            per(bounce)
        );
        // The cafe shell's five sub-meshes cover ~528k texels in the real bake.
        let total_us = (per(sun) + per(ao) + per(bounce)) * 528_000.0 / 1e3;
        println!(
            "  x 528k covered texels (the cafe shell) = {:.2}s single-threaded, sink={sink}",
            total_us / 1e6
        );
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
            sink += compute_ao(o, mo, n, &cache, &bvh, ao_radius, ao_rays, ao_steps, 0);
        }
        let ao = t.elapsed();

        // AO against an empty BVH isolates terrain marching from BVH traversal.
        let empty = TriangleBvh::build(Vec::new());
        let t = Instant::now();
        for &(o, mo, n) in &pts {
            sink += compute_ao(o, mo, n, &cache, &empty, ao_radius, ao_rays, ao_steps, 0);
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
                &bench_lighting(sun_dir),
                reach,
                bounce_rays,
                shadow_steps,
                0.5,
                1,
                |_| 48,
                i as u32,
            )
            .x;
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
