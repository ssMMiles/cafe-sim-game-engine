//! Baked lightmap generation for placed mesh instances.

use bevy::{
    math::{Mat4, Vec2, Vec3, Vec4Swizzles},
    mesh::{Indices, Mesh, VertexAttributeValues},
    platform::time::Instant,
    prelude::*,
};
use rayon::prelude::*;
use std::collections::HashSet;
use std::time::Duration;

use super::bvh::TriangleBvh;
use super::lightmap::{BakeLighting, LightmapBuffer, LightmapLocation, store_lightmap};
use super::object_lightmaps::{
    ToonMat, ToonMeshQuery, collect_toon_meshes, object_lightmap_location, unique_material,
};
use super::raycast::BakeHeightCache;
use super::uv_unwrap::{
    LightmapUvCache, cell_texels, lightmap_resolution, max_world_edge, texels_per_cell,
};
use super::{BakeConfig, BakeRegion, raycast};
use crate::game::world::{
    objects::PlacedObject,
    terrain::{HeightmapData, storage::TerrainWorld},
};

/// A sub-mesh's bake, packaged so the work can leave the main thread.
///
/// Everything here is a copy, because the ECS is not `Send` and the whole
/// point is to hand the job to `AsyncComputeTaskPool` and let the frame
/// carry on. Mesh *data* is snapshotted rather than the rasterised g-buffer:
/// a 1024² atlas of [`TexelGeom`] is ~40 MB, where the mesh it came from is
/// usually a few KB, so rasterisation happens inside the task.
pub struct ObjectBakeJob {
    object_id: u64,
    sub_mesh: usize,
    res: usize,
    tri_count: u32,
    mesh: MeshSnapshot,
    world_mat: Mat4,
    cache: BakeHeightCache,
    /// Resolved on the main world — `unique_material` may *add* an asset, so
    /// it cannot run off-thread.
    material: Handle<ToonMat>,
    /// Where the finished EXR goes: a file path natively, an OPFS key on wasm.
    dest: LightmapLocation,
}

/// A finished sub-mesh lightmap on its way back to the ECS.
pub struct ObjectBakeResult {
    pub material: Handle<ToonMat>,
    pub image: Image,
}

/// Mesh arrays copied out of `Assets<Mesh>` so rasterisation can happen off
/// the main thread.
struct MeshSnapshot {
    positions: Vec<[f32; 3]>,
    normals: Vec<[f32; 3]>,
    uv1: Vec<[f32; 2]>,
    indices: Vec<u32>,
}

impl MeshSnapshot {
    /// `None` if the mesh is missing anything the rasteriser needs. UV1 in
    /// particular should always be present — `uv_unwrap` puts it there — so
    /// its absence means the unwrap was skipped.
    fn from_mesh(mesh: &Mesh) -> Option<Self> {
        let positions = match mesh.attribute(Mesh::ATTRIBUTE_POSITION)? {
            VertexAttributeValues::Float32x3(v) => v.clone(),
            _ => return None,
        };
        let normals = match mesh.attribute(Mesh::ATTRIBUTE_NORMAL)? {
            VertexAttributeValues::Float32x3(v) => v.clone(),
            _ => return None,
        };
        let uv1 = match mesh.attribute(Mesh::ATTRIBUTE_UV_1)? {
            VertexAttributeValues::Float32x2(v) => v.clone(),
            _ => return None,
        };
        let indices = match mesh.indices()? {
            Indices::U32(v) => v.clone(),
            Indices::U16(v) => v.iter().map(|&i| i as u32).collect(),
        };
        Some(Self {
            positions,
            normals,
            uv1,
            indices,
        })
    }
}

