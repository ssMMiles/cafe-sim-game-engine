// Outline post-process: draws a line wherever the depth or normal prepass
// breaks. docs/EDGE_DETECTION.md explains every term and knob.

import bevy_core_pipeline::fullscreen_vertex_shader::FullscreenVertexOutput;

// Mirrors `EdgeDetectionSettings` (post_process.rs) field for field;
// `edge_settings_match_the_shader_struct` checks the size.
struct EdgeSettings {
    edge_color: vec4<f32>,
    depth_threshold: f32,
    grazing_bias: f32,
    // 1 − cos(crease angle).
    normal_threshold: f32,
    softness: f32,
    thickness: f32,
    fade_start: f32,
    fade_end: f32,
    near: f32,
    enabled: u32,
}

@group(0) @binding(0) var screen_texture: texture_2d<f32>;
@group(0) @binding(1) var depth_texture: texture_depth_multisampled_2d;
@group(0) @binding(2) var normal_texture: texture_multisampled_2d<f32>;
@group(0) @binding(3) var<uniform> settings: EdgeSettings;

// Sample 0 of the multisampled prepass. Every pixel reads the same sample
// position, so the offset is uniform across the screen and the plane
// property below still holds.
fn load_depth(pixel: vec2<i32>) -> f32 {
    return textureLoad(depth_texture, pixel, 0);
}

fn load_normal(pixel: vec2<i32>) -> vec3<f32> {
    // Prepass normals are world space, packed from [-1, 1] to [0, 1].
    let packed = textureLoad(normal_texture, pixel, 0).xyz;
    return normalize(packed * 2.0 - vec3<f32>(1.0));
}

// Depth discontinuity across `a – c – b` (three pixels on a line), relative
// to the threshold: ≥ 1 draws a line.
//
// Reverse-Z depth is `near / distance`, which is *affine in screen space
// across any plane*, so `a + b − 2c` is exactly zero on a flat surface at
// any viewing angle and only becomes non-zero where the surface breaks.
// Dividing by the nearest of the three makes it distance-independent: a
// step from distance z to z' reads as `1 − z/z'`.
//
// The threshold grows with the flatter side's own slope (`grazing_bias`):
// a depth change no bigger than what the surface already does from pixel
// to pixel is a bump seen edge-on, not an outline.
fn depth_strength(a: f32, c: f32, b: f32) -> f32 {
    let nearest = max(c, max(a, b));
    if (nearest <= 0.0) {
        return 0.0; // all three are sky (cleared depth)
    }
    let step = abs(a + b - 2.0 * c) / nearest;
    let slope = min(abs(a - c), abs(b - c)) / nearest;
    return step / (settings.depth_threshold + settings.grazing_bias * slope);
}

fn in_bounds(pixel: vec2<i32>, dims: vec2<i32>) -> bool {
    return all(pixel >= vec2<i32>(0)) && all(pixel < dims);
}

@fragment
fn fragment(in: FullscreenVertexOutput) -> @location(0) vec4<f32> {
    let dims = vec2<i32>(textureDimensions(screen_texture));
    let pixel = vec2<i32>(in.position.xy);
    let base = textureLoad(screen_texture, pixel, 0);

    let radius = max(1, i32(round(settings.thickness)));
    let c = load_depth(pixel);
    let n = load_normal(pixel);

    // The four lines through the pixel: horizontal, vertical, both diagonals.
    var axes = array<vec2<i32>, 4>(
        vec2<i32>(1, 0),
        vec2<i32>(0, 1),
        vec2<i32>(1, 1),
        vec2<i32>(1, -1),
    );

    var strength = 0.0;
    var nearest = c;
    for (var i = 0; i < 4; i++) {
        let offset = axes[i] * radius;
        let pa = pixel + offset;
        let pb = pixel - offset;
        let a_ok = in_bounds(pa, dims);
        let b_ok = in_bounds(pb, dims);

        // The second difference needs both sides. At the screen border it
        // would degenerate into a first difference, which is non-zero on
        // every sloped plane — so that line is skipped there instead.
        if (a_ok && b_ok) {
            let a = load_depth(pa);
            let b = load_depth(pb);
            strength = max(strength, depth_strength(a, c, b));
            nearest = max(nearest, max(a, b));
        }

        // Crease: angle between neighbouring normals, as 1 − cos.
        if (a_ok) {
            strength = max(strength, (1.0 - dot(n, load_normal(pa))) / settings.normal_threshold);
        }
        if (b_ok) {
            strength = max(strength, (1.0 - dot(n, load_normal(pb))) / settings.normal_threshold);
        }
    }

    // A soft band around the threshold so an edge that hovers near it fades
    // in and out instead of popping. Geometric antialiasing is FXAA's job:
    // this pass runs before it.
    let soft = max(settings.softness, 1e-3);
    let edge = smoothstep(1.0 - soft, 1.0 + soft, strength);

    // Fade with the distance of the nearest surface in the neighbourhood,
    // so a silhouette against the sky (cleared depth 0) fades with the
    // object rather than vanishing on the sky side.
    let distance = settings.near / max(nearest, 1e-9);
    let fade = 1.0 - smoothstep(settings.fade_start, settings.fade_end, distance);

    let amount = edge * fade * settings.edge_color.a;
    return vec4<f32>(mix(base.rgb, settings.edge_color.rgb, amount), base.a);
}
