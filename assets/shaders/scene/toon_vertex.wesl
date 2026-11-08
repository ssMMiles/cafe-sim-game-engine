// Toon vertex stage for the scene pass, colour and prepass alike — static
// meshes and the one skinned mesh (the player) by the `SKINNED` def.
//
// Replaces bevy_pbr's `mesh.wesl` vertex for toon materials. What it keeps
// from there: the transform comes from an instance buffer indexed by
// `instance_index`, skinning is a weighted sum of four joint matrices that
// already fold in the inverse bind pose, and a skinned normal goes through
// the skinned matrix's inverse transpose rather than the instance's. What it
// drops: morph targets, tangents, vertex colours, motion vectors, the
// visibility-range dither — none of which a toon material reads.

import package::shaders::scene::bindings::{
    instances, scene_position_world_to_clip, scene_skin_model, scene_normal_matrix,
};
import package::shaders::scene::toon_io::VertexOutput;

struct Vertex {
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @if(VERTEX_UVS_A)
    @location(2) uv: vec2<f32>,
    @if(VERTEX_UVS_B)
    @location(3) uv_b: vec2<f32>,
    @if(SKINNED)
    @location(4) joint_indices: vec4<u32>,
    @if(SKINNED)
    @location(5) joint_weights: vec4<f32>,
    @builtin(instance_index) instance_index: u32,
}

@vertex
fn vertex(in: Vertex) -> VertexOutput {
    var out: VertexOutput;

    let instance = instances[in.instance_index];
@if(SKINNED)
    let world_from_local = scene_skin_model(in.instance_index, in.joint_indices, in.joint_weights);
@if(SKINNED)
    let normal_from_local = scene_normal_matrix(world_from_local);
@if(!SKINNED)
    let world_from_local = instance.world_from_local;
@if(!SKINNED)
    let normal_from_local = instance.normal_from_local;

    let world_position = world_from_local * vec4<f32>(in.position, 1.0);
    out.position = scene_position_world_to_clip(world_position.xyz);
    out.world_position = world_position;
    out.world_normal = normalize(normal_from_local * in.normal);
@if(VERTEX_UVS_A)
    out.uv = in.uv;
@if(VERTEX_UVS_B)
    out.uv_b = in.uv_b;
    out.instance_index = in.instance_index;

    return out;
}
