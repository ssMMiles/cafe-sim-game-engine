// Bind group 0 of the scene pass: everything a draw needs that isn't the
// material. Filled by `src/game/gfx/scene_pass` — see docs/SCENE_PASS_PLAN.md.
//
// Binding 0 is bevy's own per-view uniform buffer (`ViewUniforms`), bound with
// a dynamic offset. We reuse bevy_render's buffer and its `View` struct rather
// than keeping a second copy of the camera matrices in step; `bevy_render` is
// on the keep side of the plan's keep/drop table, `bevy_pbr` is not.
//
// Binding 1 is the per-frame instance array. WebGPU guarantees
// `@builtin(instance_index)` is `firstInstance + i`, so a draw issued over the
// range `[start, end)` indexes exactly its own slice with no extra uniform.

import bevy_render::view::View;

// Mirrors `SceneInstance` in scene_pass/instances.rs field for field.
struct SceneInstance {
    world_from_local: mat4x4<f32>,
    // Inverse transpose of the upper 3x3, so normals survive non-uniform scale.
    normal_from_local: mat3x3<f32>,
    // See `SCENE_INSTANCE_SIGN_DETERMINANT`.
    flags: u32,
}

// Set when the 3x3 has a *positive* determinant, i.e. the instance is not
// mirrored. A mirrored instance winds its triangles the other way round, so a
// double-sided material's `@builtin(front_facing)` has to be read against this
// bit rather than on its own. Same convention (and same meaning of "set") as
// bevy's `MESH_FLAGS_SIGN_DETERMINANT_MODEL_3X3_BIT`, which the toon shaders
// read today; mirrored in `SceneInstanceFlags` in scene_pass/instances.rs.
const SCENE_INSTANCE_SIGN_DETERMINANT: u32 = 1u;

@group(0) @binding(0) var<uniform> view: View;
@group(0) @binding(1) var<storage> instances: array<SceneInstance>;

fn scene_position_world_to_clip(world: vec3<f32>) -> vec4<f32> {
    return view.clip_from_world * vec4<f32>(world, 1.0);
}
