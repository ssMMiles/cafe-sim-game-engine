//! Baked lightmap generation for placed mesh instances.

use bevy::{
    asset::RenderAssetUsages,
    math::{Mat4, Vec2, Vec3, Vec4Swizzles},
    mesh::{Indices, Mesh, VertexAttributeValues},
    pbr::ExtendedMaterial,
    prelude::*,
    render::render_resource::TextureFormat,
};
use std::{collections::HashSet, fs, path::Path};

use super::{BakeConfig, raycast};
use super::raycast::BakeHeightCache;
use crate::game::{
    gfx::toon_material::ToonMaterial,
    world::{
        objects::PlacedObject,
        terrain::{HeightmapData, storage::TerrainWorld},
    },
};

/// Bake lightmaps for all placed objects currently in the world.
///
/// For each object this function:
/// 1. Generates UV2 if the mesh doesn't already have it (via xatlas).
/// 2. Rasterizes each triangle in UV2 space, interpolating world-space
///    position and normal from barycentric coords.
/// 3. Casts shadow + AO rays from each texel's world position.
/// 4. Saves the result as a PNG and applies it to the object's material.
#[allow(clippy::too_many_arguments)]
pub fn bake_object_lightmaps(
    heightmap: &HeightmapData,
    terrain_world: &TerrainWorld,
    config: &BakeConfig,
    placed_query: &Query<(Entity, &PlacedObject, &GlobalTransform)>,
    mesh_query: &Query<(
        &Mesh3d,
        &MeshMaterial3d<ExtendedMaterial<StandardMaterial, ToonMaterial>>,
    )>,
    children_query: &Query<&Children>,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<ExtendedMaterial<StandardMaterial, ToonMaterial>>,
    images: &mut Assets<Image>,
    baked_uv_cache: &mut HashSet<bevy::asset::AssetId<Mesh>>,
    sun_dir: Vec3,
) {
    let res = config.mesh_resolution as usize;

    for (root_entity, placed_obj, global_transform) in placed_query.iter() {
        let world_mat = global_transform.to_matrix();

        // Collect all mesh+material pairs in the scene hierarchy.
        let mesh_entities = collect_mesh_entities(root_entity, mesh_query, children_query);

        if mesh_entities.is_empty() {
            continue;
        }

        // Build one lightmap image per placed object instance (all sub-meshes
        // contribute into the same atlas; we use the object id as filename).
        let mut pixels = vec![0u8; res * res];
        // Track which texels have been written so we don't overwrite with
        // later (possibly worse-sampled) triangles.
        let mut written = vec![false; res * res];

        // Compute the world-space bounding box across all sub-meshes so we can
        // build a tight heightmap cache covering the entire object + ray reach.
        let ray_reach = config.ao_radius * 4.0;
        let mut bbox_min = Vec3::splat(f32::MAX);
        let mut bbox_max = Vec3::splat(f32::MIN);
        let mut have_bounds = false;

        for (mesh_handle, _) in &mesh_entities {
            if let Some(mesh) = meshes.get(mesh_handle.id()) {
                let (mn, mx) = world_bounds(mesh, &world_mat);
                bbox_min = bbox_min.min(mn);
                bbox_max = bbox_max.max(mx);
                have_bounds = true;
            }
        }

        // Fall back to a generous region centred on the object's world position
        // if we couldn't derive bounds from vertices.
        if !have_bounds {
            let centre = world_mat.w_axis.truncate();
            bbox_min = centre - Vec3::splat(ray_reach * 2.0);
            bbox_max = centre + Vec3::splat(ray_reach * 2.0);
        }

        let cache = BakeHeightCache::extract(
            heightmap,
            bbox_min.x - ray_reach,
            bbox_min.z - ray_reach,
            bbox_max.x + ray_reach,
            bbox_max.z + ray_reach,
        );

        for (mesh_handle, _mat_handle) in &mesh_entities {
            // Ensure the mesh has UV2; generate if absent.
            let mesh_id = mesh_handle.id();
            let has_uv2 = meshes
                .get(mesh_id)
                .map(|m| m.attribute(Mesh::ATTRIBUTE_UV_1).is_some())
                .unwrap_or(false);

            if !has_uv2 && !baked_uv_cache.contains(&mesh_id) {
                if let Some(mesh) = meshes.get_mut(mesh_id) {
                    super::uv_unwrap::generate_lightmap_uvs(mesh);
                }
                baked_uv_cache.insert(mesh_id);
            }

            let Some(mesh) = meshes.get(mesh_id) else {
                continue;
            };

            rasterize_mesh_lightmap(
                mesh,
                &world_mat,
                &cache,
                config,
                sun_dir,
                &mut pixels,
                &mut written,
                res,
            );
        }

        // Simple 3×3 max dilation pass to fill UV seam boundary texels.
        dilate_lightmap(&mut pixels, res);

        // Save to disk.
        let save_result = save_object_lightmap_png(&pixels, res, terrain_world, placed_obj.id);
        if let Err(e) = save_result {
            error!(
                "Failed to save object lightmap for id={}: {e}",
                placed_obj.id
            );
            continue;
        }

        // Create a GPU image and apply to all sub-mesh materials.
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

        for (_mesh_handle, mat_handle) in &mesh_entities {
            if let Some(mat) = materials.get_mut(mat_handle.id()) {
                mat.extension.lightmap = new_handle.clone();
            }
        }

        info!(
            "Baked object lightmap for placed object id={}",
            placed_obj.id
        );
    }
}

