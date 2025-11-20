#import bevy_core_pipeline::fullscreen_vertex_shader::FullscreenVertexOutput
#import bevy_pbr::prepass_utils

// Settings for edge detection
struct EdgeSettings {
    edge_thickness: f32,
    depth_sensitivity: f32,
    normal_sensitivity: f32,
    edge_color: vec4<f32>,
}

@group(0) @binding(0) var screen_texture: texture_2d<f32>;
@group(0) @binding(1) var texture_sampler: sampler;
@group(0) @binding(2) var<uniform> settings: EdgeSettings;

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
    let pixel_size = vec2<f32>(settings.edge_thickness, settings.edge_thickness);

    // Initialize Sobel accumulators
    var depth_sobel_x = 0.0;
    var depth_sobel_y = 0.0;
    var normal_sobel_x = vec3<f32>(0.0);
    var normal_sobel_y = vec3<f32>(0.0);

    // Sample 3x3 neighborhood
    var kernel_index = 0u;
    for (var y: i32 = -1; y <= 1; y++) {
        for (var x: i32 = -1; x <= 1; x++) {
            // Calculate screen position for this sample
            let offset = vec2<f32>(f32(x), f32(y)) * pixel_size;
            let sample_pos = in.position.xy + offset;

            // Sample depth using prepass utilities
            #ifdef MULTISAMPLED
                let depth = bevy_pbr::prepass_utils::prepass_depth(sample_pos, 0u);
            #else
                let depth = bevy_pbr::prepass_utils::prepass_depth(sample_pos, 0u);
            #endif

            // Sample normal using prepass utilities
            #ifdef MULTISAMPLED
                let normal = bevy_pbr::prepass_utils::prepass_normal(sample_pos, 0u);
            #else
                let normal = bevy_pbr::prepass_utils::prepass_normal(sample_pos, 0u);
            #endif

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

    // Threshold and sharpen
    let edge = smoothstep(0.05, 0.15, edge_strength);

    // Sample original toon-shaded color
    let base_color = textureSample(screen_texture, texture_sampler, in.uv);

    // Composite: blend edge color over base
    let final_color = mix(base_color, settings.edge_color, edge);

    return final_color;
}
