// Prepass fragment for toon materials: the world normal the edge detector
// reads (docs/EDGE_DETECTION.md), flipped on the back faces of double-sided
// materials.
//
// bevy's default prepass fragment writes `in.world_normal` as is. That was
// fine while toon extended StandardMaterial — its prepass shader did the flip —
// but a standalone material falls back to the default, and every leaf and
// thin double-sided part would then show its back with an inverted normal:
// spurious crease outlines wherever the two sides meet.
//
// Depth needs no fragment work; this only runs because the camera has a
// normal prepass. No material bindings are read, so it suits any toon-shaded
// material — `TOON_DOUBLE_SIDED` comes from the pipeline key, not a uniform.

import bevy_pbr::{
    prepass::io::{VertexOutput, FragmentOutput},
    render::{
        mesh_bindings::mesh,
        mesh_types::MESH_FLAGS_SIGN_DETERMINANT_MODEL_3X3_BIT,
    },
};

// Same as `facing_normal` in toon_lighting.wesl; keep the two in step.
fn facing_normal(world_normal: vec3<f32>, is_front: bool, instance_index: u32) -> vec3<f32> {
    let positive_determinant =
        (mesh[instance_index].flags & MESH_FLAGS_SIGN_DETERMINANT_MODEL_3X3_BIT) != 0u;
    let front = is_front == positive_determinant;
@if(TOON_DOUBLE_SIDED)
    return select(-world_normal, world_normal, front);
@else
    return world_normal;
}

@fragment
fn fragment(
    in: VertexOutput,
    @builtin(front_facing) is_front: bool,
) -> FragmentOutput {
    var out: FragmentOutput;
@if(NORMAL_PREPASS)
    out.normal = vec4(facing_normal(in.world_normal, is_front, in.instance_index) * 0.5 + vec3(0.5), 1.0);
@if(UNCLIPPED_DEPTH_ORTHO_EMULATION)
    out.frag_depth = in.unclipped_depth;
    return out;
}