// ============================================================================
// Helpers
// ============================================================================

/// Collect all (Mesh3d, MeshMaterial3d) pairs from an entity's subtree.
fn collect_mesh_entities<'a>(
    root: Entity,
    mesh_query: &'a Query<(
        &Mesh3d,
        &MeshMaterial3d<ExtendedMaterial<StandardMaterial, ToonMaterial>>,
    )>,
    children_query: &Query<&Children>,
) -> Vec<(
    Handle<Mesh>,
    Handle<ExtendedMaterial<StandardMaterial, ToonMaterial>>,
)> {
    let mut result = Vec::new();
    collect_recursive(root, mesh_query, children_query, &mut result);
    result
}

fn collect_recursive(
    entity: Entity,
    mesh_query: &Query<(
        &Mesh3d,
        &MeshMaterial3d<ExtendedMaterial<StandardMaterial, ToonMaterial>>,
    )>,
    children_query: &Query<&Children>,
    out: &mut Vec<(
        Handle<Mesh>,
        Handle<ExtendedMaterial<StandardMaterial, ToonMaterial>>,
    )>,
) {
    if let Ok((mesh_handle, mat_handle)) = mesh_query.get(entity) {
        out.push((mesh_handle.0.clone(), mat_handle.0.clone()));
    }
    if let Ok(children) = children_query.get(entity) {
        // Collect child entities first to avoid borrow conflicts in recursion.
        let child_entities: Vec<Entity> = children.to_vec();
        for child in child_entities {
            collect_recursive(child, mesh_query, children_query, out);
        }
    }
}

/// Compute the world-space axis-aligned bounding box of a mesh's vertices
/// transformed by `world_mat`. Returns `(min, max)`.
fn world_bounds(mesh: &Mesh, world_mat: &Mat4) -> (Vec3, Vec3) {
    let positions: &[[f32; 3]] = match mesh.attribute(Mesh::ATTRIBUTE_POSITION) {
        Some(VertexAttributeValues::Float32x3(v)) => v,
        _ => return (Vec3::ZERO, Vec3::ZERO),
    };

    let mut mn = Vec3::splat(f32::MAX);
    let mut mx = Vec3::splat(f32::MIN);

    for pos in positions {
        let wp = (*world_mat * Vec3::from(*pos).extend(1.0)).xyz();
        mn = mn.min(wp);
        mx = mx.max(wp);
    }

    (mn, mx)
}

