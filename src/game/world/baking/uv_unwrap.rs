//! UV2 generation for placed mesh lightmap baking.
//!
//! Uses a triplanar projection that assigns each vertex to one of three
//! non-overlapping UV atlas regions based on its dominant normal axis:
//!
//! - Region u=[0, 1/3]:   Y-dominant normals (horizontal faces) — XZ projection
//! - Region u=[1/3, 2/3]: Z-dominant normals (front/back faces) — XY projection
//! - Region u=[2/3, 1]:   X-dominant normals (left/right faces)  — ZY projection
//!
//! GLTF meshes with hard edges already have vertices split per face, so each
//! vertex has one clear dominant axis and triangles stay within a single region.
//! For smooth meshes the few boundary triangles may straddle regions, but
//! the low-frequency nature of shadow/AO makes this imperceptible.

use bevy::{
    log::warn,
    mesh::{Mesh, VertexAttributeValues},
};

/// Generate lightmap UV2 coordinates for `mesh` using triplanar bounding-box
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

    if positions.is_empty() {
        return false;
    }

    // Bevy's pbr_fragment.wgsl assumes VERTEX_UVS_A (UV_0) is always present
    // whenever VERTEX_UVS is set. Without UV_0, adding UV_1 causes
    // VERTEX_UVS_B without VERTEX_UVS_A, which breaks shader compilation.
    // Add a dummy zero UV_0 to satisfy this assumption.
    if mesh.attribute(Mesh::ATTRIBUTE_UV_0).is_none() {
        mesh.insert_attribute(
            Mesh::ATTRIBUTE_UV_0,
            vec![[0.0f32, 0.0f32]; positions.len()],
        );
    }

    // Per-axis normals for triplanar assignment.
    let normals: Option<Vec<[f32; 3]>> = mesh.attribute(Mesh::ATTRIBUTE_NORMAL).and_then(|v| {
        if let VertexAttributeValues::Float32x3(v) = v {
            Some(v.clone())
        } else {
            None
        }
    });

    // Compute full bounding box (all three axes needed for triplanar).
    let mut min_x = f32::MAX;
    let mut max_x = f32::MIN;
    let mut min_y = f32::MAX;
    let mut max_y = f32::MIN;
    let mut min_z = f32::MAX;
    let mut max_z = f32::MIN;

    for p in &positions {
        min_x = min_x.min(p[0]);
        max_x = max_x.max(p[0]);
        min_y = min_y.min(p[1]);
        max_y = max_y.max(p[1]);
        min_z = min_z.min(p[2]);
        max_z = max_z.max(p[2]);
    }

    let range_x = (max_x - min_x).max(1e-6);
    let range_y = (max_y - min_y).max(1e-6);
    let range_z = (max_z - min_z).max(1e-6);

    // Triplanar UV assignment:
    //   |n.y| dominant → XZ projection in atlas column 0 (u ∈ [0,   1/3])
    //   |n.z| dominant → XY projection in atlas column 1 (u ∈ [1/3, 2/3])
    //   |n.x| dominant → ZY projection in atlas column 2 (u ∈ [2/3, 1  ])
    let uv1s: Vec<[f32; 2]> = match normals {
        Some(normals) => positions
            .iter()
            .zip(normals.iter())
            .map(|(p, n)| {
                let ax = n[0].abs();
                let ay = n[1].abs();
                let az = n[2].abs();

                if ay >= ax && ay >= az {
                    // Y-dominant (horizontal / top-bottom faces) — XZ plane
                    [(p[0] - min_x) / range_x / 3.0, (p[2] - min_z) / range_z]
                } else if az >= ax {
                    // Z-dominant (front/back faces) — XY plane
                    [
                        1.0 / 3.0 + (p[0] - min_x) / range_x / 3.0,
                        (p[1] - min_y) / range_y,
                    ]
                } else {
                    // X-dominant (left/right faces) — ZY plane
                    [
                        2.0 / 3.0 + (p[2] - min_z) / range_z / 3.0,
                        (p[1] - min_y) / range_y,
                    ]
                }
            })
            .collect(),

        // No normals — fall back to XZ projection for all vertices.
        None => positions
            .iter()
            .map(|p| [(p[0] - min_x) / range_x, (p[2] - min_z) / range_z])
            .collect(),
    };

    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_1, uv1s);
    true
}
