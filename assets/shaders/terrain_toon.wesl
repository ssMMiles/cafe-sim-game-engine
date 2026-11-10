// Toon fragment shader for terrain chunks.
//
// All lighting is baked (see toon_lighting.wesl for the model); this shader
// only decides the *base colour*: which painted layers are under this pixel
// and what their patterns and tints make of it (docs/TERRAIN_PAINTING_PLAN.md).
//
// Painting is a material-ID map. Every tile of the chunk's `paint_map` holds
// a base layer, an overlay layer and a blend amount (R, G, B as bytes); the
// palette uniform says what each layer looks like; `layer_textures` is one
// 2d array slice per layer. The common case — the four tiles around a pixel
// agree on their layer pair — costs two layers' worth of taps; only the
// one-tile band where neighbouring tiles disagree evaluates all four.
//
// WebGPU uniformity: every branch below depends on the paint map, i.e. is
// non-uniform, and Tint rejects implicit-derivative sampling there. So the
// screen-space gradients of the world position are taken *once*, at the top
// of `fragment`, and every pattern tap is `textureSampleGrad` — legal in any
// control flow. (`dpdx` itself must stay at the top for the same reason.)

import package::shaders::scene::terrain_io::{VertexOutput, FragmentOutput};
import package::shaders::scene::bindings::scene;

// One slice per palette layer, built by `paint::build_palette_array`.
@group(constants::MATERIAL_BIND_GROUP) @binding(100)
var layer_textures: texture_2d_array<f32>;
@group(constants::MATERIAL_BIND_GROUP) @binding(101)
var layer_sampler: sampler;

@group(constants::MATERIAL_BIND_GROUP) @binding(102)
var lightmap_texture: texture_2d<f32>;
@group(constants::MATERIAL_BIND_GROUP) @binding(103)
var lightmap_sampler: sampler;

// Mirrors `TerrainShading` in terrain/material.rs field for field. The toon
// curve is per scene, not per material: `scene.curve` (bind group 0).
struct TerrainShading {
    // 1 = shade from the baked lightmap, 0 = flat unlit debug view.
    use_lightmap: u32,
    _pad: vec3<f32>,
};
@group(constants::MATERIAL_BIND_GROUP) @binding(104)
var<uniform> terrain: TerrainShading;

// The chunk's paint map: texel (lx, ly) = tile (lx, ly), `Rgba8Unorm`, read
// with `textureLoad` so ids come back exact. See `paint::paint_image`.
@group(constants::MATERIAL_BIND_GROUP) @binding(105)
var paint_map: texture_2d<f32>;

// Mirrors `TerrainLayerGpu` / `TerrainPaletteGpu` in terrain/material.rs.
struct TerrainLayer {
    // Linear RGB tint; w = pattern strength.
    tint: vec4<f32>,
    // x = repeats per metre, y = 1 for the no-tile sampler, z = blend
    // profile (only Soft so far), w unused.
    params: vec4<f32>,
};
struct TerrainPaletteUniform {
    layer_count: u32,
    _pad: vec3<f32>,
    layers: array<TerrainLayer, 32>,
};
@group(constants::MATERIAL_BIND_GROUP) @binding(106)
var<uniform> palette: TerrainPaletteUniform;

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

// Where a pattern is dark the layer's tint is pulled towards this. A
// constant rather than per layer until something needs it to differ.
const PATTERN_DARK: vec3<f32> = vec3<f32>(0.02, 0.02, 0.02);

// Cheap 2D hash — maps an integer-lattice vec2 to a random [0,1]^2 offset
fn hash2(p: vec2<f32>) -> vec2<f32> {
    let q = vec2<f32>(
        dot(p, vec2<f32>(127.1, 311.7)),
        dot(p, vec2<f32>(269.5, 183.3))
    );
    return fract(sin(q) * 43758.5453);
}

// One pattern tap with explicit gradients (see the header).
fn pattern_tap(layer: u32, uv: vec2<f32>, gx: vec2<f32>, gy: vec2<f32>) -> f32 {
    return textureSampleGrad(layer_textures, layer_sampler, uv, layer, gx, gy).r;
}

// Stochastic (no-tile) pattern sample: blends four neighbouring lattice
// cells, each with its own random UV offset, so the texture's repeat grid
// becomes invisible. The offsets are integer-free noise added to the UV, so
// the gradients are unchanged.
fn pattern_no_tile(layer: u32, uv: vec2<f32>, gx: vec2<f32>, gy: vec2<f32>) -> f32 {
    let p = floor(uv);
    let f = fract(uv);
    let w = f * f * (3.0 - 2.0 * f);

    let va = pattern_tap(layer, uv + hash2(p), gx, gy);
    let vb = pattern_tap(layer, uv + hash2(p + vec2<f32>(1.0, 0.0)), gx, gy);
    let vc = pattern_tap(layer, uv + hash2(p + vec2<f32>(0.0, 1.0)), gx, gy);
    let vd = pattern_tap(layer, uv + hash2(p + vec2<f32>(1.0, 1.0)), gx, gy);

    return mix(mix(va, vb, w.x), mix(vc, vd, w.x), w.y);
}

