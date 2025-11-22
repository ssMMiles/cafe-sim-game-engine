#import bevy_core_pipeline::fullscreen_vertex_shader::FullscreenVertexOutput

// Settings for edge detection
struct EdgeSettings {
    edge_thickness: f32,
    depth_sensitivity: f32,
    normal_sensitivity: f32,
    edge_color: vec4<f32>,
}

@group(0) @binding(0) var screen_texture: texture_2d<f32>;
@group(0) @binding(1) var depth_texture: texture_depth_multisampled_2d;
@group(0) @binding(2) var normal_texture: texture_multisampled_2d<f32>;
@group(0) @binding(3) var texture_sampler: sampler;
@group(0) @binding(4) var<uniform> settings: EdgeSettings;

// Sobel kernels for edge detection
const SOBEL_X: array<f32, 9> = array<f32, 9>(
    -1.0, 0.0, 1.0,
    -2.0, 0.0, 2.0,
    -1.0, 0.0, 1.0
);

const SOBEL_Y: array<f32, 9> = array<f32, 9>(
    -1.0, -2.0, -1.0,
     0.0,  0.0,  0.0,
     1.0,  2.0,  1.0
);

@fragment
fn fragment(in: FullscreenVertexOutput) -> @location(0) vec4<f32> {
    let resolution = vec2<f32>(textureDimensions(screen_texture));
    let texel_size = settings.edge_thickness / resolution;

    // Convert UV to pixel coordinates for textureLoad
    let pixel_coords = vec2<i32>(in.position.xy);

    // Initialize Sobel accumulators
    var depth_sobel_x = 0.0;
    var depth_sobel_y = 0.0;
    var normal_sobel_x = vec3<f32>(0.0);
    var normal_sobel_y = vec3<f32>(0.0);

    // Sample 3x3 neighborhood
    var kernel_index = 0u;
    for (var y: i32 = -1; y <= 1; y++) {
        for (var x: i32 = -1; x <= 1; x++) {
            let sample_coords = pixel_coords + vec2<i32>(x, y);

            // Sample depth (using textureLoad for multisampled depth textures, sample index 0)
            let depth = textureLoad(depth_texture, sample_coords, 0);

            // Sample normal (stored as RGB10A2, need to unpack from [0,1] to [-1,1])
            // For multisampled textures, we also need to specify the sample index
            let normal_sample = textureLoad(normal_texture, sample_coords, 0);
            let normal = normalize(normal_sample.xyz * 2.0 - vec3<f32>(1.0));

            // Apply Sobel kernels
            let sobel_weight_x = SOBEL_X[kernel_index];
            let sobel_weight_y = SOBEL_Y[kernel_index];

            depth_sobel_x += depth * sobel_weight_x;
            depth_sobel_y += depth * sobel_weight_y;

            normal_sobel_x += normal * sobel_weight_x;
            normal_sobel_y += normal * sobel_weight_y;

            kernel_index++;
        }
    }

    // Calculate edge strength from depth discontinuities
    // This detects geometric boundaries (object edges)
    let depth_edge = sqrt(
        depth_sobel_x * depth_sobel_x +
        depth_sobel_y * depth_sobel_y
    ) * settings.depth_sensitivity;

    // Calculate edge strength from normal discontinuities
    // This detects surface angle changes (creases, folds)
    let normal_edge_x = length(normal_sobel_x);
    let normal_edge_y = length(normal_sobel_y);
    let normal_edge = sqrt(
        normal_edge_x * normal_edge_x +
        normal_edge_y * normal_edge_y
    ) * settings.normal_sensitivity;

    // Combine edge signals (max for sharper edges)
    // Using max ensures either geometric edges OR surface angle changes trigger detection
    let edge_strength = max(depth_edge, normal_edge);

    // Threshold and sharpen
    let edge = smoothstep(0.05, 0.15, edge_strength);

    // Sample original toon-shaded color
    let base_color = textureSample(screen_texture, texture_sampler, in.uv);

    // Composite: blend edge color over base
    let final_color = mix(base_color, settings.edge_color, edge);

    return final_color;
}
