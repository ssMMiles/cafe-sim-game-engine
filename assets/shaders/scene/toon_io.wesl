// What the toon vertex stage hands its two fragment stages
// (`toon_lighting.wesl`, `toon_prepass.wesl`). Replaces bevy_pbr's
// `forward_io::VertexOutput` / `prepass::io::VertexOutput`, which differed
// from each other in location numbering and so forced two vertex variants;
// here one struct and one vertex shader serve both passes.
//
// `VERTEX_UVS_A` / `VERTEX_UVS_B` keep bevy's names on purpose: the fragment
// shaders already branch on them, and the scene pipeline sets them from the
// mesh layout exactly as bevy's did.

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) world_position: vec4<f32>,
    @location(1) world_normal: vec3<f32>,
    @if(VERTEX_UVS_A)
    @location(2) uv: vec2<f32>,
    @if(VERTEX_UVS_B)
    @location(3) uv_b: vec2<f32>,
    // Which `SceneInstance` this fragment belongs to; the double-sided
    // front/back test reads its winding flag.
    @location(4) @interpolate(flat) instance_index: u32,
}

struct FragmentOutput {
    @location(0) color: vec4<f32>,
}

// Attachment 0 of the prepass is the normal target; see terrain_io.wesl.
struct PrepassOutput {
    @location(0) normal: vec4<f32>,
}
