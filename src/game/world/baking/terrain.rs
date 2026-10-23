//! Baked lightmap generation for terrain chunks (CPU path).
//!
//! The GPU path in `gpu.rs` computes the same texels with the same lighting
//! model; this is the wasm / fallback implementation.

use bevy::{pbr::ExtendedMaterial, prelude::*};
use rayon::prelude::*;

use super::bvh::TriangleBvh;
use super::lightmap::{BakeLighting, LightmapBuffer, terrain_lightmap_path};
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
#[allow(clippy::too_many_arguments)]
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

        // UV layout: U = x (column), V = y (row).
        // Image data is stored row-major: row ty, column tx.
        let texels: Vec<Vec3> = (0..res * res)
            .into_par_iter()
            .map(|i| {
                let tx = i % res;
                let ty = i / res;

                let u = tx as f32 / (res - 1).max(1) as f32;
                let v = ty as f32 / (res - 1).max(1) as f32;

                let wx = chunk_origin_x + u * chunk_world_size;
                let wz = chunk_origin_z + v * chunk_world_size;
                let surface = Vec3::new(wx, cache.sample_height(wx, wz), wz);
                let (normal, _slope) = cache.sample_normal(wx, wz);

                // Two ray origins: the terrain march starts 0.5 up so the
                // coarse DDA (~1.25-unit steps) can't re-hit the ground it
                // left, while the exact triangle test starts right at the
                // surface so a crate resting on the ground still shadows it.
                let origin = surface + Vec3::Y * 0.5;
                let mesh_origin = surface + normal * 0.02;

                let shadow = raycast::cast_shadow_ray(
                    origin,
                    mesh_origin,
                    sun_dir,
                    &cache,
                    bvh,
                    config.ao_radius * 4.0,
                    config.shadow_steps,
                );
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

                // Per-texel seed used to rotate hemisphere sample azimuths so adjacent
                // texels don't share correlated ray directions → no stripe artifacts.
                let seed_offset: u32 = (tx as u32)
                    .wrapping_mul(1_000_003)
                    .wrapping_add((ty as u32).wrapping_mul(2_654_435_761));

                let ndotl = normal.dot(sun_dir).max(0.0);
                let bounce = raycast::compute_indirect_bounce(
                    origin,
                    mesh_origin,
                    normal,
                    &cache,
                    bvh,
                    sun_dir,
                    config.ao_radius,
                    config.ao_radius * 4.0,
                    config.bounce_rays,
                    config.shadow_steps,
                    config.terrain_albedo,
                    config.bounce_count,
                    seed_offset,
                );

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
        let path = terrain_lightmap_path(&terrain_world.world_path, chunk.chunk_x, chunk.chunk_y);
        if let Err(e) = lightmap.save_exr(&path) {
            error!(
                "Failed to save terrain lightmap for chunk ({}, {}) to {}: {e}",
                chunk.chunk_x,
                chunk.chunk_y,
                path.display()
            );
        }

        let new_handle = images.add(lightmap.into_image());
        if let Some(mat) = materials.get_mut(material_handle.id()) {
            mat.extension.lightmap = new_handle;
        }

        info!(
            "Baked terrain lightmap for chunk ({}, {})",
            chunk.chunk_x, chunk.chunk_y
        );
    }
}
