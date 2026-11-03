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
    // The toon ramp: (bands, floor, white, gamma). See `toon_shade`.
    curve: vec4<f32>,
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

// Directionality below which a probe channel is read linearly and above
// which as an isotropic part plus one q^3 lobe, smoothstep between. Mirrors
// `PROBE_LOBE_BLEND` / `reconstruct_channel` in baking/probes.rs — the two
// must agree; the Rust side is what the tests measure.
const PROBE_LOBE_A0: f32 = 0.5;
const PROBE_LOBE_A1: f32 = 1.0;

// One colour channel of the smooth (L1) irradiance. A linear read of the
// coefficients is exact for broad light, but a lamp's lobe (1/4 + 3/4 cos)
// goes negative over a third of the sphere, and clamping that drew a hard
// planar black edge across every object near a lamp. Above a = |c|/c0 = 1
// the channel is instead split into an isotropic part and one lobe drawn as
// q^3 (same mean and peak as the clamped cosine, never negative, zero only
// directly opposite); below 1/2 it is the linear read; between, a blend.
fn probe_channel(c0_in: f32, dir: vec3<f32>, n: vec3<f32>) -> f32 {
    let c0 = max(c0_in, 0.0);
    let len = length(dir);
    if (len <= 1e-6 || c0 <= 0.0) {
        return c0;
    }
    let a = len / c0;
    let linear = max(c0 + dot(dir, n), 0.0);
    let iso = max(c0 - len / 3.0, 0.0);
    let lobe_peak = (4.0 / 3.0) * min(len, 3.0 * c0);
    let q = 0.5 * (1.0 + dot(n, dir) / len);
    let lobe = iso + lobe_peak * q * q * q;
    let w = smoothstep(PROBE_LOBE_A0, PROBE_LOBE_A1, a);
    return mix(linear, lobe, w);
}

// The four coefficient textures sampled together at one point.
struct ProbeTaps {
    c0: vec4<f32>,
    c1: vec4<f32>,
    c2: vec4<f32>,
    c3: vec4<f32>,
};

// One axis of a cubic B-spline reconstruction, folded into two *hardware*
// linear taps.
//
// Why not plain trilinear: it is C0 but not C1 — its gradient jumps at every
// probe plane. A smooth gradient hides that, but the toon quantiser draws one
// hard contour per band boundary, and each contour kinks where it crosses a
// plane. With a lamp's pool only two or three cells across, those kinks are
// four or eight visible corners: the reported "diamond with a sharp point in
// each direction" on the tops of furniture near the cafe's lamp. Measured on
// the cafe's own baked probes (`dump_probe_plane`), the rms curvature of a
// band contour falls 15-55x and its axis/diagonal anisotropy goes from 0.896
// to 0.989.
//
// A cubic B-spline is C2, so there is no slope break left to kink on. Four
// taps per axis would be 64 fetches; pairing adjacent samples and placing one
// off-centre hardware tap per pair (Sigg & Hadwiger) gives the whole 4x4x4
// kernel in 8 trilinear fetches. `bspline_axis` in `baking/probes.rs` is the
// same function, and `bspline_two_tap_form_is_exact` proves the pairing
// reproduces the direct 4-tap weights.
//
// The trade is that a B-spline approximates rather than interpolates: it is a
// low-pass, so the peak of a pool this narrow drops about a quarter (measured
// 0.99 -> 0.73 at the cafe lamp, on the 2 m lattice of the time). That is the
// honest cost of smoothing a coarse lattice, and it is why lamp intensities
// may want raising. Catmull-Rom was
// measured as the interpolating alternative and rejected — it keeps the peak
// but, being only C1 with negative lobes, left the contours as kinked as
// trilinear.
//
// `i` is the lower sample index, `t` the fraction past it; returns the two
// tap coordinates in texels and their weights, which sum to 1.
struct BsplineAxis {
    tap0: f32,
    tap1: f32,
    w0: f32,
    w1: f32,
};

