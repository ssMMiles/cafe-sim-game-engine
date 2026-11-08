// Prepass fragment for toon materials: the world normal the edge detector
// reads (docs/EDGE_DETECTION.md), flipped on the back faces of double-sided
// materials.
//
// Without the flip every leaf and thin double-sided part would show its back
// with an inverted normal: spurious crease outlines wherever the two sides
// meet.
//
// Depth needs no fragment work; this only runs because the camera has a
// normal prepass. No material bindings are read, so it suits any toon-shaded
// material — `TOON_DOUBLE_SIDED` comes from the pipeline key, not a uniform.
// Drawn by the scene pass (`src/game/gfx/scene_pass`), whose modules provide
// the varyings and the per-instance winding flag.

import package::shaders::scene::toon_io::{VertexOutput, PrepassOutput};
import package::shaders::scene::bindings::scene_instance_winds_forward;

// Same as `facing_normal` in toon_lighting.wesl; keep the two in step.
fn facing_normal(world_normal: vec3<f32>, is_front: bool, instance_index: u32) -> vec3<f32> {
    let front = is_front == scene_instance_winds_forward(instance_index);
@if(TOON_DOUBLE_SIDED)
    return select(-world_normal, world_normal, front);
@else
    return world_normal;
}

@fragment
fn fragment(
    in: VertexOutput,
    @builtin(front_facing) is_front: bool,
) -> PrepassOutput {
    var out: PrepassOutput;
    out.normal = vec4(facing_normal(in.world_normal, is_front, in.instance_index) * 0.5 + vec3(0.5), 1.0);
    return out;
}
