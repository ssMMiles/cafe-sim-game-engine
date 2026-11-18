// Outline post-process: draws a line wherever the depth or normal prepass
// breaks. docs/EDGE_DETECTION.md explains every term and knob.
//
// The edge test runs once per MSAA sample of the prepass textures and the
// results are averaged (`constants::MSAA_SAMPLES`, set by the pipeline from
// the view's `Msaa`). This pass runs *after* the colour MSAA resolve, so that
// averaging is the only antialiasing an outline can get: each sample index
// sits at the same sub-pixel offset in every pixel, so each iteration traces
// the same line shifted by a fraction of a pixel, and the average is N+1
// coverage levels along it instead of a hard one-pixel staircase. (FXAA used
// to paper over that staircase — and blur everything else with it; removed
// 2026-11-17.)

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
    // The crease test's own fade band; silhouettes use the one above.
    crease_fade_start: f32,
    crease_fade_end: f32,
    near: f32,
    enabled: u32,
}

@group(0) @binding(0) var screen_texture: texture_2d<f32>;
// The prepass textures are multisampled exactly when the view's MSAA is on;
// the pipeline sets `MULTISAMPLED` (and the matching bind group layout) from
// the same sample count.
@if(MULTISAMPLED)
@group(0) @binding(1) var depth_texture: texture_depth_multisampled_2d;
@else
@group(0) @binding(1) var depth_texture: texture_depth_2d;
@if(MULTISAMPLED)
@group(0) @binding(2) var normal_texture: texture_multisampled_2d<f32>;
@else
@group(0) @binding(2) var normal_texture: texture_2d<f32>;
@group(0) @binding(3) var<uniform> settings: EdgeSettings;

// The third `textureLoad` argument is the sample index on a multisampled
// texture and the mip level otherwise, so the single-sampled variant ignores
// `index` (the loop runs once). Every pixel reads the same index, so each
// iteration's offsets are uniform across the screen and the plane property of
// `depth_strength` below still holds per sample.
fn load_depth(pixel: vec2<i32>, index: i32) -> f32 {
@if(MULTISAMPLED) {
    return textureLoad(depth_texture, pixel, index);
} @else {
    return textureLoad(depth_texture, pixel, 0);
}
}

// The raw normal-target texel: xyz is the world normal packed from [-1, 1]
// to [0, 1]; the 2-bit alpha is the instance's outline weight (grass writes
// a low one, buildings and terrain 1.0, and the clear value — sky — is 1.0
// too, so silhouettes against sky keep full strength).
fn load_packed_normal(pixel: vec2<i32>, index: i32) -> vec4<f32> {
@if(MULTISAMPLED) {
    return textureLoad(normal_texture, pixel, index);
} @else {
    return textureLoad(normal_texture, pixel, 0);
}
}

fn decode_normal(packed: vec4<f32>) -> vec3<f32> {
    return normalize(packed.xyz * 2.0 - vec3<f32>(1.0));
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

// One MSAA sample's edge amount at this pixel: the depth and crease tests
// thresholded separately (each softened, each with its own distance fade),
// then the stronger of the two scaled by the neighbourhood's outline weight.
fn sample_amount(pixel: vec2<i32>, dims: vec2<i32>, radius: i32, index: i32) -> f32 {
    let c = load_depth(pixel, index);
    let centre = load_packed_normal(pixel, index);
    let n = decode_normal(centre);

    // The four lines through the pixel: horizontal, vertical, both diagonals.
    var axes = array<vec2<i32>, 4>(
        vec2<i32>(1, 0),
        vec2<i32>(0, 1),
        vec2<i32>(1, 1),
        vec2<i32>(1, -1),
    );

    var depth_edge = 0.0;
    var crease_edge = 0.0;
    var nearest = c;
    // The *strongest* weight among the sampled texels, not the centre's: an
    // edge between a faint object and a full-weight one (grass against the
    // terrain, or against the sky's clear value) is somebody's silhouette
    // and keeps full strength. Only edges whose whole neighbourhood is faint
    // — a small object's interior creases and blade-over-blade steps — are
    // scaled down.
    var weight = centre.a;
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
            let a = load_depth(pa, index);
            let b = load_depth(pb, index);
            depth_edge = max(depth_edge, depth_strength(a, c, b));
            nearest = max(nearest, max(a, b));
        }

        // Crease: angle between neighbouring normals, as 1 − cos.
        if (a_ok) {
            let ta = load_packed_normal(pa, index);
            crease_edge = max(crease_edge, (1.0 - dot(n, decode_normal(ta))) / settings.normal_threshold);
            weight = max(weight, ta.a);
        }
        if (b_ok) {
            let tb = load_packed_normal(pb, index);
            crease_edge = max(crease_edge, (1.0 - dot(n, decode_normal(tb))) / settings.normal_threshold);
            weight = max(weight, tb.a);
        }
    }

    // A soft band around each threshold so an edge that hovers near it fades
    // in and out instead of popping. This is tonal softness; the geometric
    // antialiasing is the per-sample average in `fragment`. (With the fades
    // equal this maxing of two smoothsteps is exactly the old single
    // smoothstep of the maxed strengths — smoothstep is monotone.)
    let soft = max(settings.softness, 1e-3);
    let edge_depth = smoothstep(1.0 - soft, 1.0 + soft, depth_edge);
    let edge_crease = smoothstep(1.0 - soft, 1.0 + soft, crease_edge);

    // Fade with the distance of the nearest surface in the neighbourhood,
    // so a silhouette against the sky (cleared depth 0) fades with the
    // object rather than vanishing on the sky side. Creases have their own,
    // usually much shorter band: they are surface detail, and dense geometry
    // turns to scribble at range if they persist as far as silhouettes.
    let distance = settings.near / max(nearest, 1e-9);
    let fade = 1.0 - smoothstep(settings.fade_start, settings.fade_end, distance);
    let crease_fade = 1.0 - smoothstep(settings.crease_fade_start, settings.crease_fade_end, distance);

    return max(edge_depth * fade, edge_crease * crease_fade) * weight;
}

@fragment
fn fragment(in: FullscreenVertexOutput) -> @location(0) vec4<f32> {
    let dims = vec2<i32>(textureDimensions(screen_texture));
    let pixel = vec2<i32>(in.position.xy);
    let base = textureLoad(screen_texture, pixel, 0);

    let radius = max(1, i32(round(settings.thickness)));

    var coverage = 0.0;
    for (var s = 0; s < constants::MSAA_SAMPLES; s++) {
        coverage += sample_amount(pixel, dims, radius, s);
    }
    coverage = coverage / f32(constants::MSAA_SAMPLES);

    let amount = coverage * settings.edge_color.a;
    return vec4<f32>(mix(base.rgb, settings.edge_color.rgb, amount), base.a);
}