/// Rasterize a mesh's triangles in UV2 space and accumulate baked values.
#[allow(clippy::too_many_arguments)]
fn rasterize_mesh_lightmap(
    mesh: &Mesh,
    world_mat: &Mat4,
    cache: &BakeHeightCache,
    config: &BakeConfig,
    sun_dir: Vec3,
    pixels: &mut [u8],
    written: &mut [bool],
    res: usize,
) {
    let positions: &[[f32; 3]] = match mesh.attribute(Mesh::ATTRIBUTE_POSITION) {
        Some(VertexAttributeValues::Float32x3(v)) => v,
        _ => return,
    };
    let normals: &[[f32; 3]] = match mesh.attribute(Mesh::ATTRIBUTE_NORMAL) {
        Some(VertexAttributeValues::Float32x3(v)) => v,
        _ => return,
    };
    let uv1s: &[[f32; 2]] = match mesh.attribute(Mesh::ATTRIBUTE_UV_1) {
        Some(VertexAttributeValues::Float32x2(v)) => v,
        _ => return, // no UV2 — skip (shouldn't happen after unwrap)
    };
    let indices: Vec<u32> = match mesh.indices() {
        Some(Indices::U32(v)) => v.clone(),
        Some(Indices::U16(v)) => v.iter().map(|&i| i as u32).collect(),
        None => return,
    };

    let res_f = res as f32;

    // Normal matrix is constant for this world transform — compute it once
    // outside the triangle loop rather than once per triangle.
    let norm_mat = world_mat.inverse().transpose();

    for tri in indices.chunks_exact(3) {
        let (i0, i1, i2) = (tri[0] as usize, tri[1] as usize, tri[2] as usize);

        // World-space positions (transform by the instance matrix).
        let wp0 = (*world_mat * Vec3::from(positions[i0]).extend(1.0)).xyz();
        let wp1 = (*world_mat * Vec3::from(positions[i1]).extend(1.0)).xyz();
        let wp2 = (*world_mat * Vec3::from(positions[i2]).extend(1.0)).xyz();

        // World-space normals (use the normal matrix = transpose(inverse(M))).
        let wn0 = (norm_mat * Vec3::from(normals[i0]).extend(0.0))
            .xyz()
            .normalize();
        let wn1 = (norm_mat * Vec3::from(normals[i1]).extend(0.0))
            .xyz()
            .normalize();
        let wn2 = (norm_mat * Vec3::from(normals[i2]).extend(0.0))
            .xyz()
            .normalize();

        let uv0 = Vec2::from(uv1s[i0]);
        let uv1 = Vec2::from(uv1s[i1]);
        let uv2 = Vec2::from(uv1s[i2]);

        // Pixel-space bounding box of the triangle.
        let min_u = (uv0.x.min(uv1.x).min(uv2.x) * res_f).floor().max(0.0) as usize;
        let max_u = (uv0.x.max(uv1.x).max(uv2.x) * res_f)
            .ceil()
            .min(res_f - 1.0) as usize;
        let min_v = (uv0.y.min(uv1.y).min(uv2.y) * res_f).floor().max(0.0) as usize;
        let max_v = (uv0.y.max(uv1.y).max(uv2.y) * res_f)
            .ceil()
            .min(res_f - 1.0) as usize;

        for py in min_v..=max_v {
            for px in min_u..=max_u {
                let pixel_idx = py * res + px;
                if written[pixel_idx] {
                    continue;
                }

                // UV of this texel centre.
                let texel_uv = Vec2::new((px as f32 + 0.5) / res_f, (py as f32 + 0.5) / res_f);

                // Compute barycentric coordinates in UV space.
                let Some((b0, b1, b2)) = barycentric_uv(texel_uv, uv0, uv1, uv2) else {
                    continue;
                };

                // Interpolate world position and normal.
                let world_pos = wp0 * b0 + wp1 * b1 + wp2 * b2;
                let world_normal = (wn0 * b0 + wn1 * b1 + wn2 * b2).normalize();

                let origin = world_pos + world_normal * 0.02;

                let shadow = raycast::cast_shadow_ray(
                    origin,
                    sun_dir,
                    cache,
                    config.ao_radius * 4.0,
                    config.shadow_steps,
                );
                let ao = raycast::compute_ao(
                    origin,
                    world_normal,
                    cache,
                    config.ao_radius,
                    config.ao_rays,
                    config.shadow_steps,
                );

                let combined = (shadow * 0.5 + ao * 0.5).clamp(0.0, 1.0);
                pixels[pixel_idx] = (combined * 255.0) as u8;
                written[pixel_idx] = true;
            }
        }
    }
}

