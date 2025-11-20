#import bevy_sprite::mesh2d_vertex_output::VertexOutput

// Texture bindings
@group(2) @binding(0) var color_texture: texture_2d<f32>;
@group(2) @binding(1) var color_sampler: sampler;
@group(2) @binding(2) var depth_texture: texture_depth_2d;
@group(2) @binding(3) var depth_sampler: sampler;
@group(2) @binding(4) var normal_texture: texture_2d<f32>;
@group(2) @binding(5) var normal_sampler: sampler;

// Material uniforms
struct EdgeDetectionSettings {
    edge_thickness: f32,      // Line thickness (pixel size multiplier)
    depth_sensitivity: f32,   // How sensitive to depth changes
    normal_sensitivity: f32,  // How sensitive to normal changes
    edge_color: vec4<f32>,    // Color of the outlines
}

@group(2) @binding(6) var<uniform> settings: EdgeDetectionSettings;

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
fn fragment(in: VertexOutput) -> @location(0) vec4<f32> {
    let resolution = vec2<f32>(textureDimensions(color_texture));
    let texel_size = settings.edge_thickness / resolution;

    // Initialize Sobel accumulators
    var depth_sobel_x = 0.0;
    var depth_sobel_y = 0.0;
    var normal_sobel_x = vec3<f32>(0.0);
    var normal_sobel_y = vec3<f32>(0.0);

    // Sample 3x3 neighborhood
    var kernel_index = 0u;
    for (var y: i32 = -1; y <= 1; y++) {
        for (var x: i32 = -1; x <= 1; x++) {
            let offset = vec2<f32>(f32(x), f32(y)) * texel_size;
            let sample_uv = in.uv + offset;

            // Sample depth
            let depth = textureSample(depth_texture, depth_sampler, sample_uv);

            // Sample normal (world-space, encoded in RGB)
            let normal = textureSample(normal_texture, normal_sampler, sample_uv).rgb;

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
    let depth_edge = sqrt(
        depth_sobel_x * depth_sobel_x +
        depth_sobel_y * depth_sobel_y
    ) * settings.depth_sensitivity;

    // Calculate edge strength from normal discontinuities
    let normal_edge_x = length(normal_sobel_x);
    let normal_edge_y = length(normal_sobel_y);
    let normal_edge = sqrt(
        normal_edge_x * normal_edge_x +
        normal_edge_y * normal_edge_y
    ) * settings.normal_sensitivity;

    // Combine edge signals (max for sharper edges)
    let edge_strength = max(depth_edge, normal_edge);

    // Threshold and sharpen (adjust thresholds to taste)
    let edge = smoothstep(0.05, 0.15, edge_strength);

    // Sample original color
    let base_color = textureSample(color_texture, color_sampler, in.uv);

    // Composite: blend edge color over base
    let final_color = mix(base_color, settings.edge_color, edge);

    return final_color;
}
