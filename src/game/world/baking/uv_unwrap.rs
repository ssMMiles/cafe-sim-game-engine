//! UV2 generation for placed mesh lightmap baking.
//!
//! Uses a simple axis-aligned planar (XZ) projection onto the mesh bounding
//! box rather than a full atlas unwrap. This avoids seam artifacts at silhouette
//! edges of complex shapes but is sufficient for the low-frequency shadow/AO
//! data stored in lightmaps.

use bevy::{
    log::warn,
    mesh::{Mesh, VertexAttributeValues},
};

/// Generate lightmap UV2 coordinates for `mesh` using a planar XZ bounding-box
/// projection and write them into [`Mesh::ATTRIBUTE_UV_1`].
///
/// This is a no-op if the mesh already has `ATTRIBUTE_UV_1`.
///
/// Returns `true` if UV2 was generated, `false` if the mesh already had UV2
/// or did not have sufficient data to project.
pub fn generate_lightmap_uvs(mesh: &mut Mesh) -> bool {
    // Already has UV2 — skip.
    if mesh.attribute(Mesh::ATTRIBUTE_UV_1).is_some() {
        return false;
    }

    let positions: Vec<[f32; 3]> = match mesh.attribute(Mesh::ATTRIBUTE_POSITION) {
        Some(VertexAttributeValues::Float32x3(v)) => v.clone(),
        _ => {
            warn!("generate_lightmap_uvs: mesh has no ATTRIBUTE_POSITION, skipping");
            return false;
        }
    };

    // Bevy's pbr_fragment.wgsl assumes VERTEX_UVS_A (UV_0) is always present
    // whenever VERTEX_UVS is set. Without UV_0, adding UV_1 causes
    // VERTEX_UVS_B without VERTEX_UVS_A, breaking shader compilation at line 208
    // (`uv` undefined). Add a dummy zero UV_0 to satisfy this assumption.
    if mesh.attribute(Mesh::ATTRIBUTE_UV_0).is_none() {
        mesh.insert_attribute(
            Mesh::ATTRIBUTE_UV_0,
            vec![[0.0f32, 0.0f32]; positions.len()],
        );
    }

    if positions.is_empty() {
        return false;
    }

    // Compute bounding box in XZ.
    let mut min_x = f32::MAX;
    let mut max_x = f32::MIN;
    let mut min_z = f32::MAX;
    let mut max_z = f32::MIN;

    for p in &positions {
        min_x = min_x.min(p[0]);
        max_x = max_x.max(p[0]);
        min_z = min_z.min(p[2]);
        max_z = max_z.max(p[2]);
    }

    let range_x = (max_x - min_x).max(1e-6);
    let range_z = (max_z - min_z).max(1e-6);

    let uv1s: Vec<[f32; 2]> = positions
        .iter()
        .map(|p| [(p[0] - min_x) / range_x, (p[2] - min_z) / range_z])
        .collect();

    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_1, uv1s);
    true
}
