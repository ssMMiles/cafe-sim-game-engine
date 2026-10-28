//! Baked lightmap generation for terrain chunks (CPU path).
//!
//! The GPU path in `gpu.rs` computes the same texels with the same lighting
//! model; this is the wasm / fallback implementation.

use bevy::{pbr::ExtendedMaterial, prelude::*};
use rayon::prelude::*;

use super::bvh::TriangleBvh;
use super::lightmap::{BakeLighting, LightmapBuffer, store_lightmap, terrain_lightmap_location};
use super::{BakeConfig, BakeRegion, raycast};
use crate::game::world::{
    objects::CHUNK_WORLD_SIZE,
    terrain::{HeightmapData, TerrainChunk, material::TerrainMaterial, storage::TerrainWorld},
};

/// Bake lightmaps for the loaded terrain chunks inside `region`.
///
/// For each chunk: generates a `BakeConfig::terrain_resolution²` HDR lightmap
/// of outgoing radiance (see `lightmap.rs` for the model), saves it as EXR
/// and swaps it into the chunk's material.
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
    let res = config.terrain_resolution as usize;
    // The mesh's lightmap UVs span exactly the chunk footprint, so texels
    // must be placed over the same (CHUNK_SIZE - 1) tiles the mesh covers.
    let chunk_world_size = CHUNK_WORLD_SIZE;
    // Maximum horizontal distance any ray can travel.
    let ray_reach = config.ao_radius * 4.0;
    let sun_dir = lighting.sun_dir;

    for (chunk, material_handle) in chunk_query.iter() {
        if !region.contains_chunk(chunk.chunk_x, chunk.chunk_y) {
            continue;
        }
        let chunk_origin_x = chunk.chunk_x as f32 * chunk_world_size;
        let chunk_origin_z = chunk.chunk_y as f32 * chunk_world_size;

        // Extract a flat heightmap cache covering the chunk + full ray reach in
        // every direction. This replaces per-step HashMap lookups with direct
        // Vec indexing, giving a large speedup in the parallel hot loop.
        let cache = raycast::BakeHeightCache::extract(
            heightmap,
            chunk_origin_x - ray_reach,
            chunk_origin_z - ray_reach,
            chunk_origin_x + chunk_world_size + ray_reach,
            chunk_origin_z + chunk_world_size + ray_reach,
        );

        // Geometry at a texel coordinate (which may sit fractionally past
        // the last texel, for the coarse grid's outer edge).
        let surface_at = |fx: f32, fy: f32| {
            let u = fx / (res - 1).max(1) as f32;
            let v = fy / (res - 1).max(1) as f32;
            let wx = chunk_origin_x + u * chunk_world_size;
            let wz = chunk_origin_z + v * chunk_world_size;
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
        let cg = (res - 1) / step + 2;
        let low_freq: Vec<(f32, f32)> = (0..cg * cg)
            .into_par_iter()
            .map(|i| {
                let gx = i % cg;
                let gy = i / cg;
                let (origin, mesh_origin, normal) =
                    surface_at((gx * step) as f32, (gy * step) as f32);

                let ao = raycast::compute_ao(
                    origin,
                    mesh_origin,
                    normal,
                    &cache,
                    bvh,
                    config.ao_radius,
                    config.ao_rays,
                    config.ao_steps,
                );

                // Per-sample seed rotating hemisphere azimuths so neighbours
                // don't share correlated ray directions → no stripes.
                let seed_offset: u32 = (gx as u32)
                    .wrapping_mul(1_000_003)
                    .wrapping_add((gy as u32).wrapping_mul(2_654_435_761));
                let bounce = raycast::compute_indirect_bounce(
                    origin,
                    mesh_origin,
                    normal,
                    &cache,
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
        let texels: Vec<Vec3> = (0..res * res)
            .into_par_iter()
            .map(|i| {
                let tx = i % res;
                let ty = i / res;
                let (origin, mesh_origin, normal) = surface_at(tx as f32, ty as f32);

                let (ao, bounce) = sample_low_freq(&low_freq, cg, tx, ty, step);

                let shadow = raycast::cast_shadow_ray(
                    origin,
                    mesh_origin,
                    sun_dir,
                    &cache,
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
                        &cache,
                        bvh,
                        config.light_shadow_steps(dist),
                    )
                });

                radiance
            })
            .collect();

        // Blur the raw lightmap to eliminate per-texel Monte Carlo variance before
        // it reaches the toon shader's band quantizer.
        let lightmap = LightmapBuffer { res, texels }.blur(config.blur_radius);

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
