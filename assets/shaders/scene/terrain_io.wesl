// What the terrain vertex stage hands to its two fragment stages.
//
// This replaces `bevy_pbr::render::forward_io::VertexOutput`, which carried
// seven varyings behind shader defs for features terrain has never used
// (tangents, vertex colours, UV0, the visibility-range dither). Terrain binds
// three attributes and interpolates three varyings, and both fragment shaders
// are ours, so the struct is written out rather than configured.
//
// The colour and prepass fragments take the *same* struct: a fragment entry
// point may read a subset of the vertex stage's locations, so one vertex
// shader serves both passes.

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) world_position: vec4<f32>,
    @location(1) world_normal: vec3<f32>,
    // UV1 — the chunk's baked lightmap coordinates. (UV0 exists in the mesh
    // but is all zeros; nothing reads it, so the pipeline doesn't bind it.)
    @location(2) uv_b: vec2<f32>,
}

// The colour pass writes the view's HDR target.
struct FragmentOutput {
    @location(0) color: vec4<f32>,
}

// The prepass writes `ViewPrepassTextures.normal`; depth needs no fragment
// work. Attachment 0 is the normal target — the layout
// `bevy_core_pipeline::prepass::prepass_target_descriptors` fixes, which the
// pass systems in scene_pass/pass.rs reproduce.
struct PrepassOutput {
    @location(0) normal: vec4<f32>,
}