/// Gather one [`ObjectBakeJob`] per toon-shaded sub-mesh of every placed
/// object standing inside `region`.
///
/// This is the half of the object bake that *must* run on the main world:
/// `uv_unwrap` mutates the shared `Mesh` asset, `unique_material` may add a
/// material asset, and the heightmap lives in a resource. Everything
/// expensive is deferred to [`run_object_bake_jobs`].
///
/// See `object_lightmaps.rs` for why lightmaps are per sub-mesh rather than
/// per object.
pub fn collect_object_bake_jobs(
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
    uv_cache: &mut LightmapUvCache,
) -> Vec<ObjectBakeJob> {
    let mut jobs = Vec::new();

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

        // Materials claimed so far for this object; sub-meshes that share one
        // get a private clone so their lightmaps don't overwrite each other.
        let mut seen_materials = HashSet::new();

        for (k, entry) in entries.iter().enumerate() {
            // Unwrap the mesh (once per asset), then size this sub-mesh's
            // atlas from how big its triangles actually are in the world.
            let mesh_id = entry.mesh.id();
            let Some(tri_count) = uv_cache.ensure(meshes, mesh_id) else {
                continue;
            };

            let Some(mesh) = meshes.get(mesh_id) else {
                continue;
            };

            // Per *entry*, not per mesh asset: two placed instances of one
            // mesh at different scales need different texel budgets, and the
            // EXR carries its own dimensions, so nothing downstream requires
            // instances of a shared asset to agree on resolution.
            let world_mat = entry.transform.to_matrix();
            let cell = texels_per_cell(
                max_world_edge(mesh, &world_mat),
                config.mesh_texels_per_metre,
                config.mesh_texels_per_triangle,
            );
            let res = lightmap_resolution(tri_count, cell, config.mesh_resolution_max) as usize;

            let Some(snapshot) = MeshSnapshot::from_mesh(mesh) else {
                warn!(
                    "Skipping object lightmap for id={} sub-mesh {k}: mesh is missing \
                     positions, normals, lightmap UVs or indices",
                    placed_obj.id
                );
                continue;
            };

            // Resolved here, not in the task: this can add a material asset.
            let Some(material) = unique_material(entry, &mut seen_materials, materials, commands)
            else {
                continue;
            };

            jobs.push(ObjectBakeJob {
                object_id: placed_obj.id,
                sub_mesh: k,
                res,
                tri_count,
                mesh: snapshot,
                world_mat,
                cache: BakeHeightCache::extract(
                    heightmap,
                    bbox_min.x - ray_reach,
                    bbox_min.z - ray_reach,
                    bbox_max.x + ray_reach,
                    bbox_max.z + ray_reach,
                ),
                material,
                dest: object_lightmap_location(terrain_world, placed_obj.id, k),
            });
        }
    }

    jobs
}

/// Bake every gathered job. **Touches no ECS state** — this is what runs on
/// `AsyncComputeTaskPool`, so that Shift+B doesn't stall the frame.
///
/// Per job:
/// 1. Rasterize the triangles in UV2 space into a per-texel surface buffer
///    ([`rasterize_geometry`]).
/// 2. Shade every covered texel with the shared lighting model
///    ([`shade_geometry`], in parallel): sun shadow rays, ambient×AO, and
///    every placed point/spot light; no indirect bounce for objects yet.
///    Shadow rays test the scene BVH, which includes this very object, so it
///    shadows itself; ray origins are nudged along the *face* normal to keep
///    a triangle from occluding its own texels.
/// 3. Blur, dilate, and write the EXR.
///
/// Steps 1 and 2 are separate passes rather than one loop because only step 2
/// parallelises: rasterisation resolves overlapping triangles in order, while
/// shading is independent per texel. The split is also what a GPU bake would
/// need — step 1's buffer is what it would upload. See
/// `docs/GPU_OBJECT_BAKE_PLAN.md`.
pub fn run_object_bake_jobs(
    jobs: Vec<ObjectBakeJob>,
    config: &BakeConfig,
    lighting: &BakeLighting,
    bvh: &TriangleBvh,
) -> Vec<ObjectBakeResult> {
    let mut stats = BakeStats::default();
    let mut results = Vec::with_capacity(jobs.len());

    for job in jobs {
        let res = job.res;

        // Pass 1 — rasterise. Cheap: barycentric interpolation, no rays.
        let t_raster = Instant::now();
        let geometry = rasterize_geometry(&job.mesh, &job.world_mat, res);
        stats.rasterize += t_raster.elapsed();

        // Texels a triangle actually covered. Dilation fills the rest and
        // marks what it filled, so this has to be owned and mutable.
        let mut written: Vec<bool> = geometry.iter().map(Option::is_some).collect();
        let covered = written.iter().filter(|w| **w).count();

        // Pass 2 — shade. This is the expensive one, and it parallelises.
        let t_shade = Instant::now();
        let mut lightmap = shade_geometry(&geometry, res, &job.cache, bvh, config, lighting);
        stats.shade += t_shade.elapsed();

        stats.sub_meshes += 1;
        stats.texels_total += res * res;
        stats.texels_covered += covered;
        stats.max_res = stats.max_res.max(res);
        debug!(
            "  obj {} sub-mesh {}: {res}×{res} atlas, {} tris, {covered} texel(s) covered ({:.0}%)",
            job.object_id,
            job.sub_mesh,
            job.tri_count,
            100.0 * covered as f32 / (res * res).max(1) as f32,
        );

        let t_post = Instant::now();

        // Smooth away AO's Monte Carlo variance before anything quantises
        // it, exactly as the terrain bake does — objects were getting no
        // blur at all, so their noise landed straight on the toon band
        // edges as speckle. Confined to each triangle's own cell (atlas
        // neighbours are not surface neighbours) and to covered texels
        // (the gutter is still empty until dilation below), and capped so
        // a small cell can't be blurred flat.
        let cell_px = cell_texels(res as u32, job.tri_count) as usize;
        let blur_radius = config.blur_radius.min((cell_px / 4) as u32);
        lightmap = lightmap.blur_cells(blur_radius, cell_px, &written);

        // Flood lit values one texel past each triangle's edge so bilinear
        // filtering there never samples an empty (black) texel. A small
        // constant on purpose — see `BakeConfig::dilate_passes` for why
        // this must *not* scale with the gutter.
        lightmap.dilate(&mut written, config.dilate_passes);
        stats.post += t_post.elapsed();

        // A failed write is logged but doesn't stop the live material from
        // picking up the result.
        let t_io = Instant::now();
        if let Err(e) = store_lightmap(&job.dest, &lightmap) {
            error!(
                "Failed to save object lightmap for id={} sub-mesh {}: {e}",
                job.object_id, job.sub_mesh,
            );
        }
        stats.io += t_io.elapsed();

        results.push(ObjectBakeResult {
            material: job.material,
            image: lightmap.into_image(),
        });
    }

    stats.report(config);
    results
}

