#import bevy_pbr::{
    mesh_functions,
    view_transformations::position_world_to_clip,
    forward_io::{VertexOutput, Vertex},
}

@group(#{MATERIAL_BIND_GROUP}) @binding(100)
var heightmap_texture: texture_2d<f32>;

@group(#{MATERIAL_BIND_GROUP}) @binding(101)
var heightmap_sampler: sampler;

@group(#{MATERIAL_BIND_GROUP}) @binding(102)
var<uniform> config: TerrainConfig;

struct TerrainConfig {
    grid_size: vec2<f32>,      // [width, height] in vertices
    tile_size: vec2<f32>,      // world-space size per tile
    height_scale: f32,
}

@vertex
fn vertex(
    @builtin(vertex_index) vertex_index: u32,
    @builtin(instance_index) instance_index: u32,
) -> VertexOutput {
    var out: VertexOutput;
    
    // 1. Calculate grid coordinates from vertex index
    let grid_width = u32(config.grid_size.x);
    let x = vertex_index % grid_width;
    let z = vertex_index / grid_width;
    
    // 2. Calculate UV coordinates (0.0 to 1.0)
    let uv = vec2<f32>(
        f32(x) / (config.grid_size.x - 1.0),
        f32(z) / (config.grid_size.y - 1.0)
    );
    
    // 3. Sample heightmap
    let height = textureSampleLevel(
        heightmap_texture,
        heightmap_sampler,
        uv,
        0.0  // mip level
    ).r;
    
    // 4. Calculate world position
    let local_pos = vec3<f32>(
        f32(x) * config.tile_size.x,
        height * config.height_scale,
        f32(z) * config.tile_size.y
    );
    
    // 5. Calculate normal from heightmap
    let texel_size = 1.0 / config.grid_size;
    let h_left = textureSampleLevel(heightmap_texture, heightmap_sampler, uv + vec2(-texel_size.x, 0.0), 0.0).r;
    let h_right = textureSampleLevel(heightmap_texture, heightmap_sampler, uv + vec2(texel_size.x, 0.0), 0.0).r;
    let h_down = textureSampleLevel(heightmap_texture, heightmap_sampler, uv + vec2(0.0, -texel_size.y), 0.0).r;
    let h_up = textureSampleLevel(heightmap_texture, heightmap_sampler, uv + vec2(0.0, texel_size.y), 0.0).r;
    
    let tangent = normalize(vec3(2.0 * config.tile_size.x, (h_right - h_left) * config.height_scale, 0.0));
    let bitangent = normalize(vec3(0.0, (h_up - h_down) * config.height_scale, 2.0 * config.tile_size.y));
    let normal = normalize(cross(bitangent, tangent)); // Swapped order for correct Y-up direction
    
    // 6. Transform to world space
    let world_from_local = mesh_functions::get_world_from_local(instance_index);
    let world_pos = mesh_functions::mesh_position_local_to_world(world_from_local, vec4(local_pos, 1.0));
    
    // 7. Fill output
    out.position = position_world_to_clip(world_pos.xyz);
    out.world_position = world_pos;
    out.world_normal = mesh_functions::mesh_normal_local_to_world(normal, instance_index);

    return out;
}