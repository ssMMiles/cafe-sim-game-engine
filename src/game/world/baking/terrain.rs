//! Baked lightmap generation for terrain chunks (CPU path).
//!
//! The GPU path in `gpu.rs` computes the same texels with the same lighting
//! model; this is the wasm / fallback implementation.

use bevy::{pbr::ExtendedMaterial, prelude::*};
use rayon::prelude::*;

use super::bvh::TriangleBvh;
use super::lightmap::{
    BakeLighting, ChunkBakeGrid, LightmapBuffer, store_lightmap, terrain_lightmap_location,
};
use super::{BakeConfig, BakeRegion, raycast};
use crate::game::world::{
    objects::CHUNK_WORLD_SIZE,
    terrain::{HeightmapData, TerrainChunk, material::TerrainMaterial, storage::TerrainWorld},
};

/// Bake lightmaps for the loaded terrain chunks inside `region`.
///
/// For each chunk: generates a `BakeConfig::terrain_resolution²` HDR lightmap
/// of outgoing radiance (see `lightmap.rs` for the model; `bake_chunk` for
/// the per-chunk work), saves it as EXR and swaps it into the chunk's
/// material.
pub fn bake_terrain_lightmaps(
    heightmap: &HeightmapData,
    terrain_world: &TerrainWorld,
    config: &BakeConfig,
    region: &BakeRegion,
    chunk_query: &Query<(
        &TerrainChunk,
        &MeshMaterial3d<ExtendedMaterial<StandardMaterial, TerrainMaterial>>,
    )>,
    materials: &mut Assets<ExtendedMaterial<StandardMaterial, TerrainMaterial>>,
    images: &mut Assets<Image>,
    lighting: &BakeLighting,
    bvh: &TriangleBvh,
) {
    let grid = ChunkBakeGrid::new(config);
    // Maximum horizontal distance any ray can travel.
    let ray_reach = config.ao_radius * 4.0;

    for (chunk, material_handle) in chunk_query.iter() {
        if !region.contains_chunk(chunk.chunk_x, chunk.chunk_y) {
            continue;
        }
        // A flat heightmap cache covering the chunk + full ray reach in
        // every direction. This replaces per-step HashMap lookups with direct
        // Vec indexing, giving a large speedup in the parallel hot loop.
        let cache = extract_bake_cache(heightmap, grid, chunk.chunk_x, chunk.chunk_y, ray_reach);

        let lightmap = bake_chunk(
            &cache,
            grid,
            chunk.chunk_x,
            chunk.chunk_y,
            config,
            lighting,
            bvh,
        );

        // A failed write is logged but doesn't stop the live material from
        // picking up the result — the bake is still useful for this session.
        let location = terrain_lightmap_location(terrain_world, chunk.chunk_x, chunk.chunk_y);
        if let Err(e) = store_lightmap(&location, &lightmap) {
            error!(
                "Failed to save terrain lightmap for chunk ({}, {}): {e}",
                chunk.chunk_x, chunk.chunk_y,
            );
        }

        let new_handle = images.add(lightmap.into_image());
        if let Some(mut mat) = materials.get_mut(material_handle.id()) {
            mat.extension.lightmap = new_handle;
        }

        info!(
            "Baked terrain lightmap for chunk ({}, {})",
            chunk.chunk_x, chunk.chunk_y
        );
    }
}

/// World-space origin of chunk `(chunk_x, chunk_y)`. The mesh's lightmap UVs
/// span exactly the chunk footprint, so texels are placed over the same
/// `CHUNK_SIZE - 1` tiles the mesh covers.
pub fn chunk_origin(chunk_x: i32, chunk_y: i32) -> (f32, f32) {
    (
        chunk_x as f32 * CHUNK_WORLD_SIZE,
        chunk_y as f32 * CHUNK_WORLD_SIZE,
    )
}

