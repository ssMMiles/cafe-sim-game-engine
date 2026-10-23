// Toon fragment shader for placed meshes.
//
// All lighting is baked: the lightmap holds outgoing radiance in "lightmap
// units" (1.0 = full base colour) with the sun, ambient×AO, and — soon —
// point/spot lights already folded in. The runtime does no light evaluation
// at all: sample, band, multiply. That keeps the WASM/WebGPU path free of
// shadow maps and clustered lighting entirely.

#import bevy_pbr::{
    pbr_fragment::pbr_input_from_vertex_output,
    pbr_bindings,
    pbr_types,
    mesh_view_bindings as view_bindings,
}

#ifdef PREPASS_PIPELINE
#import bevy_pbr::{
    prepass_io::{VertexOutput, FragmentOutput},
    pbr_deferred_functions::deferred_output,
}
#else
#import bevy_pbr::forward_io::{VertexOutput, FragmentOutput}
#endif

@group(#{MATERIAL_BIND_GROUP}) @binding(100)
var lightmap_texture: texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(101)
var lightmap_sampler: sampler;

struct ToonFlags {
    use_lightmap: u32,
};
@group(#{MATERIAL_BIND_GROUP}) @binding(102)
var<uniform> toon_flags: ToonFlags;

const TOON_BANDS: f32 = 5.0;
// Darkest a surface can get, as a fraction of its base colour. Fully
// shadowed geometry reads as a dark tint rather than dropping to pure black.
const TOON_FLOOR: f32 = 0.18;

// Perceptual luminance from a linear RGB color
fn luminance(color: vec3<f32>) -> f32 {
    return dot(color, vec3<f32>(0.2126, 0.7152, 0.0722));
}

// Quantize a scalar value into discrete toon bands
fn toon_band(value: f32, bands: f32) -> f32 {
    return floor(clamp(value, 0.0, 1.0) * bands) / bands;
}

// Quantize brightness into toon bands while preserving the hue of coloured
// light. Banding each channel separately would shift hue at band edges.
// Anything that lands in the bottom band is lifted to a neutral TOON_FLOOR:
// neutral rather than hue-preserving, so a trace of tinted light reaching a
// dark corner doesn't paint the whole corner that colour.
fn toon_shade(radiance: vec3<f32>) -> vec3<f32> {
    let lum = luminance(radiance);
    let band = toon_band(lum, TOON_BANDS);
    if (lum <= 0.0 || band <= TOON_FLOOR) {
        return vec3<f32>(TOON_FLOOR);
    }
    return radiance * (band / lum);
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
#ifdef VERTEX_UVS_A
    if ((pbr_input.material.flags & pbr_types::STANDARD_MATERIAL_FLAGS_BASE_COLOR_TEXTURE_BIT) != 0u) {
        let uv = (pbr_bindings::material.uv_transform * vec3<f32>(in.uv, 1.0)).xy;
        pbr_input.material.base_color = pbr_input.material.base_color * textureSampleBias(
            pbr_bindings::base_color_texture,
            pbr_bindings::base_color_sampler,
            uv,
            view_bindings::view.mip_bias,
        );
    }
#endif

#ifdef PREPASS_PIPELINE
    let out = deferred_output(in, pbr_input);
#else
    var out: FragmentOutput;

    // White = fully lit. Used for the unlit debug view and for meshes that
    // have no lightmap UVs at all.
    var radiance = vec3<f32>(1.0);
    if (toon_flags.use_lightmap == 1u) {
#ifdef VERTEX_UVS_B
        radiance = textureSample(lightmap_texture, lightmap_sampler, in.uv_b).rgb;
#endif
    }

    out.color = vec4<f32>(
        pbr_input.material.base_color.rgb * toon_shade(radiance),
        pbr_input.material.base_color.a,
    );
#endif

    return out;
}
