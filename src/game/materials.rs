use bevy::{asset::RenderAssetUsages, mesh::{Indices, PrimitiveTopology}, prelude::*};

#[derive(Component, Clone)]
pub struct PendingMaterial {
    pub base_color: Color,
    pub metallic: f32,
    pub perceptual_roughness: f32,
    pub alpha_mode: bevy::render::alpha::AlphaMode,
}

pub fn apply_material_to_meshes(
    commands: &mut Commands,
    children: &Children,
    material: &Handle<StandardMaterial>,
    mesh_query: &Query<Entity, With<Mesh3d>>,
    children_query: &Query<&Children>,
) {
    for child in children.iter() {
        if mesh_query.contains(child) {
            commands
                .entity(child)
                .insert(MeshMaterial3d(material.clone()));
        }

        // Recursively apply to grandchildren
        if let Ok(grandchildren) = children_query.get(child) {
            apply_material_to_meshes(
                commands,
                grandchildren,
                material,
                mesh_query,
                children_query,
            );
        }
    }
}

pub fn update_mesh_materials_color(
    children: &Children,
    new_color: Color,
    children_query: &Query<&Children>,
    mesh_material_query: &mut Query<&mut MeshMaterial3d<StandardMaterial>, With<Mesh3d>>,
    materials: &mut ResMut<Assets<StandardMaterial>>,
) {
    for child in children.iter() {
        if let Ok(mesh_material) = mesh_material_query.get_mut(child) {
            // Update the material's color
            if let Some(material) = materials.get_mut(&mesh_material.0) {
                material.base_color = new_color;
            }
        }

        // Recursively update grandchildren
        if let Ok(grandchildren) = children_query.get(child) {
            update_mesh_materials_color(
                grandchildren,
                new_color,
                children_query,
                mesh_material_query,
                materials,
            );
        }
    }
}

// Create a mesh for a circular arc (for progress indicators)
pub fn create_arc_mesh(radius: f32, progress: f32) -> Mesh {
    let mut mesh = Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::RENDER_WORLD,
    );

    // Ensure radius is valid
    let radius = radius.max(0.001); // Minimum radius to avoid degenerate geometry

    // Clamp progress to valid range
    let progress = progress.clamp(0.0, 1.0);

    // Calculate number of segments based on progress (more segments = smoother arc)
    let segments = ((progress * 32.0).ceil() as usize).max(1); // At least 1 segment to avoid empty mesh

    let mut vertices = Vec::new();
    let mut normals = Vec::new();
    let mut uvs = Vec::new();
    let mut indices = Vec::new();

    // Calculate y offset to center the arc
    // For arcs starting from top, the visual center is above the geometric origin
    let y_offset = radius * 0.5;

    // Center vertex - offset up
    vertices.push([0.0, y_offset, 0.0]);
    normals.push([0.0, 0.0, 1.0]);
    uvs.push([0.5, 0.5]);

    // Create vertices around the arc
    let angle_per_segment = (progress * 2.0 * std::f32::consts::PI) / segments as f32;
    let start_angle = -std::f32::consts::PI / 2.0; // Start from top

    for i in 0..=segments {
        let angle = start_angle + i as f32 * angle_per_segment;
        let x = radius * angle.cos();
        let y = radius * angle.sin() + y_offset; // Apply offset to arc vertices too

        vertices.push([x, y, 0.0]);
        normals.push([0.0, 0.0, 1.0]);
        uvs.push([
            (x / radius + 1.0) / 2.0,
            ((y - y_offset) / radius + 1.0) / 2.0,
        ]); // Adjust UV to maintain texture mapping

        // Create triangles (except for the first vertex)
        if i > 0 {
            indices.push(0);
            indices.push(i as u32);
            indices.push((i + 1) as u32);
        }
    }

    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, vertices);
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, normals);
    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, uvs);
    mesh.insert_indices(Indices::U32(indices));

    mesh
}