/// Extract the heightmap cache a chunk bake needs: the padded texel grid
/// (chunk plus bake margin) grown by `ray_reach` in every direction, so no
/// texel's rays run off the cached terrain.
pub fn extract_bake_cache(
    heightmap: &HeightmapData,
    grid: ChunkBakeGrid,
    chunk_x: i32,
    chunk_y: i32,
    ray_reach: f32,
) -> raycast::BakeHeightCache {
    let (ox, oz) = chunk_origin(chunk_x, chunk_y);
    let reach = ray_reach + grid.margin as f32 * CHUNK_WORLD_SIZE / (grid.res - 1).max(1) as f32;
    raycast::BakeHeightCache::extract(
        heightmap,
        ox - reach,
        oz - reach,
        ox + CHUNK_WORLD_SIZE + reach,
        oz + CHUNK_WORLD_SIZE + reach,
    )
}

/// Bake one chunk's lightmap on `grid` from a heightmap cache that covers
/// the chunk plus the full ray reach. Returns the finished `grid.res²`
/// buffer: blurred on the padded grid, then cropped (see [`ChunkBakeGrid`]).
pub fn bake_chunk(
    cache: &raycast::BakeHeightCache,
    grid: ChunkBakeGrid,
    chunk_x: i32,
    chunk_y: i32,
    config: &BakeConfig,
    lighting: &BakeLighting,
    bvh: &TriangleBvh,
) -> LightmapBuffer {
    let sun_dir = lighting.sun_dir;
    let padded = grid.padded();

    // Geometry at a padded texel coordinate (which may sit fractionally past
    // the last texel, for the coarse grid's outer edge).
    let surface_at = |px: f32, py: f32| {
        let Vec2 { x: wx, y: wz } = grid.world_xz(chunk_x, chunk_y, px, py);
        let surface = Vec3::new(wx, cache.sample_height(wx, wz), wz);
        let (normal, _slope) = cache.sample_normal(wx, wz);
        // Two ray origins: the terrain march starts 0.5 up so the coarse
        // DDA (~1.25-unit steps) can't re-hit the ground it left, while
        // the exact triangle test starts right at the surface so a crate
        // resting on the ground still shadows it.
        (surface + Vec3::Y * 0.5, surface + normal * 0.02, normal)
    };

    // ── Low-frequency pass: ambient occlusion and indirect bounce ──
    //
    // These two are ~95% of the CPU bake's ray cost and both vary over
    // metres, not texels: AO integrates a 20 m hemisphere and bounce a
    // 80 m one. Sampling them every texel buys detail that the
    // `blur_radius` box blur then immediately destroys — at the default
    // 16 texels/m and radius 4, the finished lightmap is already smeared
    // over 25 cm. So they are evaluated on a grid `downsample` times
    // coarser and bilinearly interpolated, which costs `downsample²`
    // fewer rays for a result the blur makes near-indistinguishable.
    //
    // The sun shadow and local-light shadows stay at full resolution:
    // those are the sharp, high-frequency terms the toon bands key off.
    //
    // Set `cpu_lowfreq_downsample` to 1 for texel-exact parity with the
    // GPU path, which does not do this.
    let step = config.cpu_lowfreq_downsample.max(1) as usize;
    // +2 so the bilinear tap at the last texel always has a right/bottom
    // neighbour to read.
    let cg = (padded - 1) / step + 2;
    let low_freq: Vec<(f32, f32)> = (0..cg * cg)
        .into_par_iter()
        .map(|i| {
            let gx = i % cg;
            let gy = i / cg;
            let (origin, mesh_origin, normal) = surface_at((gx * step) as f32, (gy * step) as f32);

            let ao = raycast::compute_ao(
                origin,
                mesh_origin,
                normal,
                cache,
                bvh,
                config.ao_radius,
                config.ao_rays,
                config.ao_steps,
            );

            // Per-sample seed rotating hemisphere azimuths so neighbours
            // don't share correlated ray directions → no stripes.
            let seed_offset = grid.texel_seed(chunk_x, chunk_y, gx * step, gy * step);
            let bounce = raycast::compute_indirect_bounce(
                origin,
                mesh_origin,
                normal,
                cache,
                bvh,
                sun_dir,
                config.ao_radius * 4.0,
                config.bounce_rays,
                config.shadow_steps,
                config.terrain_albedo,
                config.bounce_count,
                seed_offset,
            );
            (ao, bounce)
        })
        .collect();

    // ── Full-resolution pass ──
    // UV layout: U = x (column), V = y (row).
    // Image data is stored row-major: row ty, column tx.
    let texels: Vec<Vec3> = (0..padded * padded)
        .into_par_iter()
        .map(|i| {
            let tx = i % padded;
            let ty = i / padded;
            let (origin, mesh_origin, normal) = surface_at(tx as f32, ty as f32);

            let (ao, bounce) = sample_low_freq(&low_freq, cg, tx, ty, step);

            let shadow = raycast::cast_shadow_ray(
                origin,
                mesh_origin,
                sun_dir,
                cache,
                bvh,
                config.ao_radius * 4.0,
                config.shadow_steps,
            );

            let ndotl = normal.dot(sun_dir).max(0.0);
            let mut radiance =
                lighting.sun_and_ambient(ndotl, shadow, ao, config.bounce_strength * bounce);

            // Placed point/spot lights, each occluded only by geometry
            // between this texel and the light itself.
            radiance += lighting.local_lights(mesh_origin, normal, |dir, dist| {
                raycast::cast_shadow_ray_segment(
                    origin,
                    mesh_origin,
                    dir,
                    dist,
                    cache,
                    bvh,
                    config.light_shadow_steps(dist),
                )
            });

            radiance
        })
        .collect();

    // Blur the raw lightmap to eliminate per-texel Monte Carlo variance before
    // it reaches the toon shader's band quantizer, then drop the margin.
    LightmapBuffer {
        res: padded,
        texels,
    }
    .blur(config.blur_radius)
    .crop(grid.margin)
}

