// Toon fragment shader for terrain chunks.
//
// All lighting is baked (see toon_lighting.wesl for the model); this shader
// only adds the stochastic stone texture blend on top of the base colour.

import bevy_pbr::render::{
    pbr_fragment::pbr_input_from_vertex_output,
    pbr_bindings,
};

@if(PREPASS_PIPELINE)
import bevy_pbr::{
    prepass::io::{VertexOutput, FragmentOutput},
    deferred::functions::deferred_output,
};
@else
import bevy_pbr::render::forward_io::{VertexOutput, FragmentOutput};

@group(constants::MATERIAL_BIND_GROUP) @binding(100)
var stone_texture: texture_2d<f32>;
@group(constants::MATERIAL_BIND_GROUP) @binding(101)
var stone_sampler: sampler;

@group(constants::MATERIAL_BIND_GROUP) @binding(102)
var lightmap_texture: texture_2d<f32>;
@group(constants::MATERIAL_BIND_GROUP) @binding(103)
var lightmap_sampler: sampler;

// Mirrors `TerrainShading` in terrain/material.rs field for field.
struct TerrainShading {
    // 1 = shade from the baked lightmap, 0 = flat unlit debug view.
    use_lightmap: u32,
    _pad: vec3<f32>,
    // The toon ramp: (bands, floor, white, gamma). The same values the
    // object materials carry — see `toon_shade`.
    curve: vec4<f32>,
};
@group(constants::MATERIAL_BIND_GROUP) @binding(104)
var<uniform> terrain: TerrainShading;

// ─── TOON CURVE ──────────────────────────────────────────────────────────
// Everything between this marker and END TOON CURVE is byte-identical
// between `toon_lighting.wesl` and `terrain_toon.wesl`, and mirrored in
// `engine-api/src/toon_curve.rs`.
// Terrain and the objects standing on it meet along every contact shadow, so
// a ramp that differs between them draws a step right at the join; the Rust
// copy is what the tests measure and what the tables in docs/LIGHTING.md are
// computed from. `toon_curve_matches_the_shaders` holds all three in step —
// these are asset-path shaders with no shared import module, so the check is
// textual.

// The ramp, as (bands, floor, white, gamma). Per world, not per shader: see
// `ToonCurve`. Unpacked through these accessors so the three uses of `white`
// and `gamma` can't drift apart.
fn toon_bands(curve: vec4<f32>) -> f32 { return curve.x; }
fn toon_floor(curve: vec4<f32>) -> f32 { return curve.y; }
fn toon_white(curve: vec4<f32>) -> f32 { return curve.z; }
fn toon_gamma(curve: vec4<f32>) -> f32 { return curve.w; }

// Perceptual luminance from a linear RGB color
fn luminance(color: vec3<f32>) -> f32 {
    return dot(color, vec3<f32>(0.2126, 0.7152, 0.0722));
}

// Quantize a scalar value into discrete toon bands, with each band boundary
// resolved over roughly one screen pixel.
//
// A plain `floor(value * bands) / bands` is a hard threshold applied to a
// bilinearly filtered texture, so the contour it traces is reconstructed at
// *texel* granularity and then aliases against the pixel grid — the jagged
// staircase along every shadow edge. Raising lightmap resolution only shrinks
// the steps; there is always exactly one texel of staircase, so it stays
// visible up close no matter how many texels you spend.
//
// `fwidth` is how fast the banded coordinate changes per screen pixel, so
// ramping across it makes the transition one pixel wide regardless of
// lightmap resolution, surface size, or viewing distance. Flat regions have
// fwidth ~ 0 and stay perfectly flat, so the banded look survives intact —
// only the boundary itself is resolved smoothly. At grazing angles and
// distance a whole band can fall inside one pixel, where the clamp lets the
// bands dissolve into smooth shading instead of shimmering.
//
// The ramp sits in the top of each band and ends exactly on the boundary,
// which keeps the function continuous across it: at the top of band `f` the
// value has already reached `(f + 1) / bands`, which is where band `f + 1`
// starts.
fn toon_band(value: f32, bands: f32) -> f32 {
    let x = clamp(value, 0.0, 1.0) * bands;
    let f = floor(x);
    let t = x - f;
    let w = clamp(fwidth(x), 1e-5, 1.0);
    return (f + smoothstep(1.0 - w, 1.0, t)) / bands;
}

