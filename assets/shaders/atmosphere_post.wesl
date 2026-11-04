// Atmosphere post-process: distance fog, then god rays. One pass over the
// scene colour and the depth prepass. docs/ATMOSPHERE.md explains the model
// and every knob.
//
// Everything here reads depth with `textureLoad` — no sampler, no implicit
// LOD, no derivatives — so the whole shader is safe under WebGPU's uniform
// control flow rule even inside the god-ray loop.

import bevy_core_pipeline::fullscreen_vertex_shader::FullscreenVertexOutput;

// Mirrors `AtmosphereSettings` (atmosphere.rs) field for field;
// `atmosphere_settings_match_the_shader_struct` checks the size.
struct AtmosphereSettings {
    // Unprojects a clip-space point straight to world space.
    world_from_clip: mat4x4<f32>,
    // xyz = camera world position.
    camera: vec4<f32>,
    // rgb = fog colour (linear), w = maximum opacity.
    fog_color: vec4<f32>,
    // density per metre, start distance, height falloff (0 = uniform), height base.
    fog_params: vec4<f32>,
    // rgb = sun colour × tint strength, w = tint exponent.
    fog_sun: vec4<f32>,
    // xyz = world-space direction *to* the sun.
    sun_dir: vec4<f32>,
    // xy = the sun's position in uv, z = visibility 0..1, w = ray spread.
    sun_screen: vec4<f32>,
    // rgb = sun colour × ray intensity, w = per-sample decay.
    ray_color: vec4<f32>,
    // fog on, rays on, fog quantisation steps (0 = smooth), ray sample count.
    flags: vec4<u32>,
}

@group(0) @binding(0) var screen_texture: texture_2d<f32>;
@group(0) @binding(1) var depth_texture: texture_depth_multisampled_2d;
@group(0) @binding(2) var<uniform> settings: AtmosphereSettings;

// The loop bound has to be a constant for the compiler to unroll sensibly;
// the uniform sample count breaks out early. Keep in step with
// `AtmosphereStyle::rays_samples`' range.
const MAX_RAY_SAMPLES: u32 = 64u;

// Saturation guards. Fog is opaque long before an optical depth of 50, and
// clamping the height exponent keeps `exp` away from overflow when the
// camera sits far below the fog base.
const MAX_OPTICAL_DEPTH: f32 = 50.0;
const MAX_HEIGHT_EXPONENT: f32 = 30.0;

fn load_depth(pixel: vec2<i32>) -> f32 {
    return textureLoad(depth_texture, pixel, 0);
}

// Optical depth along a segment of length `len` rising from `y0` to `y1`
// through a density that falls off exponentially with height:
//
//     ρ(y) = density · exp(−(y − base) / falloff)
//
// The integral has a closed form, `ρ(y0) · (1 − e^−k)/k · len` with
// `k = Δy/falloff`, whose k→0 limit is the uniform case. `falloff <= 0`
// means "no height variation" and takes that uniform path directly.
//
// Mirrored by `optical_depth` in atmosphere.rs, which the tests measure
// against numerical integration.
fn optical_depth(density: f32, len: f32, y0: f32, y1: f32, falloff: f32, base: f32) -> f32 {
    if (falloff <= 0.0) {
        return min(density * len, MAX_OPTICAL_DEPTH);
    }
    let exponent = clamp(-(y0 - base) / falloff, -MAX_HEIGHT_EXPONENT, MAX_HEIGHT_EXPONENT);
    let rho0 = density * exp(exponent);
    let k = clamp((y1 - y0) / falloff, -MAX_HEIGHT_EXPONENT, MAX_HEIGHT_EXPONENT);
    var factor = 1.0 - 0.5 * k; // series limit, used where (1−e^−k)/k loses precision
    if (abs(k) > 1e-4) {
        factor = (1.0 - exp(-k)) / k;
    }
    return min(rho0 * factor * len, MAX_OPTICAL_DEPTH);
}

@fragment
fn fragment(in: FullscreenVertexOutput) -> @location(0) vec4<f32> {
    let pixel = vec2<i32>(in.position.xy);
    var color = textureLoad(screen_texture, pixel, 0);

    let depth = load_depth(pixel);
    // Cleared reverse-Z depth: nothing was drawn here. The sky dome writes no
    // depth (its pipeline disables depth writes in the prepass too), so the
    // sky lands here and is deliberately left alone — `SkyColors` already
    // carries the horizon.
    let is_sky = depth <= 0.0;

    if (settings.flags.x != 0u && !is_sky) {
        // Unproject to world space. Only valid off the far plane, which is
        // why this sits inside the `!is_sky` branch.
        let ndc = vec3<f32>(in.uv.x * 2.0 - 1.0, 1.0 - in.uv.y * 2.0, depth);
        let unprojected = settings.world_from_clip * vec4<f32>(ndc, 1.0);
        let world_pos = unprojected.xyz / unprojected.w;

        let camera = settings.camera.xyz;
        let to_fragment = world_pos - camera;
        let dist = length(to_fragment);
        let dir = to_fragment / max(dist, 1e-6);

        // Fog starts a set distance out, so nearby geometry stays crisp.
        let start = settings.fog_params.y;
        let segment = max(dist - start, 0.0);
        let entry = camera + dir * min(start, dist);

        let od = optical_depth(
            settings.fog_params.x,
            segment,
            entry.y,
            world_pos.y,
            settings.fog_params.z,
            settings.fog_params.w,
        );
        var fog = (1.0 - exp(-od)) * settings.fog_color.w;

        // Quantising the fog *amount* keeps distance reading in flat steps,
        // like the toon curve does for brightness. 0 leaves it smooth.
        let steps = settings.flags.z;
        if (steps > 0u) {
            fog = round(fog * f32(steps)) / f32(steps);
        }

        // Looking towards the sun, the fog takes on its colour.
        let tint = pow(max(dot(dir, settings.sun_dir.xyz), 0.0), settings.fog_sun.w);
        let fog_rgb = settings.fog_color.rgb + settings.fog_sun.rgb * tint;
        color = vec4<f32>(mix(color.rgb, fog_rgb, fog), color.a);
    }

    let sun_visibility = settings.sun_screen.z;
    if (settings.flags.y != 0u && sun_visibility > 0.0) {
        // March from this pixel towards the sun, counting how much of the
        // path is open sky. Occlusion comes from the depth buffer, so
        // anything that wrote depth blocks the shaft — no shadow map.
        let dims = vec2<f32>(textureDimensions(screen_texture));
        let last = vec2<i32>(dims) - vec2<i32>(1);
        let samples = clamp(settings.flags.w, 1u, MAX_RAY_SAMPLES);
        let step = (settings.sun_screen.xy - in.uv) * settings.sun_screen.w / f32(samples);
        let decay = settings.ray_color.w;

        var probe = in.uv;
        var weight = 1.0;
        var open = 0.0;
        for (var i = 0u; i < MAX_RAY_SAMPLES; i = i + 1u) {
            if (i >= samples) {
                break;
            }
            probe = probe + step;
            let coord = clamp(vec2<i32>(probe * dims), vec2<i32>(0), last);
            if (load_depth(coord) <= 0.0) {
                open = open + weight;
            }
            weight = weight * decay;
        }
        open = open / f32(samples);

        color = vec4<f32>(
            color.rgb + settings.ray_color.rgb * open * sun_visibility,
            color.a,
        );
    }

    return color;
}
