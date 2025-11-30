// Compute shader for pre-calculating terrain vertex positions and normals
// This runs once when the terrain is loaded or when the heightmap changes

struct TerrainConfig {
    grid_size: vec2<f32>,      // [width, height] in vertices
    tile_size: vec2<f32>,      // world-space size per tile
    height_scale: f32,
    _padding: f32,             // Padding for alignment
}

@group(0) @binding(0) var heightmap_texture: texture_2d<f32>;
@group(0) @binding(1) var heightmap_sampler: sampler;
@group(0) @binding(2) var<uniform> config: TerrainConfig;
@group(0) @binding(3) var<storage, read_write> positions: array<vec4<f32>>;
@group(0) @binding(4) var<storage, read_write> normals: array<vec4<f32>>;

@compute @workgroup_size(8, 8, 1)
fn compute_terrain(@builtin(global_invocation_id) global_id: vec3<u32>) {
    let x = global_id.x;
    let y = global_id.y;
    let width = u32(config.grid_size.x);
    let height = u32(config.grid_size.y);

    // Bounds check
    if (x >= width || y >= height) {
        return;
    }

    let index = x * height + y;

    // Calculate UV coordinates (0.0 to 1.0)
    let uv = vec2<f32>(
        f32(x) / f32(width - 1u),
        f32(y) / f32(height - 1u)
    );

    // Sample height from heightmap
    let h = textureSampleLevel(heightmap_texture, heightmap_sampler, uv, 0.0).r;

    // Calculate vertex position
    let pos_x = f32(x) * config.tile_size.x;
    let pos_y = h * config.height_scale;
    let pos_z = f32(y) * config.tile_size.y;
    positions[index] = vec4<f32>(pos_x, pos_y, pos_z, 1.0);

    // Calculate normal using neighbor heights
    let texel_size = 1.0 / config.grid_size;

    let h_left = textureSampleLevel(
        heightmap_texture,
        heightmap_sampler,
        uv + vec2<f32>(-texel_size.x, 0.0),
        0.0
    ).r;

    let h_right = textureSampleLevel(
        heightmap_texture,
        heightmap_sampler,
        uv + vec2<f32>(texel_size.x, 0.0),
        0.0
    ).r;

    let h_down = textureSampleLevel(
        heightmap_texture,
        heightmap_sampler,
        uv + vec2<f32>(0.0, -texel_size.y),
        0.0
    ).r;

    let h_up = textureSampleLevel(
        heightmap_texture,
        heightmap_sampler,
        uv + vec2<f32>(0.0, texel_size.y),
        0.0
    ).r;

    // Calculate tangent and bitangent
    let tangent = normalize(vec3<f32>(
        2.0 * config.tile_size.x,
        (h_right - h_left) * config.height_scale,
        0.0
    ));

    let bitangent = normalize(vec3<f32>(
        0.0,
        (h_up - h_down) * config.height_scale,
        2.0 * config.tile_size.y
    ));

    // Calculate normal via cross product (swapped order for Y-up)
    let normal = normalize(cross(bitangent, tangent));
    normals[index] = vec4<f32>(normal, 0.0);
}
