#import bevy_pbr::{
    mesh_functions,
    view_transformations::position_world_to_clip,
}

// Conditionally import the correct VertexOutput based on which pass is running
#ifdef PREPASS_PIPELINE
#import bevy_pbr::prepass_io::VertexOutput
#else
#import bevy_pbr::forward_io::VertexOutput
#endif

@vertex
fn vertex(
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @builtin(instance_index) instance_index: u32,
) -> VertexOutput {
    var out: VertexOutput;

    // Use pre-computed position and normal from vertex attributes
    let local_pos = position;
    let normal_vec = normal;

    // Transform to world space
    let world_from_local = mesh_functions::get_world_from_local(instance_index);
    let world_pos = mesh_functions::mesh_position_local_to_world(world_from_local, vec4(local_pos, 1.0));

    // Fill output (works for both forward and prepass layouts)
    out.position = position_world_to_clip(world_pos.xyz);
    out.world_position = world_pos;

    // Output normal (required by both passes)
#ifdef NORMAL_PREPASS_OR_DEFERRED_PREPASS
    out.world_normal = mesh_functions::mesh_normal_local_to_world(normal_vec, instance_index);
#endif

#ifndef PREPASS_PIPELINE
    out.world_normal = mesh_functions::mesh_normal_local_to_world(normal_vec, instance_index);
#endif

    return out;
}

// Minimal prepass fragment shader (depth only, no normals required)
#ifdef PREPASS_PIPELINE
@fragment
fn fragment(in: VertexOutput) -> @location(0) vec4<f32> {
    // Just output depth - don't use normals
    return vec4<f32>(0.0, 0.0, 0.0, 1.0);
}
#endif