/// Compute barycentric coordinates `(b0, b1, b2)` of point `p` in the triangle
/// `(a, b, c)` in 2D UV space. Returns `None` if the point is outside the
/// triangle or the triangle is degenerate.
fn barycentric_uv(p: Vec2, a: Vec2, b: Vec2, c: Vec2) -> Option<(f32, f32, f32)> {
    let v0 = c - a;
    let v1 = b - a;
    let v2 = p - a;

    let dot00 = v0.dot(v0);
    let dot01 = v0.dot(v1);
    let dot02 = v0.dot(v2);
    let dot11 = v1.dot(v1);
    let dot12 = v1.dot(v2);

    let denom = dot00 * dot11 - dot01 * dot01;
    if denom.abs() < 1e-10 {
        return None; // Degenerate triangle
    }

    let inv_denom = 1.0 / denom;
    let u = (dot11 * dot02 - dot01 * dot12) * inv_denom;
    let v = (dot00 * dot12 - dot01 * dot02) * inv_denom;

    if u < 0.0 || v < 0.0 || u + v > 1.0 {
        return None;
    }

    let b2 = u;
    let b1 = v;
    let b0 = 1.0 - b1 - b2;
    Some((b0, b1, b2))
}

/// 3×3 max dilation: for each unwritten texel, assign the max of the 3×3
/// neighbourhood of written texels (or leave it at 0 if no neighbours are written).
fn dilate_lightmap(pixels: &mut Vec<u8>, res: usize) {
    let original = pixels.clone();
    let res_i = res as i32;

    for y in 0..res_i {
        for x in 0..res_i {
            let idx = (y * res_i + x) as usize;
            if original[idx] > 0 {
                continue; // already written
            }
            let mut best: u8 = 0;
            for dy in -1i32..=1 {
                for dx in -1i32..=1 {
                    let nx = x + dx;
                    let ny = y + dy;
                    if nx >= 0 && ny >= 0 && nx < res_i && ny < res_i {
                        best = best.max(original[(ny * res_i + nx) as usize]);
                    }
                }
            }
            pixels[idx] = best;
        }
    }
}

fn save_object_lightmap_png(
    pixels: &[u8],
    res: usize,
    terrain_world: &TerrainWorld,
    object_id: u64,
) -> Result<(), Box<dyn std::error::Error>> {
    let objects_dir = terrain_world.world_path.join("objects");
    fs::create_dir_all(&objects_dir)?;
    let path = object_lightmap_path(&terrain_world.world_path, object_id);

    let img = image::GrayImage::from_raw(res as u32, res as u32, pixels.to_vec())
        .ok_or("Failed to build GrayImage")?;
    img.save(&path)?;

    Ok(())
}

/// Path at which a placed-object lightmap PNG is stored.
pub fn object_lightmap_path(world_path: &Path, object_id: u64) -> std::path::PathBuf {
    world_path
        .join("objects")
        .join(format!("obj_{object_id}_lightmap.png"))
}