// The colour one layer gives a point: its tint, darkened where its pattern
// is dark. `gx`/`gy` are the world-xz gradients per screen pixel.
fn layer_color(layer: u32, xz: vec2<f32>, gx: vec2<f32>, gy: vec2<f32>) -> vec3<f32> {
    let l = palette.layers[layer];
    let scale = l.params.x;
    let uv = xz * scale;
    var pattern = 1.0;
    if (l.params.y > 0.5) {
        pattern = pattern_no_tile(layer, uv, gx * scale, gy * scale);
    } else {
        pattern = pattern_tap(layer, uv, gx * scale, gy * scale);
    }
    let factor = smoothstep(0.05, 0.45, 1.0 - pattern) * l.tint.w;
    return mix(l.tint.rgb, PATTERN_DARK, factor);
}

// A paint sample's id byte as a layer index, clamped into the palette.
fn layer_id(channel: f32) -> u32 {
    return min(u32(round(channel * 255.0)), palette.layer_count - 1u);
}

// The colour of one paint sample: base, with the overlay faded in by the
// blend byte. Blend profiles other than Soft are not implemented yet.
fn sample_color(sample: vec4<f32>, xz: vec2<f32>, gx: vec2<f32>, gy: vec2<f32>) -> vec3<f32> {
    let base = layer_id(sample.r);
    let overlay = layer_id(sample.g);
    var color = layer_color(base, xz, gx, gy);
    if (sample.b > 0.0 && overlay != base) {
        color = mix(color, layer_color(overlay, xz, gx, gy), sample.b);
    }
    return color;
}

// Base colour at this pixel from the four surrounding paint tiles.
fn painted_color(in: VertexOutput, gx: vec2<f32>, gy: vec2<f32>) -> vec3<f32> {
    let xz = in.world_position.xz;
    let dims = vec2<i32>(textureDimensions(paint_map));
    // UV1 runs 0..1 over the chunk's (dims - 1) tiles, so this is the
    // tile-space position; the four corners are the tiles around it.
    let local = in.uv_b * vec2<f32>(dims - vec2<i32>(1));
    let p0 = clamp(vec2<i32>(floor(local)), vec2<i32>(0), dims - vec2<i32>(2));
    let f = clamp(local - vec2<f32>(p0), vec2<f32>(0.0), vec2<f32>(1.0));

    let c00 = textureLoad(paint_map, p0, 0);
    let c10 = textureLoad(paint_map, p0 + vec2<i32>(1, 0), 0);
    let c01 = textureLoad(paint_map, p0 + vec2<i32>(0, 1), 0);
    let c11 = textureLoad(paint_map, p0 + vec2<i32>(1, 1), 0);

    let same_pair = all(c00.rg == c10.rg) && all(c00.rg == c01.rg) && all(c00.rg == c11.rg);
    if (same_pair) {
        // Nearly every pixel: one layer pair, blend interpolated across the tile.
        let blend = mix(mix(c00.b, c10.b, f.x), mix(c01.b, c11.b, f.x), f.y);
        return sample_color(vec4<f32>(c00.rg, blend, 0.0), xz, gx, gy);
    }
    // A layer boundary runs through this tile: shade each corner's own pair
    // and mix the colours bilinearly.
    let w00 = (1.0 - f.x) * (1.0 - f.y);
    let w10 = f.x * (1.0 - f.y);
    let w01 = (1.0 - f.x) * f.y;
    let w11 = f.x * f.y;
    return sample_color(c00, xz, gx, gy) * w00
        + sample_color(c10, xz, gx, gy) * w10
        + sample_color(c01, xz, gx, gy) * w01
        + sample_color(c11, xz, gx, gy) * w11;
}

@fragment
fn fragment(in: VertexOutput) -> FragmentOutput {
    // Taken first, in uniform control flow (see the header).
    let gx = dpdx(in.world_position.xz);
    let gy = dpdy(in.world_position.xz);

    let base_color = painted_color(in, gx, gy);

    // White = fully lit (unlit debug view / unbaked chunk). The branch tests
    // the uniform directly, so the sample stays in uniform control flow.
    var radiance = vec3<f32>(1.0);
    if (terrain.use_lightmap == 1u) {
        radiance = textureSample(lightmap_texture, lightmap_sampler, in.uv_b).rgb;
    }

    var out: FragmentOutput;
    out.color = vec4<f32>(base_color * toon_shade(radiance, scene.curve), 1.0);
    // The edge detector's normal target. Terrain is never double-sided, so
    // there is no front/back flip; the skirt normal was fixed up in the
    // vertex stage, where the mesh generator's zero normals come from.
@if(SCENE_NORMAL_TARGET)
    out.normal = vec4<f32>(in.world_normal * 0.5 + vec3<f32>(0.5), 1.0);
    return out;
}
