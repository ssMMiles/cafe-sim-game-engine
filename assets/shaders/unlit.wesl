// Translucent unlit tint (engine-api `UnlitMaterial`): base colour × optional
// texture, nothing else. Drawn by the scene pass in its transparent layer —
// alpha-blended over the finished opaque image, no depth write, no prepass
// variant — for the placement ghost, the brush disc and blob shadows.

import package::shaders::scene::mesh_io::{VertexOutput, FragmentOutput};

// Mirrors `UnlitUniform` in engine-api's unlit_material.rs.
struct UnlitSurface {
    // Linear RGBA; alpha is the opacity.
    base_color: vec4<f32>,
};
@group(constants::MATERIAL_BIND_GROUP) @binding(0)
var<uniform> surface: UnlitSurface;
// bevy binds its white fallback when the material has no texture, so the
// sample is unconditional where the mesh has UVs — no branch for WebGPU's
// uniformity analysis to object to.
@group(constants::MATERIAL_BIND_GROUP) @binding(1)
var base_color_texture: texture_2d<f32>;
@group(constants::MATERIAL_BIND_GROUP) @binding(2)
var base_color_sampler: sampler;

@fragment
fn fragment(in: VertexOutput) -> FragmentOutput {
    var color = surface.base_color;
@if(VERTEX_UVS_A) {
    color = color * textureSample(base_color_texture, base_color_sampler, in.uv);
}
    var out: FragmentOutput;
    out.color = color;
    // Never written: the transparent layer's pipeline masks the normal
    // target off, so a blob shadow gets no outline of its own.
@if(SCENE_NORMAL_TARGET)
    out.normal = vec4<f32>(0.0);
    return out;
}
