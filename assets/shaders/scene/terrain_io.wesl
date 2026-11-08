// What the terrain vertex stage hands to its two fragment stages.
//
// This replaces `bevy_pbr::render::forward_io::VertexOutput`, which carried
// seven varyings behind shader defs for features terrain has never used
// (tangents, vertex colours, UV0, the visibility-range dither). Terrain binds
// three attributes and interpolates three varyings, and both fragment shaders
// are ours, so the struct is written out rather than configured.
//

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) world_position: vec4<f32>,
    @location(1) world_normal: vec3<f32>,
    // UV1 — the chunk's baked lightmap coordinates. (UV0 exists in the mesh
    // but is all zeros; nothing reads it, so the pipeline doesn't bind it.)
    @location(2) uv_b: vec2<f32>,
}

// The scene pass writes the view's HDR target and, as a second colour
// target, `ViewPrepassTextures.normal` — the world normal the edge detector
// reads, packed `n * 0.5 + 0.5` into `Rgb10a2Unorm` (the reader normalises).
// See mesh_io.wesl.
// `SCENE_NORMAL_TARGET` is set by the pipeline for views that have a normal
// prepass texture (the 3D camera); the thumbnail rigs have none, and their
// variant has a single colour target and no second output.
struct FragmentOutput {
    @location(0) color: vec4<f32>,
    @if(SCENE_NORMAL_TARGET)
    @location(1) normal: vec4<f32>,
}
