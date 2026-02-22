use bevy::{
    asset::RenderAssetUsages,
    camera::primitives::Aabb,
    math::Vec3,
    mesh::{Indices, Mesh, PrimitiveTopology},
};

/// Number of LOD levels per chunk
pub const LOD_LEVELS: usize = 4;

/// Skirt depth multiplier per LOD stride unit
const SKIRT_DEPTH_PER_STRIDE: f32 = 1.0;

/// Generate a chunk mesh at a specific LOD level with baked heights and normals.
///
/// `heights` is the full-terrain height grid in column-major layout:
/// `heights[x * grid_height + y]` gives the raw [0..1] height at grid position (x, y).
///
/// The chunk covers grid positions `[start_x .. start_x+chunk_width]` x
/// `[start_y .. start_y+chunk_height]`.
///
/// Returns `(Mesh, Aabb)` with real vertex positions and normals baked in.
pub fn generate_lod_mesh(
    heights: &[f32],
    grid_width: usize,
    grid_height: usize,
    start_x: usize,
    start_y: usize,
    chunk_width: usize,
    chunk_height: usize,
    tile_size: (f32, f32),
    height_scale: f32,
    lod_level: usize,
) -> (Mesh, Aabb) {
    let stride = 1usize << lod_level;
    let skirt_depth = stride as f32 * SKIRT_DEPTH_PER_STRIDE;

    // Number of vertices along each axis after striding
    let lod_width = chunk_width.div_ceil(stride);
    let lod_height = chunk_height.div_ceil(stride);

    let surface_count = lod_width * lod_height;
    // Perimeter vertex count for skirts: 2*(w+h) - 4 corners counted once each
    let perimeter = 2 * (lod_width + lod_height) - 4;

    let mut positions = Vec::with_capacity(surface_count + perimeter);
    let mut normals = Vec::with_capacity(surface_count + perimeter);

    // Helper to sample height from the full-terrain grid, clamping to bounds
    let sample = |gx: usize, gy: usize| -> f32 {
        let cx = gx.min(grid_width - 1);
        let cy = gy.min(grid_height - 1);
        heights[cx * grid_height + cy] * height_scale
    };

    let mut min_y = f32::MAX;
    let mut max_y = f32::MIN;

    // --- Surface vertices (column-major: x varies slowly, y varies fast) ---
    for lod_x in 0..lod_width {
        for lod_y in 0..lod_height {
            // Force edge vertices to actual chunk boundary so adjacent chunks share edges
            let local_x = if lod_x == lod_width - 1 {
                chunk_width - 1
            } else {
                lod_x * stride
            };
            let local_y = if lod_y == lod_height - 1 {
                chunk_height - 1
            } else {
                lod_y * stride
            };

            let global_x = start_x + local_x;
            let global_y = start_y + local_y;

            let h = sample(global_x, global_y);
            min_y = min_y.min(h);
            max_y = max_y.max(h);

            positions.push([
                local_x as f32 * tile_size.0,
                h,
                local_y as f32 * tile_size.1,
            ]);

            // Compute normal from 4-neighbor heights
            let h_left = sample(global_x.saturating_sub(1), global_y);
            let h_right = sample((global_x + 1).min(grid_width - 1), global_y);
            let h_down = sample(global_x, global_y.saturating_sub(1));
            let h_up = sample(global_x, (global_y + 1).min(grid_height - 1));

            let tangent_x = 2.0 * tile_size.0;
            let tangent_y = h_right - h_left;
            let bitangent_y = h_up - h_down;
            let bitangent_z = 2.0 * tile_size.1;

            // normal = cross(bitangent, tangent)
            // bitangent = (0, bitangent_y, bitangent_z)
            // tangent   = (tangent_x, tangent_y, 0)
            let nx = -tangent_y * bitangent_z;
            let ny = tangent_x * bitangent_z;
            let nz = -tangent_x * bitangent_y;
            let len = (nx * nx + ny * ny + nz * nz).sqrt();
            normals.push([nx / len, ny / len, nz / len]);
        }
    }

    // Helper: column-major index into surface vertices
    let idx = |x: usize, y: usize| -> u32 { (x * lod_height + y) as u32 };

    // --- Surface indices ---
    let mut indices = Vec::new();
    for lod_x in 0..(lod_width - 1) {
        for lod_y in 0..(lod_height - 1) {
            let tl = idx(lod_x, lod_y);
            let tr = idx(lod_x + 1, lod_y);
            let bl = idx(lod_x, lod_y + 1);
            let br = idx(lod_x + 1, lod_y + 1);

            indices.extend_from_slice(&[tl, bl, tr]);
            indices.extend_from_slice(&[tr, bl, br]);
        }
    }

    // --- Skirt vertices + indices ---
    // Walk the perimeter: left → bottom → right → top
    let mut skirt_edge: Vec<u32> = Vec::with_capacity(perimeter);

    // Left edge (x=0, y ascending)
    for y in 0..lod_height {
        skirt_edge.push(idx(0, y));
    }
    // Bottom edge (y=max, x ascending, skip first corner already added)
    for x in 1..lod_width {
        skirt_edge.push(idx(x, lod_height - 1));
    }
    // Right edge (x=max, y descending, skip first corner already added)
    for y in (0..lod_height - 1).rev() {
        skirt_edge.push(idx(lod_width - 1, y));
    }
    // Top edge (y=0, x descending, skip both corners already added)
    for x in (1..lod_width - 1).rev() {
        skirt_edge.push(idx(x, 0));
    }

    let skirt_start = positions.len() as u32;

    for &base_idx in &skirt_edge {
        let pos = positions[base_idx as usize];
        positions.push([pos[0], pos[1] - skirt_depth, pos[2]]);
        normals.push([0.0, 0.0, 0.0]); // zero normal marks skirt vertices
    }

    let skirt_len = skirt_edge.len();
    for i in 0..skirt_len {
        let base0 = skirt_edge[i];
        let base1 = skirt_edge[(i + 1) % skirt_len];
        let skirt0 = skirt_start + i as u32;
        let skirt1 = skirt_start + ((i + 1) % skirt_len) as u32;

        indices.extend_from_slice(&[base0, base1, skirt0, skirt0, base1, skirt1]);
    }

    // --- AABB from actual vertex heights ---
    let chunk_extent_x = (chunk_width - 1) as f32 * tile_size.0;
    let chunk_extent_z = (chunk_height - 1) as f32 * tile_size.1;
    let aabb = Aabb::from_min_max(
        Vec3::new(0.0, min_y - skirt_depth, 0.0),
        Vec3::new(chunk_extent_x, max_y, chunk_extent_z),
    );

    // --- Build mesh ---
    let mut mesh = Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::MAIN_WORLD | RenderAssetUsages::RENDER_WORLD,
    );
    mesh.insert_indices(Indices::U32(indices));
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, normals);

    (mesh, aabb)
}
