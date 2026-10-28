// Toon fragment shader for placed meshes.
//
// All lighting is baked: the lightmap holds outgoing radiance in "lightmap
// units" (1.0 = full base colour) with the sun, ambient×AO, and — soon —
// point/spot lights already folded in. The runtime does no light evaluation
// at all: sample, band, multiply. That keeps the WASM/WebGPU path free of
// shadow maps and clustered lighting entirely.

import bevy_pbr::render::{
    pbr_fragment::pbr_input_from_vertex_output,
    pbr_bindings,
    pbr_types,
    mesh_view_bindings as view_bindings,
};

@if(PREPASS_PIPELINE)
import bevy_pbr::{
    prepass::io::{VertexOutput, FragmentOutput},
    deferred::functions::deferred_output,
};
@else
import bevy_pbr::render::forward_io::{VertexOutput, FragmentOutput};

@group(constants::MATERIAL_BIND_GROUP) @binding(100)
var lightmap_texture: texture_2d<f32>;
@group(constants::MATERIAL_BIND_GROUP) @binding(101)
var lightmap_sampler: sampler;

struct ToonFlags {
    use_lightmap: u32,
};
@group(constants::MATERIAL_BIND_GROUP) @binding(102)
var<uniform> toon_flags: ToonFlags;

const TOON_BANDS: f32 = 5.0;
// Darkest a surface can get, as a fraction of its base colour. Fully
// shadowed geometry reads as a dark tint rather than dropping to pure black.
const TOON_FLOOR: f32 = 0.18;

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
//
// NOTE: duplicated verbatim in `terrain_toon.wesl` — the two shaders must
// band identically or terrain and objects disagree where they meet. These
// are asset-path shaders with no shared import module; keep them in step by
// hand.
fn toon_band(value: f32, bands: f32) -> f32 {
    let x = clamp(value, 0.0, 1.0) * bands;
    let f = floor(x);
    let t = x - f;
    let w = clamp(fwidth(x), 1e-5, 1.0);
    return (f + smoothstep(1.0 - w, 1.0, t)) / bands;
}

// Quantize brightness into toon bands while preserving the hue of coloured
// light. Banding each channel separately would shift hue at band edges.
// Anything in the bottom band reads as a neutral TOON_FLOOR: neutral rather
// than hue-preserving, so a trace of tinted light reaching a dark corner
// doesn't paint the whole corner that colour.
//
// That floor used to be an `if`, which is one more hard threshold and
// aliased exactly like an un-antialiased band edge. It is a cross-fade now,
// over a range the banded value only ever crosses inside the bottom band —
// so above the first boundary this is bit-for-bit the old behaviour.
fn toon_shade(radiance: vec3<f32>) -> vec3<f32> {
    let lum = luminance(radiance);
    let band = toon_band(lum, TOON_BANDS);
    let tinted = radiance * (band / max(lum, 1e-4));
    return mix(vec3<f32>(TOON_FLOOR), tinted, smoothstep(0.0, TOON_FLOOR, band));
}

@fragment
fn fragment(
    in: VertexOutput,
    @builtin(front_facing) is_front: bool,
) -> FragmentOutput {
    let double_sided = false;
    var pbr_input = pbr_input_from_vertex_output(in, is_front, double_sided);
    // Read base color from the material uniform directly to avoid
    // pbr_input_from_standard_material's dependency on uv (VERTEX_UVS_A) when
    // only VERTEX_UVS_B is defined on the mesh.
    pbr_input.material.base_color = pbr_bindings::material.base_color;
    pbr_input.material.flags = pbr_bindings::material.flags;

    // Mix in base_color_texture when one is bound and the mesh ships a
    // primary UV channel. Mirrors the StandardMaterial path in
    // pbr_fragment::pbr_input_from_standard_material; guarded on
    // VERTEX_UVS_A so lightmap-only meshes still compile.
@if(VERTEX_UVS_A)
    if ((pbr_input.material.flags & pbr_types::STANDARD_MATERIAL_FLAGS_BASE_COLOR_TEXTURE_BIT) != 0u) {
        let uv = (pbr_bindings::material.uv_transform * vec3<f32>(in.uv, 1.0)).xy;
        pbr_input.material.base_color = pbr_input.material.base_color * textureSampleBias(
            pbr_bindings::base_color_texture,
            pbr_bindings::base_color_sampler,
            uv,
            view_bindings::view.mip_bias,
        );
    }


    // White = fully lit. Used for the unlit debug view and for meshes that
    // have no lightmap UVs at all. Computed unconditionally: in the deferred
    // prepass it is simply unused, which is the same trade bevy_pbr makes in
    // `pbr.wesl` (WESL `@if` attaches to one statement, so the forward-only
    // branch is kept to the statements that need the guard).
    var radiance = vec3<f32>(1.0);
    if (toon_flags.use_lightmap == 1u) {
@if(VERTEX_UVS_B)
        radiance = textureSample(lightmap_texture, lightmap_sampler, in.uv_b).rgb;
    }

@if(PREPASS_PIPELINE)
    let out = deferred_output(in, pbr_input);
@if(!PREPASS_PIPELINE)
    var out: FragmentOutput;
@if(!PREPASS_PIPELINE)
    out.color = vec4<f32>(
        pbr_input.material.base_color.rgb * toon_shade(radiance),
        pbr_input.material.base_color.a,
    );

    return out;
}