/// Bilinearly read the coarse AO/bounce grid at full-resolution texel
/// `(tx, ty)`. `grid` is `cg × cg` samples taken every `step` texels.
fn sample_low_freq(
    grid: &[(f32, f32)],
    cg: usize,
    tx: usize,
    ty: usize,
    step: usize,
) -> (f32, f32) {
    let fx = tx as f32 / step as f32;
    let fy = ty as f32 / step as f32;
    let x0 = (fx.floor() as usize).min(cg - 1);
    let y0 = (fy.floor() as usize).min(cg - 1);
    let x1 = (x0 + 1).min(cg - 1);
    let y1 = (y0 + 1).min(cg - 1);
    let dx = fx - x0 as f32;
    let dy = fy - y0 as f32;

    let at = |x: usize, y: usize| grid[y * cg + x];
    let (a00, b00) = at(x0, y0);
    let (a10, b10) = at(x1, y0);
    let (a01, b01) = at(x0, y1);
    let (a11, b11) = at(x1, y1);

    let lerp = |a: f32, b: f32, t: f32| a + (b - a) * t;
    (
        lerp(lerp(a00, a10, dx), lerp(a01, a11, dx), dy),
        lerp(lerp(b00, b10, dx), lerp(b01, b11, dx), dy),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a coarse grid whose AO channel is a known linear ramp in x and
    /// bounce a ramp in y, so interpolation is checkable in closed form.
    fn ramp_grid(cg: usize, step: usize) -> Vec<(f32, f32)> {
        (0..cg * cg)
            .map(|i| {
                let gx = (i % cg) * step;
                let gy = (i / cg) * step;
                (gx as f32, gy as f32)
            })
            .collect()
    }

    /// Two chunks must bake the same texels along their shared edge.
    ///
    /// Without the bake margin the blur's clamped window biased each side
    /// towards its own edge texel, and in a steep gradient the toon bands
    /// jumped at the seam — the "line break" in the shadows around spawn,
    /// which sits on the corner of four chunks.
    #[test]
    fn neighbouring_chunks_agree_along_their_shared_edge() {
        use crate::game::world::terrain::{HeightmapData, storage::EnvironmentData};

        // Small, cheap bake: AO/bounce sampled per texel so the coarse grid
        // can't introduce its own (interpolation-only) seam differences.
        let config = BakeConfig {
            terrain_resolution: 32,
            blur_radius: 2,
            ao_radius: 5.0,
            ao_rays: 4,
            ao_steps: 8,
            shadow_steps: 16,
            bounce_rays: 2,
            cpu_lowfreq_downsample: 1,
            ..Default::default()
        };
        let reach = config.ao_radius * 4.0;

        // Rolling hills straddling x = 0, so the seam crosses slopes and
        // shadows from a low sun.
        let mut hm = HeightmapData::new((0.25, 0.25));
        let span_tiles = ((CHUNK_WORLD_SIZE + reach) / 0.25).ceil() as i32 + 2;
        for tx in -span_tiles..=span_tiles {
            for tz in -(reach / 0.25).ceil() as i32 - 2..=span_tiles {
                let (x, z) = (tx as f32 * 0.25, tz as f32 * 0.25);
                hm.set_tile(tx, tz, 6.0 * (x * 0.15).sin() * (z * 0.09).cos());
            }
        }
        let sun_dir = Vec3::new(0.7, 0.35, 0.2).normalize();
        let lighting = BakeLighting::new(&EnvironmentData::default(), sun_dir, Vec::new());
        let bvh = TriangleBvh::build(Vec::new());

        let bake = |chunk_x: i32, grid: ChunkBakeGrid| {
            let cache = extract_bake_cache(&hm, grid, chunk_x, 0, reach);
            bake_chunk(&cache, grid, chunk_x, 0, &config, &lighting, &bvh)
        };
        let res = config.terrain_resolution as usize;
        let seam_gap = |left: &LightmapBuffer, right: &LightmapBuffer| {
            (0..res)
                .map(|y| {
                    (left.texels[y * res + res - 1] - right.texels[y * res])
                        .abs()
                        .max_element()
                })
                .fold(0.0_f32, f32::max)
        };

        let grid = ChunkBakeGrid::new(&config);
        let gap = seam_gap(&bake(-1, grid), &bake(0, grid));
        assert!(gap < 1e-5, "seam texels differ by {gap}");

        // The margin is what buys that: baked bare, the same chunks disagree.
        let bare = ChunkBakeGrid { res, margin: 0 };
        let bare_gap = seam_gap(&bake(-1, bare), &bake(0, bare));
        assert!(
            bare_gap > 1e-3,
            "expected a visible seam without the margin, got {bare_gap}"
        );
    }

    /// `downsample = 1` must be exactly per-texel sampling — the escape hatch
    /// for anyone who wants parity with the GPU path.
    #[test]
    fn downsample_of_one_is_exact_per_texel_sampling() {
        let res = 16;
        let step = 1;
        let cg = (res - 1) / step + 2;
        let grid = ramp_grid(cg, step);
        for ty in 0..res {
            for tx in 0..res {
                let (ao, bounce) = sample_low_freq(&grid, cg, tx, ty, step);
                assert_eq!(ao, tx as f32, "ao at ({tx},{ty})");
                assert_eq!(bounce, ty as f32, "bounce at ({tx},{ty})");
            }
        }
    }

    /// A linear field must survive bilinear interpolation untouched, so
    /// downsampling adds no bias — only loses detail finer than the grid.
    #[test]
    fn a_linear_field_is_reproduced_exactly_when_downsampled() {
        let res = 64;
        for step in [2usize, 4, 8] {
            let cg = (res - 1) / step + 2;
            let grid = ramp_grid(cg, step);
            for ty in (0..res).step_by(3) {
                for tx in (0..res).step_by(3) {
                    let (ao, bounce) = sample_low_freq(&grid, cg, tx, ty, step);
                    assert!(
                        (ao - tx as f32).abs() < 1e-3,
                        "step {step} ao at {tx}: {ao}"
                    );
                    assert!(
                        (bounce - ty as f32).abs() < 1e-3,
                        "step {step} bounce at {ty}: {bounce}"
                    );
                }
            }
        }
    }

    /// Every full-resolution texel must have four in-range taps; an
    /// off-by-one in `cg` would panic mid-bake on the last row/column.
    #[test]
    fn every_texel_has_in_range_taps() {
        for res in [8usize, 16, 17, 100, 1024] {
            for step in [1usize, 2, 3, 4, 8] {
                let cg = (res - 1) / step + 2;
                let grid = vec![(0.5f32, 0.25f32); cg * cg];
                // Corners and edges are where the indexing breaks.
                for &(tx, ty) in &[(0, 0), (res - 1, 0), (0, res - 1), (res - 1, res - 1)] {
                    let (ao, b) = sample_low_freq(&grid, cg, tx, ty, step);
                    assert_eq!((ao, b), (0.5, 0.25), "res {res} step {step} at ({tx},{ty})");
                }
            }
        }
    }
}
