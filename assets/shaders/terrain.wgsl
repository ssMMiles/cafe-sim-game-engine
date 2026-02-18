#import bevy_pbr::{
    mesh_functions,
    view_transformations::position_world_to_clip,
    forward_io::VertexOutput,
}

@vertex
fn vertex(
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @builtin(instance_index) instance_index: u32,
) -> VertexOutput {
    var out: VertexOutput;

    let world_from_local = mesh_functions::get_world_from_local(instance_index);
    let world_pos = mesh_functions::mesh_position_local_to_world(world_from_local, vec4(position, 1.0));

    out.position = position_world_to_clip(world_pos.xyz);
    out.world_position = world_pos;

    // Skirt vertices have zero normal — give them a downward-facing normal
    let is_skirt = dot(normal, normal) < 0.5;
    if is_skirt {
        out.world_normal = vec3(0.0, -1.0, 0.0);
    } else {
        out.world_normal = normal;
    }

#ifdef VERTEX_OUTPUT_INSTANCE_INDEX
    out.instance_index = instance_index;
#endif

    return out;
}
