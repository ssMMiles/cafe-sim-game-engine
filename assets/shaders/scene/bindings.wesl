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
//
// Binding 2 is bevy_pbr's own joint-matrix buffer (`SkinUniforms`), which it
// keeps preparing while `PbrPlugin` is in the app: `instance.skin_index` is
// where a skinned instance's joints start in it. Only the `SKINNED` vertex
// variant reads it; the layout entry is there for every pipeline.
//
// Binding 3 is the per-scene uniform: what every toon surface needs from the
// level that isn't in its material — the sun, the toon curve, the probe
// lattice. One write per frame instead of one per material.

import bevy_render::view::View;

// Mirrors `SceneInstance` in scene_pass/instances.rs field for field.
struct SceneInstance {
    world_from_local: mat4x4<f32>,
    // Inverse transpose of the upper 3x3, so normals survive non-uniform scale.
    normal_from_local: mat3x3<f32>,
    // See `SCENE_INSTANCE_SIGN_DETERMINANT`.
    flags: u32,
    // First joint matrix of this instance's skin in `joint_matrices`; unused
    // (zero) for anything not skinned.
    skin_index: u32,
}

// Set when the 3x3 has a *positive* determinant, i.e. the instance is not
// mirrored. A mirrored instance winds its triangles the other way round, so a
// double-sided material's `@builtin(front_facing)` has to be read against this
// bit rather than on its own. Same convention (and same meaning of "set") as
// bevy's `MESH_FLAGS_SIGN_DETERMINANT_MODEL_3X3_BIT`, which the toon shaders
// read today; mirrored in `SceneInstanceFlags` in scene_pass/instances.rs.
const SCENE_INSTANCE_SIGN_DETERMINANT: u32 = 1u;

// Mirrors `SceneUniform` in scene_pass/mod.rs field for field.
struct SceneUniform {
    // Unit direction toward the sun, and its radiance in lightmap units.
    sun_dir: vec4<f32>,
    sun_rgb: vec4<f32>,
    // The toon ramp: (bands, floor, white, gamma). See `toon_shade`.
    curve: vec4<f32>,
    // xyz: probe volume texel dims; w: probe cell size in metres.
    probe: vec4<f32>,
}

@group(0) @binding(0) var<uniform> view: View;
@group(0) @binding(1) var<storage> instances: array<SceneInstance>;
@group(0) @binding(2) var<storage> joint_matrices: array<mat4x4<f32>>;
@group(0) @binding(3) var<uniform> scene: SceneUniform;

fn scene_position_world_to_clip(world: vec3<f32>) -> vec4<f32> {
    return view.clip_from_world * vec4<f32>(world, 1.0);
}

// `SCENE_INSTANCE_SIGN_DETERMINANT`, read: whether `instance_index`'s
// triangles wind the way the mesh data says. A double-sided material compares
// `@builtin(front_facing)` against this to decide which way its normal faces.
fn scene_instance_winds_forward(instance_index: u32) -> bool {
    return (instances[instance_index].flags & SCENE_INSTANCE_SIGN_DETERMINANT) != 0u;
}

// Bits 1-2 of the flags: the instance's outline weight as a 2-bit level,
// weight = level / 3 (mirrors `SceneInstance::OUTLINE_LEVEL_SHIFT`,
// scene_pass/mod.rs). The toon fragment writes it to the normal target's
// 2-bit alpha; the edge detector scales its lines by the strongest weight in
// its sampling neighbourhood, so small scenery (grass) loses its interior
// scribble but keeps its silhouette against full-weight surroundings.
fn scene_instance_outline_weight(instance_index: u32) -> f32 {
    return f32((instances[instance_index].flags >> 1u) & 3u) / 3.0;
}

// The skinned object-to-world matrix: a weighted sum of four joint matrices,
// which already include the inverse bind pose and the object transform (that
// is what bevy's `extract_skins` writes). Same arithmetic as
// `bevy_pbr::render::skinning::skin_model`.
fn scene_skin_model(
    instance_index: u32,
    indexes: vec4<u32>,
    weights: vec4<f32>,
) -> mat4x4<f32> {
    let base = instances[instance_index].skin_index;
    return weights.x * joint_matrices[base + indexes.x]
        + weights.y * joint_matrices[base + indexes.y]
        + weights.z * joint_matrices[base + indexes.z]
        + weights.w * joint_matrices[base + indexes.w];
}

// Inverse transpose of a matrix's upper 3x3, for carrying a normal through a
// skinned (and so non-uniformly scaled) transform. From bevy's skinning.wesl.
fn scene_normal_matrix(m: mat4x4<f32>) -> mat3x3<f32> {
    let a = mat3x3<f32>(m[0].xyz, m[1].xyz, m[2].xyz);
    let x = cross(a[1], a[2]);
    let y = cross(a[2], a[0]);
    let z = cross(a[0], a[1]);
    let det = dot(a[2], z);
    return mat3x3<f32>(x / det, y / det, z / det);
}
