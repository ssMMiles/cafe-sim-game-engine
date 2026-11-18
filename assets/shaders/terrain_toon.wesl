// Toon fragment shader for terrain chunks.
//
// All lighting is baked (see toon_lighting.wesl for the model); this shader
// only decides the *base colour*: which painted layers are under this pixel
// and what their textures × tints make of it (docs/TERRAIN_PAINTING_PLAN.md).
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
    // 0 = off, 1 = flat per-layer-id colours, 2 = highlight auto tiles.
    debug: u32,
    _pad: vec2<f32>,
};
@group(constants::MATERIAL_BIND_GROUP) @binding(104)
var<uniform> terrain: TerrainShading;

// The chunk's paint map: texel (lx, ly) = tile (lx, ly), `Rgba8Unorm`, read
// with `textureLoad` so ids come back exact. See `paint::paint_image`.
@group(constants::MATERIAL_BIND_GROUP) @binding(105)
var paint_map: texture_2d<f32>;

// Mirrors `TerrainLayerGpu` / `TerrainPaletteGpu` in terrain/material.rs.
struct TerrainLayer {
    // Linear RGB tint (white = the texture as authored); w = texture strength.
    tint: vec4<f32>,
    // x = repeats per metre, y = 1 for the no-tile sampler, z = blend
    // profile (see `blend_amount`), w = that profile's parameter.
    params: vec4<f32>,
    // x = the layer id this slot displays as (a removed slot's fallback).
    misc: vec4<f32>,
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

// Cheap 2D hash — maps an integer-lattice vec2 to a random [0,1]^2 offset
fn hash2(p: vec2<f32>) -> vec2<f32> {
    let q = vec2<f32>(
        dot(p, vec2<f32>(127.1, 311.7)),
        dot(p, vec2<f32>(269.5, 183.3))
    );
    return fract(sin(q) * 43758.5453);
}

// One texture tap with explicit gradients (see the header). Full colour: the
// palette array is sRGB, so this is linear RGB.
fn pattern_tap(layer: u32, uv: vec2<f32>, gx: vec2<f32>, gy: vec2<f32>) -> vec3<f32> {
    return textureSampleGrad(layer_textures, layer_sampler, uv, layer, gx, gy).rgb;
}

// Stochastic (no-tile) texture sample: blends four neighbouring lattice
// cells, each with its own random UV offset, so the texture's repeat grid
// becomes invisible. The offsets are integer-free noise added to the UV, so
// the gradients are unchanged.
fn pattern_no_tile(layer: u32, uv: vec2<f32>, gx: vec2<f32>, gy: vec2<f32>) -> vec3<f32> {
    let p = floor(uv);
    let f = fract(uv);
    let w = f * f * (3.0 - 2.0 * f);

    let va = pattern_tap(layer, uv + hash2(p), gx, gy);
    let vb = pattern_tap(layer, uv + hash2(p + vec2<f32>(1.0, 0.0)), gx, gy);
    let vc = pattern_tap(layer, uv + hash2(p + vec2<f32>(0.0, 1.0)), gx, gy);
    let vd = pattern_tap(layer, uv + hash2(p + vec2<f32>(1.0, 1.0)), gx, gy);

    return mix(mix(va, vb, w.x), mix(vc, vd, w.x), w.y);
}

// Rec. 709 luma of a linear-RGB texel: the scalar "height" the Height blend
// profile compares now that textures are full colour (it compared the raw
// greyscale pattern value before 2026-11-17).
fn texel_luma(texel: vec3<f32>) -> f32 {
    return dot(texel, vec3<f32>(0.2126, 0.7152, 0.0722));
}

// One layer at a point: rgb = tint × the texture faded towards white by
// (1 − strength) — strength 1 is the full colour texture, 0 a flat tint, and
// a white tint leaves the texture as authored. a = the texel's luma (what
// the Height profile compares). `gx`/`gy` are the world-xz gradients per
// screen pixel.
//
// Until 2026-11-17 textures were greyscale *patterns* that could only pull
// the tint towards black (`mix(tint, PATTERN_DARK, smoothstep(0.05, 0.45,
// 1 − pattern) × strength)`); tints from palettes authored for that model
// read a little darker and flatter under the multiply and want a touch-up.
fn layer_eval(layer: u32, xz: vec2<f32>, gx: vec2<f32>, gy: vec2<f32>) -> vec4<f32> {
    let l = palette.layers[layer];
    let scale = l.params.x;
    let uv = xz * scale;
    var texel = vec3<f32>(1.0);
    if (l.params.y > 0.5) {
        texel = pattern_no_tile(layer, uv, gx * scale, gy * scale);
    } else {
        texel = pattern_tap(layer, uv, gx * scale, gy * scale);
    }
    let rgb = l.tint.rgb * mix(vec3<f32>(1.0), texel, l.tint.w);
    return vec4<f32>(rgb, texel_luma(texel));
}

// A paint sample's id byte as the layer it displays: clamped into the
// palette, then through the slot's redirect (removed layers show their
// fallback).
fn layer_id(channel: f32) -> u32 {
    let slot = min(u32(round(channel * 255.0)), palette.layer_count - 1u);
    return min(u32(palette.layers[slot].misc.x), palette.layer_count - 1u);
}

// Smooth value noise on a lattice, for the Scatter profile.
fn value_noise(p: vec2<f32>) -> f32 {
    let i = floor(p);
    let f = fract(p);
    let w = f * f * (3.0 - 2.0 * f);
    let a = hash2(i).x;
    let b = hash2(i + vec2<f32>(1.0, 0.0)).x;
    let c = hash2(i + vec2<f32>(0.0, 1.0)).x;
    let d = hash2(i + vec2<f32>(1.0, 1.0)).x;
    return mix(mix(a, b, w.x), mix(c, d, w.x), w.y);
}

// How much of the overlay shows, from the painted blend `b` (0..1) under
// the overlay layer's profile. `bw` is `fwidth` of the interpolated blend,
// taken in uniform flow by the caller, so every edge here can be resolved
// over ~one screen pixel without a derivative of its own. `h_base` /
// `h_over` are the two textures' luma at this point.
//   0 Soft     linear.
//   1 Hard     step at ½, width from one pixel (param 1) to ½ (param 0).
//   2 Height   the overlay appears first where its texture is lighter (by
//              luma) than the base's; param is how strongly that decides.
//   3 Scatter  dithered patches: overlay where b exceeds a noise field;
//              param is the patch size (25 cm – 4 m).
//   4 Stepped  quantised into 2–8 bands, each boundary ~one pixel.
fn blend_amount(
    profile: f32, param: f32, b: f32, bw: f32,
    h_base: f32, h_over: f32, xz: vec2<f32>,
) -> f32 {
    let p = u32(round(profile));
    if (p == 1u) {
        let w = max(bw, 0.5 * (1.0 - param));
        return smoothstep(0.5 - w, 0.5 + w, b);
    }
    if (p == 2u) {
        let v = b + (h_over - h_base) * param;
        let w = max(bw, 0.5 - 0.45 * param);
        return smoothstep(0.5 - w, 0.5 + w, v);
    }
    if (p == 3u) {
        let patch_size = mix(0.25, 4.0, param);
        let n = value_noise(xz / patch_size + vec2<f32>(37.0, 11.0));
        let w = max(bw, 0.02);
        return smoothstep(n - w, n + w, b);
    }
    if (p == 4u) {
        let steps = 2.0 + round(param * 6.0);
        let x = b * steps;
        let f = floor(x);
        let w = clamp(bw * steps, 1e-5, 1.0);
        return clamp((f + smoothstep(1.0 - w, 1.0, x - f)) / steps, 0.0, 1.0);
    }
    return b;
}

// The colour of one paint sample: base, with the overlay brought in by its
// blend profile. `b` is the (possibly interpolated) blend to use.
fn sample_color(
    sample: vec4<f32>, b: f32, bw: f32,
    xz: vec2<f32>, gx: vec2<f32>, gy: vec2<f32>,
) -> vec3<f32> {
    let base = layer_id(sample.r);
    let overlay = layer_id(sample.g);
    let base_eval = layer_eval(base, xz, gx, gy);
    if (b <= 0.0 || overlay == base) {
        return base_eval.rgb;
    }
    let over_eval = layer_eval(overlay, xz, gx, gy);
    let l = palette.layers[overlay];
    let t = blend_amount(l.params.z, l.params.w, b, bw, base_eval.a, over_eval.a, xz);
    return mix(base_eval.rgb, over_eval.rgb, t);
}

// The four paint tiles around this pixel and where it sits between them.
struct PaintCorners {
    c00: vec4<f32>,
    c10: vec4<f32>,
    c01: vec4<f32>,
    c11: vec4<f32>,
    f: vec2<f32>,
}

fn paint_corners(uv_b: vec2<f32>) -> PaintCorners {
    let dims = vec2<i32>(textureDimensions(paint_map));
    // UV1 runs 0..1 over the chunk's (dims - 1) tiles, so this is the
    // tile-space position; the four corners are the tiles around it.
    let local = uv_b * vec2<f32>(dims - vec2<i32>(1));
    let p0 = clamp(vec2<i32>(floor(local)), vec2<i32>(0), dims - vec2<i32>(2));
    var out: PaintCorners;
    out.f = clamp(local - vec2<f32>(p0), vec2<f32>(0.0), vec2<f32>(1.0));
    out.c00 = textureLoad(paint_map, p0, 0);
    out.c10 = textureLoad(paint_map, p0 + vec2<i32>(1, 0), 0);
    out.c01 = textureLoad(paint_map, p0 + vec2<i32>(0, 1), 0);
    out.c11 = textureLoad(paint_map, p0 + vec2<i32>(1, 1), 0);
    return out;
}

fn bilinear_blend(c: PaintCorners) -> f32 {
    return mix(mix(c.c00.b, c.c10.b, c.f.x), mix(c.c01.b, c.c11.b, c.f.x), c.f.y);
}

// Base colour at this pixel from the four surrounding paint tiles.
fn painted_color(
    c: PaintCorners, blend: f32, bw: f32,
    xz: vec2<f32>, gx: vec2<f32>, gy: vec2<f32>,
) -> vec3<f32> {
    let same_pair = all(c.c00.rg == c.c10.rg) && all(c.c00.rg == c.c01.rg) && all(c.c00.rg == c.c11.rg);
    if (same_pair) {
        // Nearly every pixel: one layer pair, blend interpolated across the tile.
        return sample_color(c.c00, blend, bw, xz, gx, gy);
    }
    // A layer boundary runs through this tile: shade each corner's own pair
    // and mix the colours bilinearly.
    let f = c.f;
    let w00 = (1.0 - f.x) * (1.0 - f.y);
    let w10 = f.x * (1.0 - f.y);
    let w01 = (1.0 - f.x) * f.y;
    let w11 = f.x * f.y;
    return sample_color(c.c00, c.c00.b, bw, xz, gx, gy) * w00
        + sample_color(c.c10, c.c10.b, bw, xz, gx, gy) * w10
        + sample_color(c.c01, c.c01.b, bw, xz, gx, gy) * w01
        + sample_color(c.c11, c.c11.b, bw, xz, gx, gy) * w11;
}

// A distinct flat colour per layer id (golden-ratio hue walk), for the
// layer-id debug view: tints can be near-identical between layers, ids never.
fn debug_layer_color(id: u32) -> vec3<f32> {
    let h = f32(id) * 0.618034;
    return vec3<f32>(0.55) + 0.45 * cos(6.2831853 * (vec3<f32>(h) + vec3<f32>(0.0, 0.33, 0.67)));
}

// The debug overlays, both driven by the *nearest* paint tile so what is
// shown is exactly one stored sample, crisp at tile edges. `shaded` is what
// the fragment would otherwise output.
fn debug_view(c: PaintCorners, shaded: vec3<f32>) -> vec3<f32> {
    let fx = c.f.x > 0.5;
    let fy = c.f.y > 0.5;
    var tile = c.c00;
    if (fx && fy) { tile = c.c11; } else if (fx) { tile = c.c10; } else if (fy) { tile = c.c01; }
    if (terrain.debug == 1u) {
        // Dominant layer of the tile, through the palette's removed-slot
        // redirect, exactly as the real shading resolves it.
        var id = u32(round(tile.r * 255.0));
        if (tile.b >= 0.5) {
            id = u32(round(tile.g * 255.0));
        }
        let shown = u32(palette.layers[min(id, palette.layer_count - 1u)].misc.x);
        return debug_layer_color(shown);
    }
    // Auto tiles (owned by the paint rules): magenta over the real shading.
    let auto_owned = (u32(round(tile.a * 255.0)) & 1u) != 0u;
    if (auto_owned) {
        return mix(shaded, vec3<f32>(1.0, 0.0, 1.0), 0.6);
    }
    return shaded;
}

@fragment
fn fragment(in: VertexOutput) -> FragmentOutput {
    // Every derivative is taken here, in uniform control flow (see the
    // header): the world-xz gradients for the pattern taps, and the blend's
    // screen-space rate for the profiles' one-pixel edges. The corner loads
    // have no derivative of their own, so they can come first.
    let corners = paint_corners(in.uv_b);
    let blend = bilinear_blend(corners);
    let gx = dpdx(in.world_position.xz);
    let gy = dpdy(in.world_position.xz);
    let bw = fwidth(blend);

    let base_color = painted_color(corners, blend, bw, in.world_position.xz, gx, gy);

    // White = fully lit (unlit debug view / unbaked chunk). The branch tests
    // the uniform directly, so the sample stays in uniform control flow.
    var radiance = vec3<f32>(1.0);
    if (terrain.use_lightmap == 1u) {
        radiance = textureSample(lightmap_texture, lightmap_sampler, in.uv_b).rgb;
    }

    var shaded = base_color * toon_shade(radiance, scene.curve);
    // Editor debug views (View menu). The branch tests a uniform, and every
    // derivative this shader takes was already taken above it.
    if (terrain.debug != 0u) {
        shaded = debug_view(corners, shaded);
    }

    var out: FragmentOutput;
    out.color = vec4<f32>(shaded, 1.0);
    // The edge detector's normal target. Terrain is never double-sided, so
    // there is no front/back flip; the skirt normal was fixed up in the
    // vertex stage, where the mesh generator's zero normals come from.
@if(SCENE_NORMAL_TARGET)
    out.normal = vec4<f32>(in.world_normal * 0.5 + vec3<f32>(0.5), 1.0);
    return out;
}
