//! Baked lightmap generation for terrain chunks.

use bevy::{
    asset::RenderAssetUsages, pbr::ExtendedMaterial, prelude::*,
    render::render_resource::TextureFormat,
};
use rayon::prelude::*;
use std::{fs, path::Path};

use super::{BakeConfig, raycast};
use crate::game::world::terrain::{
    HeightmapData, TerrainChunk, material::TerrainMaterial, storage::TerrainWorld,
};

/// Bake lightmaps for all currently loaded terrain chunks.
///
/// For each chunk: generates a `BakeConfig::terrain_resolution × terrain_resolution`
/// `R8Unorm` image where each texel stores the combined shadow + AO factor
/// sampled from the heightmap. Saves the image as PNG to disk and swaps it
/// into the chunk's material.
pub fn bake_terrain_lightmaps(
    heightmap: &HeightmapData,
    terrain_world: &TerrainWorld,
    config: &BakeConfig,
    chunk_query: &Query<(
        &TerrainChunk,
        &MeshMaterial3d<ExtendedMaterial<StandardMaterial, TerrainMaterial>>,
    )>,
    materials: &mut Assets<ExtendedMaterial<StandardMaterial, TerrainMaterial>>,
    images: &mut Assets<Image>,
    // Sun direction used for shadow rays (points FROM surface TOWARD sun).
    sun_dir: Vec3,
) {
    let res = config.terrain_resolution as usize;
    let chunk_world_size = crate::game::world::terrain::constants::CHUNK_SIZE as f32
        * crate::game::world::terrain::TILE_SIZE.0;

    for (chunk, material_handle) in chunk_query.iter() {
        let chunk_origin_x = chunk.chunk_x as f32 * chunk_world_size;
        let chunk_origin_z = chunk.chunk_y as f32 * chunk_world_size;

        // UV layout: U = x (column), V = y (row).
        // Image data is stored row-major: row ty, column tx.
        let pixels: Vec<u8> = (0..res * res)
            .into_par_iter()
            .map(|i| {
                let tx = i % res;
                let ty = i / res;

                let u = tx as f32 / (res - 1).max(1) as f32;
                let v = ty as f32 / (res - 1).max(1) as f32;

                let wx = chunk_origin_x + u * chunk_world_size;
                let wz = chunk_origin_z + v * chunk_world_size;
                let wy = heightmap.sample_height(wx, wz).unwrap_or(0.0) + 0.05;

                let origin = Vec3::new(wx, wy, wz);
                let (normal, _slope) = heightmap.sample_normal(wx, wz).unwrap_or((Vec3::Y, 0.0));

                let shadow = raycast::cast_shadow_ray(
                    origin,
                    sun_dir,
                    heightmap,
                    config.ao_radius * 4.0,
                    config.shadow_steps,
                );
                let ao = raycast::compute_ao(
                    origin,
                    normal,
                    heightmap,
                    config.ao_radius,
                    config.ao_rays,
                    config.shadow_steps,
                );

                let combined = (shadow * 0.5 + ao * 0.5).clamp(0.0, 1.0);
                (combined * 255.0) as u8
            })
            .collect();

        // Save PNG to disk.
        let save_result =
            save_lightmap_png(&pixels, res, terrain_world, chunk.chunk_x, chunk.chunk_y);
        if let Err(e) = save_result {
            error!(
                "Failed to save terrain lightmap for chunk ({}, {}): {e}",
                chunk.chunk_x, chunk.chunk_y
            );
            continue;
        }

        // Insert into Assets<Image> and swap into the material.
        let image = Image::new(
            bevy::render::render_resource::Extent3d {
                width: res as u32,
                height: res as u32,
                depth_or_array_layers: 1,
            },
            bevy::render::render_resource::TextureDimension::D2,
            pixels,
            TextureFormat::R8Unorm,
            RenderAssetUsages::RENDER_WORLD,
        );
        let new_handle = images.add(image);

        if let Some(mat) = materials.get_mut(material_handle.id()) {
            mat.extension.lightmap = new_handle;
        }

        info!(
            "Baked terrain lightmap for chunk ({}, {})",
            chunk.chunk_x, chunk.chunk_y
        );
    }
}

fn save_lightmap_png(
    pixels: &[u8],
    res: usize,
    terrain_world: &TerrainWorld,
    cx: i32,
    cy: i32,
) -> Result<(), Box<dyn std::error::Error>> {
    let chunks_dir = terrain_world.world_path.join("chunks");
    fs::create_dir_all(&chunks_dir)?;
    let path = lightmap_path(&terrain_world.world_path, cx, cy);

    // Use the `image` crate (already a dependency) to encode as PNG.
    let img = image::GrayImage::from_raw(res as u32, res as u32, pixels.to_vec())
        .ok_or("Failed to build GrayImage")?;
    img.save(&path)?;

    Ok(())
}

/// Path at which a terrain chunk lightmap PNG is stored.
pub fn lightmap_path(world_path: &Path, cx: i32, cy: i32) -> std::path::PathBuf {
    world_path
        .join("chunks")
        .join(format!("chunk_{}_{}_lightmap.png", cx, cy))
}