// ============================================================================
// Instrumentation
// ============================================================================

/// Where an object bake pass spent its time, and on how many texels.
///
/// Exists to answer one question before anyone ports this to the GPU: is the
/// cost actually in the ray casting, or somewhere unglamorous like the EXR
/// writes? See `docs/GPU_OBJECT_BAKE_PLAN.md`.
#[derive(Default)]
struct BakeStats {
    sub_meshes: usize,
    /// Texels some triangle covered — the ones that cost rays.
    texels_covered: usize,
    /// Total atlas area, covered or not. The ratio shows how much of the
    /// per-triangle cell layout is gutter and unused half-cell.
    texels_total: usize,
    max_res: usize,
    rasterize: Duration,
    shade: Duration,
    /// Blur + dilate.
    post: Duration,
    /// EXR encode and write.
    io: Duration,
}

impl BakeStats {
    fn report(&self, config: &BakeConfig) {
        if self.sub_meshes == 0 {
            return;
        }
        let total = self.rasterize + self.shade + self.post + self.io;
        // Rays per covered texel: one sun shadow ray, the AO hemisphere, and
        // one segment ray per placed light. Objects cast no bounce rays.
        let rays_per_texel = 1 + config.ao_rays as usize;
        let est_rays = self.texels_covered * rays_per_texel;

        info!(
            "Object bake: {} sub-mesh(es), {} of {} texels covered ({:.0}%), \
             largest atlas {}², ≥{:.1}M rays",
            self.sub_meshes,
            self.texels_covered,
            self.texels_total,
            100.0 * self.texels_covered as f32 / self.texels_total.max(1) as f32,
            self.max_res,
            est_rays as f64 / 1.0e6,
        );
        info!(
            "  rasterize {:.2}s | shade {:.2}s | blur+dilate {:.2}s | exr {:.2}s | total {:.2}s",
            self.rasterize.as_secs_f64(),
            self.shade.as_secs_f64(),
            self.post.as_secs_f64(),
            self.io.as_secs_f64(),
            total.as_secs_f64(),
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

/// Per-texel surface data: the output of rasterisation, the input to shading.
///
/// This is also exactly what a GPU bake would upload per sub-mesh, in place
/// of the heightmap the terrain compute shader reads — see
/// `docs/GPU_OBJECT_BAKE_PLAN.md`.
#[derive(Clone, Copy)]
struct TexelGeom {
    world_pos: Vec3,
    /// Interpolated vertex normal — what the lighting model shades with.
    normal: Vec3,
    /// Geometric face normal — what ray origins are offset along, so a
    /// triangle can't occlude its own texels on a smooth-shaded mesh.
    face_normal: Vec3,
}

/// Rasterize a mesh's triangles in UV2 space into a per-texel surface buffer.
///
/// `None` means no triangle covered that texel: the atlas gutter, or the
/// unused half of a triangle's cell. Dilation fills those in later.
///
/// Serial by design, and cheap enough for that not to matter — it is pure
/// interpolation, no rays. Triangles are visited in order and the first to
/// claim a texel keeps it, which is what makes the output independent of how
/// the triangles happen to overlap.
fn rasterize_geometry(mesh: &MeshSnapshot, world_mat: &Mat4, res: usize) -> Vec<Option<TexelGeom>> {
    let mut geometry: Vec<Option<TexelGeom>> = vec![None; res * res];

    let (positions, normals, uv1s, indices) =
        (&mesh.positions, &mesh.normals, &mesh.uv1, &mesh.indices);
    let res_f = res as f32;

    // Normal matrix is constant for this world transform — compute it once
    // outside the triangle loop rather than once per triangle.
    let norm_mat = world_mat.inverse().transpose();

    for tri in indices.as_chunks::<3>().0 {
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
                if geometry[pixel_idx].is_some() {
                    continue;
                }

                // UV of this texel centre.
                let texel_uv = Vec2::new((px as f32 + 0.5) / res_f, (py as f32 + 0.5) / res_f);

                // Compute barycentric coordinates in UV space.
                let Some((b0, b1, b2)) = barycentric_uv(texel_uv, uv0, uv1, uv2) else {
                    continue;
                };

                geometry[pixel_idx] = Some(TexelGeom {
                    world_pos: wp0 * b0 + wp1 * b1 + wp2 * b2,
                    normal: (wn0 * b0 + wn1 * b1 + wn2 * b2).normalize(),
                    face_normal,
                });
            }
        }
    }

    geometry
}

/// Shade every covered texel with the baked lighting model.
///
/// This is where an object bake spends essentially all of its time: each
/// covered texel casts a sun shadow ray, an AO hemisphere, and one segment
/// ray per placed light. None of them depend on any other texel, so the pass
/// is `par_iter` — matching what the CPU terrain bake has always done
/// (`terrain.rs`) and what the object path was missing.
///
/// Uncovered texels stay black; `dilate` floods lit values into them after
/// the blur, so nothing ever samples one.
fn shade_geometry(
    geometry: &[Option<TexelGeom>],
    res: usize,
    cache: &BakeHeightCache,
    bvh: &TriangleBvh,
    config: &BakeConfig,
    lighting: &BakeLighting,
) -> LightmapBuffer {
    let sun_dir = lighting.sun_dir;

    let texels: Vec<Vec3> = geometry
        .par_iter()
        .map(|slot| {
            let Some(g) = slot else {
                return Vec3::ZERO;
            };

            // Both ray origins are the same point, unlike the terrain bake,
            // which lifts its DDA origin 0.5 up so the coarse march can't
            // re-hit the ground it left. An object texel is not on the
            // ground, so there is nothing to step off.
            let origin = g.world_pos + g.face_normal * 0.02;

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
                g.normal,
                cache,
                bvh,
                config.ao_radius,
                config.ao_rays,
                config.ao_steps,
            );
            let ndotl = g.normal.dot(sun_dir).max(0.0);

            let mut radiance = lighting.sun_and_ambient(ndotl, shadow, ao, 0.0);
            radiance += lighting.local_lights(origin, g.normal, |dir, dist| {
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
            radiance
        })
        .collect();

    LightmapBuffer { res, texels }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::world::baking::uv_unwrap::generate_lightmap_uvs;
    use bevy::asset::RenderAssetUsages;
    use bevy::mesh::PrimitiveTopology;

    /// A 1×1 quad in the XZ plane, unwrapped for lightmapping and snapshotted
    /// exactly as the bake does — so the tests exercise the real path from
    /// `Assets<Mesh>` to the rasteriser, not a hand-built buffer.
    fn unwrapped_quad() -> MeshSnapshot {
        MeshSnapshot::from_mesh(&unwrapped_quad_mesh()).expect("quad should snapshot")
    }

    fn unwrapped_quad_mesh() -> Mesh {
        let mut mesh = Mesh::new(
            PrimitiveTopology::TriangleList,
            RenderAssetUsages::default(),
        );
        mesh.insert_attribute(
            Mesh::ATTRIBUTE_POSITION,
            vec![
                [0.0, 0.0, 0.0],
                [1.0, 0.0, 0.0],
                [0.0, 0.0, 1.0],
                [1.0, 0.0, 1.0],
            ],
        );
        mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, vec![[0.0f32, 1.0, 0.0]; 4]);
        mesh.insert_indices(Indices::U32(vec![0, 1, 2, 1, 3, 2]));
        generate_lightmap_uvs(&mut mesh).expect("quad should unwrap");
        mesh
    }

    /// Rasterisation has to place texels using the *instance* transform, not
    /// the mesh's local coordinates: a glTF node's own scale is part of where
    /// the surface actually is, and a texel positioned without it lands
    /// inside the real geometry where self-shadowing bakes it black.
    #[test]
    fn rasterized_texels_land_on_the_transformed_surface() {
        let mesh = unwrapped_quad();
        // The quad scaled 2× in X and Z, lifted 3 units.
        let world = Mat4::from_scale_rotation_translation(
            Vec3::new(2.0, 1.0, 2.0),
            Quat::IDENTITY,
            Vec3::new(0.0, 3.0, 0.0),
        );

        let res = 64;
        let geometry = rasterize_geometry(&mesh, &world, res);
        assert_eq!(geometry.len(), res * res);

        let covered: Vec<&TexelGeom> = geometry.iter().flatten().collect();
        assert!(!covered.is_empty(), "nothing was rasterized");

        for g in &covered {
            assert!(
                (g.world_pos.y - 3.0).abs() < 1e-3,
                "texel off the quad's plane: {:?}",
                g.world_pos
            );
            assert!(
                (-1e-3..=2.0 + 1e-3).contains(&g.world_pos.x)
                    && (-1e-3..=2.0 + 1e-3).contains(&g.world_pos.z),
                "texel outside the scaled quad: {:?}",
                g.world_pos
            );
            assert!(
                g.normal.dot(Vec3::Y) > 0.99,
                "normal should survive the transform: {:?}",
                g.normal
            );
            assert!(
                g.face_normal.dot(Vec3::Y) > 0.99,
                "face normal must agree with the vertex normals, not be inverted: {:?}",
                g.face_normal
            );
        }
    }

    /// Two triangles land in two of a 2×2 cell grid, each filling about half
    /// its gutter-inset cell — so roughly a tenth of the atlas. Mostly a
    /// guard against a UV or bounding-box change silently collapsing
    /// coverage to nothing (or to everything).
    #[test]
    fn coverage_matches_the_per_triangle_cell_layout() {
        let mesh = unwrapped_quad();
        let res = 128;
        let geometry = rasterize_geometry(&mesh, &Mat4::IDENTITY, res);

        let covered = geometry.iter().filter(|g| g.is_some()).count();
        let fraction = covered as f32 / (res * res) as f32;
        assert!(
            (0.05..0.20).contains(&fraction),
            "{covered} of {} texels covered ({:.1}%) — expected ~10% for two \
             triangles in a 2×2 cell grid",
            res * res,
            fraction * 100.0
        );
    }

    /// A mesh the unwrapper never touched has no UV1, so there is nothing to
    /// rasterise against. Snapshotting must refuse it — the bake then logs
    /// and skips that sub-mesh rather than producing a black lightmap.
    #[test]
    fn a_mesh_without_lightmap_uvs_cannot_be_snapshotted() {
        let mut mesh = Mesh::new(
            PrimitiveTopology::TriangleList,
            RenderAssetUsages::default(),
        );
        mesh.insert_attribute(
            Mesh::ATTRIBUTE_POSITION,
            vec![[0.0f32, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 0.0, 1.0]],
        );
        mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, vec![[0.0f32, 1.0, 0.0]; 3]);
        mesh.insert_indices(Indices::U32(vec![0, 1, 2]));

        assert!(MeshSnapshot::from_mesh(&mesh).is_none());
    }

    /// The snapshot is what crosses to the task pool, so it has to carry
    /// everything the rasteriser reads — and `generate_lightmap_uvs`
    /// de-indexes, so the arrays it copies are the post-unwrap ones.
    #[test]
    fn snapshot_carries_the_unwrapped_mesh() {
        let mesh = unwrapped_quad_mesh();
        let snapshot = MeshSnapshot::from_mesh(&mesh).expect("should snapshot");

        assert_eq!(
            snapshot.positions.len(),
            6,
            "quad de-indexed to 2 triangles"
        );
        assert_eq!(snapshot.normals.len(), 6);
        assert_eq!(snapshot.uv1.len(), 6);
        assert_eq!(snapshot.indices, vec![0, 1, 2, 3, 4, 5]);
    }
}
