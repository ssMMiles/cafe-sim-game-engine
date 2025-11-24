use bevy::{
    asset::RenderAssetUsages,
    mesh::{Indices, Mesh, PrimitiveTopology},
};

pub fn generate_terrain_topology(
    width: usize,
    height: usize,
    tile_size: (f32, f32),
    max_height: f32,
) -> Mesh {
    let vertex_count = width * height;
    let mut indices = Vec::new();

    for y in 0..(height - 1) {
        for x in 0..(width - 1) {
            let top_left = (y * width + x) as u32;
            let top_right = (y * width + (x + 1)) as u32;
            let bottom_left = ((y + 1) * width + x) as u32;
            let bottom_right = ((y + 1) * width + (x + 1)) as u32;

            // First triangle
            indices.push(top_left);
            indices.push(bottom_left);
            indices.push(top_right);

            // Second triangle
            indices.push(top_right);
            indices.push(bottom_left);
            indices.push(bottom_right);
        }
    }

    // Create mesh with NO vertex attributes - just indices!
    let mut mesh = Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::RENDER_WORLD,
    );

    mesh.insert_indices(Indices::U32(indices));

    // Generate actual XZ positions (shader will add Y displacement)
    let mut positions = Vec::with_capacity(vertex_count);
    for y in 0..height {
        for x in 0..width {
            positions.push([
                x as f32 * tile_size.0,
                0.0, // Shader will set actual height
                y as f32 * tile_size.1,
            ]);
        }
    }
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);

    // Add dummy normals (shader calculates real ones)
    let normals: Vec<[f32; 3]> = (0..vertex_count).map(|_| [0.0, 1.0, 0.0]).collect();
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, normals);

    mesh
}

/// Calculate the AABB for the terrain after GPU displacement
pub fn calculate_terrain_aabb(
    width: usize,
    height: usize,
    tile_size: (f32, f32),
    max_height: f32,
) -> bevy::camera::primitives::Aabb {
    use bevy::{camera::primitives::Aabb, math::{Vec3}};

    let terrain_width = (width - 1) as f32 * tile_size.0;
    let terrain_depth = (height - 1) as f32 * tile_size.1;

    Aabb::from_min_max(
        Vec3::new(0.0, 0.0, 0.0),
        Vec3::new(terrain_width, max_height, terrain_depth),
    )
}