// Quantize brightness into toon bands while preserving the hue of coloured
// light. Banding each channel separately would shift hue at band edges.
//
// Three separate decisions, which used to be one constant (`TOON_FLOOR =
// 0.18`) doing all three jobs at once — which is why the floor could not be
// lowered without the whole scene misbehaving:
//
//  * `white` is the exposure: the radiance that reads as fully lit. It is
//    applied to the *input* only.
//  * `gamma` spaces the bands by perceived brightness rather than by
//    radiance. At gamma 1 the boundaries sit at 0.2/0.4/0.6/0.8 of white,
//    and since a sunlit surface is about 10x a shaded one, everything in
//    shade lands in band 0 and four of the five bands cover the top octave
//    alone. Above 1 the boundaries move down to `white * (k/bands)^gamma`,
//    so ordinary shade occupies real bands and the floor is reached only by
//    genuine darkness.
//  * `floor` is the darkest displayed fraction of base colour, and nothing
//    else. It may be 0.
//
// `level` is the output luminance whatever the hue mix below does: both mix
// endpoints have exactly that luminance, so the curve is monotone in `band`.
// An earlier form mixed a constant floor with a value climbing from zero and
// dipped 26% below the floor in between — a one-pixel dark line along every
// contour where the lightmap crossed the first boundary, i.e. the edge of
// every shadow in the game.
//
// The bottom band is neutral rather than hue-preserving, so a trace of
// tinted light reaching a dark corner doesn't paint the whole corner that
// colour. That fade is one *band* wide, so it completes exactly at the first
// boundary and above it the hue is kept exactly. Measuring it in bands and
// not in floor units is what makes a floor of zero well defined: the old
// `smoothstep(0.0, TOON_FLOOR, band)` became `smoothstep(0, 0, band)` there,
// which WGSL leaves undefined and naga lowers to a divide — NaN for every
// fragment on screen, not just the dark ones.
fn toon_shade(radiance: vec3<f32>, curve: vec4<f32>) -> vec3<f32> {
    let bands = toon_bands(curve);
    let inv_gamma = 1.0 / toon_gamma(curve);
    let lum = luminance(radiance);

    // Quantise perceived brightness, not radiance.
    let x = pow(clamp(lum / toon_white(curve), 0.0, 1.0), inv_gamma);
    let band = toon_band(x, bands);

    // Back to linear for display, spaced evenly in the same space. `white`
    // deliberately does not appear: it chooses which radiance reaches the
    // top band, not how bright the top band is (that is always 1.0).
    let floor_x = pow(clamp(toon_floor(curve), 0.0, 1.0), inv_gamma);
    let level = pow(floor_x + (1.0 - floor_x) * band, toon_gamma(curve));

    let tinted = radiance * (level / max(lum, 1e-4));
    return mix(vec3<f32>(level), tinted, smoothstep(0.0, 1.0 / bands, band));
}
// ─── END TOON CURVE ──────────────────────────────────────────────────────

// Scale controlling how many world units span one texture repeat.
// Larger = stones appear larger / more spread out.
const STONE_UV_SCALE: f32 = 3.0;

// How much to shift the terrain base color toward stone gray at outlines.
const STONE_BLEND_STRENGTH: f32 = 0.85;

// Stone color (dark gray)
const STONE_COLOR: vec3<f32> = vec3<f32>(0.02, 0.02, 0.02);

// Cheap 2D hash — maps an integer-lattice vec2 to a random [0,1]^2 offset
fn hash2(p: vec2<f32>) -> vec2<f32> {
    let q = vec2<f32>(
        dot(p, vec2<f32>(127.1, 311.7)),
        dot(p, vec2<f32>(269.5, 183.3))
    );
    return fract(sin(q) * 43758.5453);
}

// Stochastic (no-tile) texture sample.
// Blends four neighboring lattice cells, each with a unique random UV offset,
// so the repeating grid of the texture becomes invisible.
// Uses a fixed mip level to avoid automatic mip selection blurring sparse textures to white.
fn sample_no_tile(uv: vec2<f32>) -> vec4<f32> {
    let p = floor(uv);
    let f = fract(uv);
    // Smoothstep blend weights
    let w = f * f * (3.0 - 2.0 * f);

    let va = textureSampleLevel(stone_texture, stone_sampler, uv + hash2(p),                        1.0);
    let vb = textureSampleLevel(stone_texture, stone_sampler, uv + hash2(p + vec2<f32>(1.0, 0.0)), 1.0);
    let vc = textureSampleLevel(stone_texture, stone_sampler, uv + hash2(p + vec2<f32>(0.0, 1.0)), 1.0);
    let vd = textureSampleLevel(stone_texture, stone_sampler, uv + hash2(p + vec2<f32>(1.0, 1.0)), 1.0);

    return mix(mix(va, vb, w.x), mix(vc, vd, w.x), w.y);
}

@fragment
fn fragment(
    in: VertexOutput,
    @builtin(front_facing) is_front: bool,
) -> FragmentOutput {
    let double_sided = false;
    var pbr_input = pbr_input_from_vertex_output(in, is_front, double_sided);
    // Read base color directly from the material uniform — we skip
    // pbr_input_from_standard_material because terrain has no UV0 and that
    // function assumes uv is defined whenever any UV set is present.
    pbr_input.material.base_color = pbr_bindings::material.base_color;
    pbr_input.material.flags = pbr_bindings::material.flags;

    // Sample with stochastic UV offset to break up visible tiling.
    // Fixed mip level avoids automatic selection blurring sparse textures to white at distance.
    let stone_uv = in.world_position.xz / STONE_UV_SCALE;
    let stone_sample = sample_no_tile(stone_uv);
    let stone_factor = smoothstep(0.05, 0.45, 1.0 - stone_sample.r) * STONE_BLEND_STRENGTH;

    pbr_input.material.base_color = vec4<f32>(
        mix(pbr_input.material.base_color.rgb, STONE_COLOR, stone_factor),
        pbr_input.material.base_color.a,
    );

    // White = fully lit (unlit debug view / unbaked chunk). Computed
    // unconditionally; unused in the deferred prepass — see toon_lighting.wesl.
    var radiance = vec3<f32>(1.0);
    if (terrain.use_lightmap == 1u) {
@if(VERTEX_UVS_B)
        radiance = textureSample(lightmap_texture, lightmap_sampler, in.uv_b).rgb;
    }

@if(PREPASS_PIPELINE)
    let out = deferred_output(in, pbr_input);
@if(!PREPASS_PIPELINE)
    var out: FragmentOutput;
@if(!PREPASS_PIPELINE)
    out.color = vec4<f32>(
        pbr_input.material.base_color.rgb * toon_shade(radiance, terrain.curve),
        pbr_input.material.base_color.a,
    );

    return out;
}
