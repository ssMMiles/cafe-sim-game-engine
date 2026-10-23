//! Baked lightmap generation for placed mesh instances.

use bevy::{
    math::{Mat4, Vec2, Vec3, Vec4Swizzles},
    mesh::{Indices, Mesh, VertexAttributeValues},
    prelude::*,
};
use std::collections::HashSet;

use super::bvh::TriangleBvh;
use super::lightmap::{BakeLighting, LightmapBuffer};
use super::object_lightmaps::{
    ToonMat, ToonMeshQuery, collect_toon_meshes, object_lightmap_path, unique_material,
};
use super::raycast::BakeHeightCache;
use super::uv_unwrap::{LightmapUvCache, lightmap_resolution};
use super::{BakeConfig, BakeRegion, raycast};
use crate::game::world::{
    objects::PlacedObject,
    terrain::{HeightmapData, storage::TerrainWorld},
};

/// Bake lightmaps for the placed objects standing inside `region`.
///
/// For each toon-shaded sub-mesh of each object (see `object_lightmaps.rs`
/// for why lightmaps are per sub-mesh) this function:
/// 1. Ensures the mesh has non-overlapping lightmap UV2 (`uv_unwrap`), and
///    sizes the atlas from its triangle count.
/// 2. Rasterizes each triangle in UV2 space, interpolating world-space
///    position and normal from barycentric coords.
/// 3. Shades each texel with the shared lighting model (sun shadow rays,
///    ambient×AO, and every placed point/spot light; no indirect bounce for
///    objects yet). Shadow rays test the scene BVH, which includes this very
///    object, so it shadows itself; ray origins are nudged along the *face*
///    normal to keep a triangle from occluding its own texels.
/// 4. Saves the result as EXR and applies it to the sub-mesh's material.
#[allow(clippy::too_many_arguments)]
pub fn bake_object_lightmaps(
    commands: &mut Commands,
    heightmap: &HeightmapData,
    terrain_world: &TerrainWorld,
    config: &BakeConfig,
    region: &BakeRegion,
    placed_query: &Query<(Entity, &PlacedObject, &GlobalTransform)>,
    mesh_query: &ToonMeshQuery,
    children_query: &Query<&Children>,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<ToonMat>,
    images: &mut Assets<Image>,
    uv_cache: &mut LightmapUvCache,
    lighting: &BakeLighting,
    bvh: &TriangleBvh,
) {
    for (root_entity, placed_obj, global_transform) in placed_query.iter() {
        if !region.contains_point(global_transform.translation()) {
            continue;
        }
        // Every toon-shaded mesh entity in the scene hierarchy, in the
        // canonical order that numbers the lightmap files. Each carries its
        // own world transform — see `ToonMeshEntry::transform`.
        let entries = collect_toon_meshes(root_entity, mesh_query, children_query);
        if entries.is_empty() {
            continue;
        }

        // Compute the world-space bounding box across all sub-meshes so we can
        // build a tight heightmap cache covering the entire object + ray reach.
        let ray_reach = config.ao_radius * 4.0;
        let mut bbox_min = Vec3::splat(f32::MAX);
        let mut bbox_max = Vec3::splat(f32::MIN);
        let mut have_bounds = false;

        for entry in &entries {
            if let Some(mesh) = meshes.get(entry.mesh.id()) {
                let (mn, mx) = world_bounds(mesh, &entry.transform.to_matrix());
                bbox_min = bbox_min.min(mn);
                bbox_max = bbox_max.max(mx);
                have_bounds = true;
            }
        }

        // Fall back to a generous region centred on the object's world position
        // if we couldn't derive bounds from vertices.
        if !have_bounds {
            let centre = global_transform.translation();
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

        // Materials claimed so far for this object; sub-meshes that share one
        // get a private clone so their lightmaps don't overwrite each other.
        let mut seen_materials = HashSet::new();

        for (k, entry) in entries.iter().enumerate() {
            // Unwrap the mesh (once per asset) and size this sub-mesh's atlas
            // from its triangle count.
            let mesh_id = entry.mesh.id();
            let Some(tri_count) = uv_cache.ensure(meshes, mesh_id) else {
                continue;
            };
            let res = lightmap_resolution(
                tri_count,
                config.mesh_texels_per_triangle,
                config.mesh_resolution_max,
            ) as usize;

            let Some(mesh) = meshes.get(mesh_id) else {
                continue;
            };

            let mut lightmap = LightmapBuffer::new(res);
            // Texels a triangle actually covered: later triangles don't
            // overwrite earlier ones, and dilation only fills the gaps.
            let mut written = vec![false; res * res];

            rasterize_mesh_lightmap(
                mesh,
                &entry.transform.to_matrix(),
                &cache,
                bvh,
                config,
                lighting,
                &mut lightmap,
                &mut written,
            );

            // Flood lit values into each cell's gutter so bilinear filtering
            // at a triangle's edge never samples an empty (black) texel.
            lightmap.dilate(&mut written, config.dilate_passes);

            // A failed write is logged but doesn't stop the live material from
            // picking up the result.
            let path = object_lightmap_path(&terrain_world.world_path, placed_obj.id, k);
            if let Err(e) = lightmap.save_exr(&path) {
                error!(
                    "Failed to save object lightmap for id={} sub-mesh {k} to {}: {e}",
                    placed_obj.id,
                    path.display()
                );
            }

            let Some(mat_handle) = unique_material(entry, &mut seen_materials, materials, commands)
            else {
                continue;
            };
            if let Some(mat) = materials.get_mut(mat_handle.id()) {
                mat.extension.lightmap = images.add(lightmap.into_image());
            }
        }

        info!(
            "Baked {} object lightmap(s) for placed object id={}",
            entries.len(),
            placed_obj.id
        );
    }
}

// ============================================================================
// Helpers
// ============================================================================

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

/// Rasterize a mesh's triangles in UV2 space and shade each covered texel.
#[allow(clippy::too_many_arguments)]
fn rasterize_mesh_lightmap(
    mesh: &Mesh,
    world_mat: &Mat4,
    cache: &BakeHeightCache,
    bvh: &TriangleBvh,
    config: &BakeConfig,
    lighting: &BakeLighting,
    lightmap: &mut LightmapBuffer,
    written: &mut [bool],
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

    let res = lightmap.res;
    let res_f = res as f32;
    let sun_dir = lighting.sun_dir;

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

        // Ray origins are offset along the geometric (face) normal, not the
        // interpolated vertex normal: on a smooth-shaded mesh the vertex
        // normal can lean enough that a point nudged along it lands *under*
        // a neighbouring triangle, which then falsely shadows it (acne). The
        // face normal is oriented to agree with the vertex normals so an
        // inverted winding doesn't push the origin inside the surface.
        let mut face_normal = (wp1 - wp0).cross(wp2 - wp0).normalize_or_zero();
        if face_normal == Vec3::ZERO {
            face_normal = (wn0 + wn1 + wn2).normalize_or(Vec3::Y);
        } else if face_normal.dot(wn0 + wn1 + wn2) < 0.0 {
            face_normal = -face_normal;
        }

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

                let origin = world_pos + face_normal * 0.02;

                let shadow = raycast::cast_shadow_ray(
                    origin,
                    origin,
                    sun_dir,
                    cache,
                    bvh,
                    config.ao_radius * 4.0,
                    config.shadow_steps,
                );
                let ao = raycast::compute_ao(
                    origin,
                    origin,
                    world_normal,
                    cache,
                    bvh,
                    config.ao_radius,
                    config.ao_rays,
                    config.ao_steps,
                );
                let ndotl = world_normal.dot(sun_dir).max(0.0);

                let mut radiance = lighting.sun_and_ambient(ndotl, shadow, ao, 0.0);
                radiance += lighting.local_lights(origin, world_normal, |dir, dist| {
                    raycast::cast_shadow_ray_segment(
                        origin,
                        origin,
                        dir,
                        dist,
                        cache,
                        bvh,
                        config.light_shadow_steps(dist),
                    )
                });

                lightmap.texels[pixel_idx] = radiance;
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
