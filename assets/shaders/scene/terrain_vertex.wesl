// Terrain vertex stage for the scene pass, shared by the colour and prepass
// pipelines (scene_pass/pipeline.rs binds it to both).
//
// The transform comes from our instance buffer rather than bevy's
// `mesh_functions::get_world_from_local`, which is the only thing that
// changed from the shader this replaces — plus the skirt flip now happens in
// the *prepass* too. It never used to: bevy's default prepass vertex shader
// knows nothing about skirts, so a skirt's zero normal reached the edge
// detector as a zero vector and every chunk edge could draw a spurious crease
// (docs/GRAPHICS.md §17).

import package::shaders::scene::bindings::{instances, scene_position_world_to_clip};
import package::shaders::scene::terrain_io::VertexOutput;

@vertex
fn vertex(
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) uv_b: vec2<f32>,        // UV1 — lightmap coords
    @builtin(instance_index) instance_index: u32,
) -> VertexOutput {
    var out: VertexOutput;

    let instance = instances[instance_index];
    let world_position = instance.world_from_local * vec4<f32>(position, 1.0);

    out.position = scene_position_world_to_clip(world_position.xyz);
    out.world_position = world_position;

    // Skirt vertices carry a zero normal — give them a downward-facing one.
    // Chunk meshes are axis-aligned and unscaled, but go through the instance
    // normal matrix anyway so a rotated or scaled chunk would still be right.
    let is_skirt = dot(normal, normal) < 0.5;
    let local_normal = select(normal, vec3<f32>(0.0, -1.0, 0.0), is_skirt);
    out.world_normal = normalize(instance.normal_from_local * local_normal);

    out.uv_b = uv_b;

    return out;
}