fn bspline_axis(u: f32) -> BsplineAxis {
    let x = u - 0.5;
    let i = floor(x);
    let t = x - i;
    let t2 = t * t;
    let t3 = t2 * t;
    let w0 = (1.0 - 3.0 * t + 3.0 * t2 - t3) / 6.0;
    let w1 = (4.0 - 6.0 * t2 + 3.0 * t3) / 6.0;
    let w2 = (1.0 + 3.0 * t + 3.0 * t2 - 3.0 * t3) / 6.0;
    let w3 = t3 / 6.0;
    // Both pair sums stay in [1/6, 5/6], so neither divide can blow up.
    let g0 = w0 + w1;
    let g1 = w2 + w3;
    return BsplineAxis(
        i - 1.0 + w1 / g0 + 0.5,
        i + 1.0 + w3 / g1 + 0.5,
        g0,
        g1,
    );
}

fn sample_probe_volume(world_pos: vec3<f32>) -> ProbeTaps {
    // Texel coordinates: cell i's probe sits at (i + 0.5) · cell and texel
    // i's centre at i + 0.5, so world / cell is already texel space.
    let u = world_pos / toon.probe_cell_size;
    let bx = bspline_axis(u.x);
    let by = bspline_axis(u.y);
    let bz = bspline_axis(u.z);
    var xs = array<f32, 2>(bx.tap0, bx.tap1);
    var ys = array<f32, 2>(by.tap0, by.tap1);
    var zs = array<f32, 2>(bz.tap0, bz.tap1);
    var xw = array<f32, 2>(bx.w0, bx.w1);
    var yw = array<f32, 2>(by.w0, by.w1);
    var zw = array<f32, 2>(bz.w0, bz.w1);

    var taps: ProbeTaps;
    taps.c0 = vec4<f32>(0.0);
    taps.c1 = vec4<f32>(0.0);
    taps.c2 = vec4<f32>(0.0);
    taps.c3 = vec4<f32>(0.0);
    for (var k = 0u; k < 8u; k = k + 1u) {
        let ix = k & 1u;
        let iy = (k >> 1u) & 1u;
        let iz = (k >> 2u) & 1u;
        let w = xw[ix] * yw[iy] * zw[iz];
        // The sampler repeats, so the lattice is toroidal and no wrap is
        // needed here — the same reason the trilinear read needed no origin.
        let p = vec3<f32>(xs[ix], ys[iy], zs[iz]) / toon.probe_dims.xyz;
        taps.c0 = taps.c0 + textureSampleLevel(probe_c0, probe_sampler, p, 0.0) * w;
        taps.c1 = taps.c1 + textureSampleLevel(probe_c1, probe_sampler, p, 0.0) * w;
        taps.c2 = taps.c2 + textureSampleLevel(probe_c2, probe_sampler, p, 0.0) * w;
        taps.c3 = taps.c3 + textureSampleLevel(probe_c3, probe_sampler, p, 0.0) * w;
    }
    return taps;
}

// Radiance a surface with normal `n` at `world_pos` receives, from the probe
// volume: the L1 part read per channel through `probe_channel`, plus the sun
// evaluated exactly against the probe's stored visibility (a hard
// terminator, which four coefficients could not hold — see the plan).
//
// Note that the sun's visibility is low-passed by the same kernel, so a
// probe-lit object's sun terminator is softer still. It was already vague by
// construction — one binary sample per 2 m cell — so this costs little; see
// docs/LIGHTING.md.
fn probe_radiance(world_pos: vec3<f32>, n: vec3<f32>) -> vec3<f32> {
    let s = sample_probe_volume(world_pos);
    let l1 = vec3<f32>(
        probe_channel(s.c0.r, vec3<f32>(s.c1.r, s.c2.r, s.c3.r), n),
        probe_channel(s.c0.g, vec3<f32>(s.c1.g, s.c2.g, s.c3.g), n),
        probe_channel(s.c0.b, vec3<f32>(s.c1.b, s.c2.b, s.c3.b), n),
    );
    let sun = toon.sun_rgb.rgb * (s.c0.a * max(dot(n, toon.sun_dir.xyz), 0.0));
    return l1 + sun;
}

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
        pbr_input.material.base_color.rgb * toon_shade(radiance, toon.curve),
        pbr_input.material.base_color.a,
    );

    return out;
}
