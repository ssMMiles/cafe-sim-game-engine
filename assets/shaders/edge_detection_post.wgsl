#import bevy_core_pipeline::fullscreen_vertex_shader::FullscreenVertexOutput

// Settings for edge detection
struct EdgeSettings {
    edge_thickness: f32,
    depth_sensitivity: f32,
    normal_sensitivity: f32,
    edge_color: vec4<f32>,
    enabled: u32,
}

@group(0) @binding(0) var screen_texture: texture_2d<f32>;
@group(0) @binding(1) var depth_texture: texture_depth_multisampled_2d;
@group(0) @binding(2) var normal_texture: texture_multisampled_2d<f32>;
@group(0) @binding(3) var texture_sampler: sampler;
@group(0) @binding(4) var<uniform> settings: EdgeSettings;

fn load_depth(pixel: vec2<i32>) -> f32 {
    return textureLoad(depth_texture, pixel, 0);
}

fn load_normal(pixel: vec2<i32>) -> vec3<f32> {
    let normal_sample = textureLoad(normal_texture, pixel, 0);
    // Prepass normals are packed from [-1, 1] to [0, 1]
    return normalize(normal_sample.xyz * 2.0 - vec3<f32>(1.0));
}

@fragment
fn fragment(in: FullscreenVertexOutput) -> @location(0) vec4<f32> {
    let dims = vec2<i32>(textureDimensions(screen_texture));
    let pixel_coords = vec2<i32>(in.position.xy);

    // Sample center values once and compare neighbors against them.
    // This is more stable than Sobel over interpolated normals and reduces
    // triangle artifacts that move as the camera rotates.
    let center_depth = load_depth(pixel_coords);
    let center_normal = load_normal(pixel_coords);

    let pixel_radius = max(1, i32(round(settings.edge_thickness)));

    var max_depth_delta = 0.0;
    var max_normal_delta = 0.0;

    for (var y: i32 = -1; y <= 1; y++) {
        for (var x: i32 = -1; x <= 1; x++) {
            if (x == 0 && y == 0) {
                continue;
            }

            let offset = vec2<i32>(x, y) * pixel_radius;
            let sample_coords = clamp(pixel_coords + offset, vec2<i32>(0), dims - vec2<i32>(1));

            let sample_depth = load_depth(sample_coords);
            let sample_normal = load_normal(sample_coords);

            max_depth_delta = max(max_depth_delta, abs(sample_depth - center_depth));

            // Angular difference proxy: 0 for aligned normals, 1 for orthogonal,
            // 2 for opposite.
            let normal_delta = 1.0 - dot(center_normal, sample_normal);
            max_normal_delta = max(max_normal_delta, normal_delta);
        }
    }

    let depth_edge = max_depth_delta * settings.depth_sensitivity;
    let normal_edge = max_normal_delta * settings.normal_sensitivity;
    let edge_strength = max(depth_edge, normal_edge);

    // Slightly higher threshold prevents crawling artifacts in shaded corners.
    let edge = smoothstep(0.02, 0.04, edge_strength);

    let base_color = textureSample(screen_texture, texture_sampler, in.uv);
    return mix(base_color, settings.edge_color, edge);
}