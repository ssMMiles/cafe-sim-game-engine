// Toon fragment shader for placed meshes.
//
// All lighting is baked: the lightmap holds outgoing radiance in "lightmap
// units" (1.0 = full base colour) with the sun, ambient×AO, terrain bounce
// and every placed point/spot light already folded in. The runtime does no
// light evaluation at all: sample, band, multiply (see docs/LIGHTING.md).
//
// Things that move can't carry a lightmap, so they sample the irradiance
// probe volume instead (`ToonShading.source == 2`): four 3D textures holding
// L1 coefficients and the sun's visibility on a toroidal world lattice, from
// which the same radiance is reconstructed per pixel — see
// docs/DYNAMIC_LIGHTING_PLAN.md §8. Either way the banding below is shared. That keeps the WASM/WebGPU path free of
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

// Mirrors `ToonShading` in engine-api's toon_material.rs field for field.
struct ToonShading {
    // 1 = lightmap, 2 = probe volume.
    source: u32,
    // 1 = flat unlit debug view (Shift+L).
    unlit: u32,
    probe_cell_size: f32,
    _pad: f32,
    probe_dims: vec4<f32>,
    // Unit direction toward the sun, and its radiance in lightmap units.
    sun_dir: vec4<f32>,
    sun_rgb: vec4<f32>,
};
@group(constants::MATERIAL_BIND_GROUP) @binding(102)
var<uniform> toon: ToonShading;

// The probe volume. c0.rgb is the constant term, c0.a the sun visibility;
// c1..c3 multiply n.x, n.y, n.z. The sampler repeats: cell i lives in texel
// i mod dims, so world space maps straight onto the texture with no origin.
@group(constants::MATERIAL_BIND_GROUP) @binding(103)
var probe_c0: texture_3d<f32>;
@group(constants::MATERIAL_BIND_GROUP) @binding(104)
var probe_sampler: sampler;
@group(constants::MATERIAL_BIND_GROUP) @binding(105)
var probe_c1: texture_3d<f32>;
@group(constants::MATERIAL_BIND_GROUP) @binding(106)
var probe_c2: texture_3d<f32>;
@group(constants::MATERIAL_BIND_GROUP) @binding(107)
var probe_c3: texture_3d<f32>;

// Radiance a surface with normal `n` at `world_pos` receives, from the probe
// volume: the clamped L1 part plus the sun evaluated exactly against the
// probe's stored visibility (a hard terminator, which four coefficients
// could not hold — see the plan). Cell i's probe sits at (i + 0.5) · cell, and
// texel i's centre at (i + 0.5) / dims, so the two line up with a plain
// divide; trilinear filtering interpolates between probes.
fn probe_radiance(world_pos: vec3<f32>, n: vec3<f32>) -> vec3<f32> {
    let uvw = world_pos / (toon.probe_cell_size * toon.probe_dims.xyz);
    let s0 = textureSampleLevel(probe_c0, probe_sampler, uvw, 0.0);
    let s1 = textureSampleLevel(probe_c1, probe_sampler, uvw, 0.0);
    let s2 = textureSampleLevel(probe_c2, probe_sampler, uvw, 0.0);
    let s3 = textureSampleLevel(probe_c3, probe_sampler, uvw, 0.0);
    let l1 = max(s0.rgb + s1.rgb * n.x + s2.rgb * n.y + s3.rgb * n.z, vec3<f32>(0.0));
    let sun = toon.sun_rgb.rgb * (s0.a * max(dot(n, toon.sun_dir.xyz), 0.0));
    return l1 + sun;
}

const TOON_BANDS: f32 = 5.0;
// Darkest a surface can get, as a fraction of its base colour, and the
// bottom of the band range: the bands are spread over [TOON_FLOOR, 1]. Fully
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
//
// The banded value is spread over [TOON_FLOOR, 1], not [0, 1]: with five
// bands the levels are 0.18, 0.34, 0.51, 0.67, 0.84, 1.0. When the bands
// spanned [0, 1] and the floor was glued on underneath, the two darkest
// outputs were 0.18 and 0.20 — indistinguishable — so a light that lifted a
// shadow into the first band changed its *hue* (which `tinted` tracks
// continuously) and not its brightness, and the scene effectively had four
// bands with a dead one at the bottom.
//
// `level` is the output luminance whatever the hue mix below does: both mix
// endpoints have exactly that luminance, so the curve is monotone in `band`.
// The old form mixed a constant 0.18 with a value climbing 0 → 0.2 and dipped
// 26% below the floor in the middle — a one-pixel dark line along every
// contour where the lightmap crossed the first band boundary, i.e. the edge
// of every shadow in the game.
//
// The bottom band is neutral rather than hue-preserving, so a trace of
// tinted light reaching a dark corner doesn't paint the whole corner that
// colour. The fade is over a range `band` only ever crosses inside the
// bottom band's antialiasing ramp, so above the first boundary the hue is
// kept exactly.
fn toon_shade(radiance: vec3<f32>) -> vec3<f32> {
    let lum = luminance(radiance);
    let band = toon_band(lum, TOON_BANDS);
    let level = mix(TOON_FLOOR, 1.0, band);
    let tinted = radiance * (level / max(lum, 1e-4));
    return mix(vec3<f32>(level), tinted, smoothstep(0.0, TOON_FLOOR, band));
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
    //
    // `material_flags` is a `let` straight from the uniform, and the texture
    // branch below tests *it*, never `pbr_input.material.flags`. WGSL's
    // uniformity analysis treats a partial assignment to a `var` as keeping
    // the variable's old dependencies, and `pbr_input` was built from `in`
    // and `is_front`, so a condition read back out of it is non-uniform and
    // `textureSampleBias` inside it is a compile error in WebGPU (Tint).
    // naga skips that rule for fragment stages, so native never notices;
    // this is the same reason bevy's own pbr_fragment branches on a local
    // `flags` copy of the uniform.
    let material_flags = pbr_bindings::material.flags;
    pbr_input.material.base_color = pbr_bindings::material.base_color;
    pbr_input.material.flags = material_flags;

    // Mix in base_color_texture when one is bound and the mesh ships a
    // primary UV channel. Mirrors the StandardMaterial path in
    // pbr_fragment::pbr_input_from_standard_material; guarded on
    // VERTEX_UVS_A so lightmap-only meshes still compile.
@if(VERTEX_UVS_A)
    if ((material_flags & pbr_types::STANDARD_MATERIAL_FLAGS_BASE_COLOR_TEXTURE_BIT) != 0u) {
        let uv = (pbr_bindings::material.uv_transform * vec3<f32>(in.uv, 1.0)).xy;
        pbr_input.material.base_color = pbr_input.material.base_color * textureSampleBias(
            pbr_bindings::base_color_texture,
            pbr_bindings::base_color_sampler,
            uv,
            view_bindings::view.mip_bias,
        );
    }


    // White = fully lit. Used for the unlit debug view and for lightmapped
    // meshes that have no lightmap UVs at all. Computed unconditionally: in
    // the deferred prepass it is simply unused, which is the same trade
    // bevy_pbr makes in `pbr.wesl` (WESL `@if` attaches to one statement, so
    // the forward-only branch is kept to the statements that need the
    // guard). Both branches test the uniform, so the samples sit in uniform
    // control flow.
    var radiance = vec3<f32>(1.0);
    if (toon.unlit == 0u && toon.source == 2u) {
        radiance = probe_radiance(in.world_position.xyz, normalize(in.world_normal));
    }
    if (toon.unlit == 0u && toon.source == 1u) {
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
