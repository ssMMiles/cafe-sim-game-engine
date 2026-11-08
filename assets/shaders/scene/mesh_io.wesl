// What `mesh_vertex.wesl` hands every fragment stage of the scene pass:
// `toon_lighting.wesl`, `sky_dome.wesl` and `unlit.wesl`. Replaces
// bevy_pbr's `forward_io::VertexOutput` / `prepass::io::VertexOutput`; one
// struct and one vertex shader serve every material that isn't terrain
// (which has its own, narrower pair — three varyings and no defs at all).
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

// The scene pass is a single geometry pass with two colour targets: the
// view's HDR target and `ViewPrepassTextures.normal`, the world normal the
// edge detector reads (docs/EDGE_DETECTION.md), packed `n * 0.5 + 0.5` into
// `Rgb10a2Unorm`. Every colour shader writes both; for the layers that must
// not leave a normal behind (sky, translucent overlays) the pipeline masks
// the second target off (`scene_pass/pipeline.rs`), so the value written
// there is irrelevant. A separate normal prepass used to produce this
// texture; it was folded in on 2026-11-08 to stop drawing the scene twice.
// `SCENE_NORMAL_TARGET` is set by the pipeline for views that have a normal
// prepass texture (the 3D camera); the thumbnail rigs have none, and their
// variant has a single colour target and no second output.
struct FragmentOutput {
    @location(0) color: vec4<f32>,
    @if(SCENE_NORMAL_TARGET)
    @location(1) normal: vec4<f32>,
}
