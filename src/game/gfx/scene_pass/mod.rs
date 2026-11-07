//! Our own geometry passes for the 3D view — see `docs/SCENE_PASS_PLAN.md`.
//!
//! Phase 1 scope: **terrain only**. `MaterialPlugin::<TerrainMaterial>` is
//! gone (`terrain/mod.rs`), and everything bevy_pbr used to do for a terrain
//! chunk — extract, specialize, queue, bin, batch, GPU-preprocess, build the
//! `mesh_view` bind group — happens here instead, in four systems and two
//! render passes. Toon materials still go through `MaterialPlugin`; a material
//! type migrates wholesale or not at all, because bevy has no per-entity
//! opt-out of a registered `MaterialPlugin` and an entity drawn by both paths
//! would be drawn twice.
//!
//! The dataflow, all in the render world:
//!
//! 1. [`terrain::extract_terrain_instances`] (`ExtractSchedule`) — every
//!    visible terrain chunk's mesh id, material id and transform, rebuilt each
//!    frame into [`SceneSources`]. At ≤81 chunks that is microseconds; a
//!    retained map keyed by `MainEntity` is the upgrade if it ever isn't.
//! 2. [`terrain::queue_terrain_draws`] (`RenderSystems::Queue`) — per view,
//!    walks that view's `RenderVisibleEntities` (so render layers and frustum
//!    culling are bevy's, and the thumbnail rig on layer 7 never sees
//!    terrain), appends to the frame's instance list, specializes the colour
//!    and prepass pipelines, and leaves a [`SceneDraws`] component on the view.
//! 3. [`prepare_scene_instances`] (`PrepareResources`) uploads the instance
//!    list, [`prepare_scene_bind_group`] (`PrepareBindGroups`) builds bind
//!    group 0 from it plus bevy's own view uniform.
//! 4. [`pass::scene_prepass`] and [`pass::scene_opaque`] draw it, as plain
//!    `Core3d` systems — the same extension mechanism `post_process.rs` and
//!    `atmosphere.rs` already use.
//!
//! **Clear ownership.** Bevy's `early_prepass` and `main_opaque_pass_3d` still
//! run (they key off the *existence* of the view's binned phases, which
//! bevy_core_pipeline creates, not off those phases having items), so the
//! stragglers still draw and `early_prepass` still copies the depth buffer
//! into `ViewPrepassTextures.depth` at its end. Both attachments clear on
//! first use within the frame (`ColorAttachment`/`DepthAttachment` track that
//! with an atomic), so ours runs *before* bevy's in each case: we clear, bevy
//! loads, and the depth copy sees our geometry.

use bevy::{
    core_pipeline::{
        Core3d, Core3dSystems, core_3d::main_opaque_pass_3d, prepass::node::early_prepass,
    },
    prelude::*,
    render::{
        Render, RenderApp, RenderSystems,
        render_resource::{
            BindGroup, BindGroupEntries, BindGroupLayoutDescriptor, BindGroupLayoutEntries,
            BufferUsages, BufferVec, CachedRenderPipelineId, PipelineCache, ShaderStages,
            ShaderType,
            binding_types::{storage_buffer_read_only, uniform_buffer},
        },
        renderer::{RenderDevice, RenderQueue},
        sync_world::MainEntity,
        view::{ViewUniform, ViewUniforms},
    },
};

use bevy::{ecs::entity::EntityHashMap, platform::collections::HashMap};

use crate::game::world::terrain::material::TerrainMaterial;

pub mod pass;
pub mod pipeline;
pub mod terrain;

/// Every system that appends to [`SceneInstances`] and writes a view's
/// [`SceneDraws`], so [`clear_scene_draws`] can be ordered before all of them
/// at once. One member in Phase 1; Phase 3 adds the toon queue.
#[derive(SystemSet, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SceneQueue;

/// One instance's slot in the scene-pass instance buffer. Mirrors
/// `SceneInstance` in `assets/shaders/scene/bindings.wesl` field for field.
///
/// Written through `encase` (via [`BufferVec`]) rather than `bytemuck`: glam's
/// `Mat3` is 36 bytes but WGSL's `mat3x3<f32>` is 48 (16-byte column stride),
/// so a plain byte copy would shear every instance past the first.
#[derive(Clone, Copy, Debug, ShaderType)]
pub struct SceneInstance {
    pub world_from_local: Mat4,
    /// Inverse transpose of the upper 3×3, so normals survive non-uniform
    /// scale.
    pub normal_from_local: Mat3,
    pub flags: u32,
}

impl SceneInstance {
    /// Set when the 3×3 has a *positive* determinant, i.e. the instance is not
    /// mirrored. Same bit, same polarity and same meaning as bevy's
    /// `MESH_FLAGS_SIGN_DETERMINANT_MODEL_3X3_BIT`, which is what the toon
    /// shaders read today — Phase 3 moves them onto this one unchanged.
    pub const SIGN_DETERMINANT: u32 = 1;

    pub fn from_transform(transform: &GlobalTransform) -> Self {
        let affine = transform.affine();
        let matrix3 = Mat3::from(affine.matrix3);
        Self {
            world_from_local: Mat4::from(affine),
            normal_from_local: matrix3.inverse().transpose(),
            flags: if matrix3.determinant() >= 0.0 {
                Self::SIGN_DETERMINANT
            } else {
                0
            },
        }
    }
}

impl Default for SceneInstance {
    fn default() -> Self {
        Self {
            world_from_local: Mat4::IDENTITY,
            normal_from_local: Mat3::IDENTITY,
            flags: Self::SIGN_DETERMINANT,
        }
    }
}

/// What extraction leaves for the queue step: one entry per visible entity the
/// scene pass owns, keyed by its main-world entity so a view's
/// `RenderVisibleEntities` can be joined against it directly.
///
/// Rebuilt every frame (`clear` + fill), which is why it is a plain map rather
/// than anything change-driven.
#[derive(Resource, Default)]
pub struct SceneSources {
    pub terrain: HashMap<MainEntity, terrain::TerrainSource>,
}

/// The frame's instance array: every view's instances in one buffer, each draw
/// holding the range that is its own.
#[derive(Resource)]
pub struct SceneInstances(pub BufferVec<SceneInstance>);

impl Default for SceneInstances {
    fn default() -> Self {
        let mut buffer = BufferVec::new(BufferUsages::STORAGE);
        buffer.set_label(Some("scene_instances"));
        Self(buffer)
    }
}

/// One `draw_indexed` call: a run of instances sharing a mesh and a material.
///
/// Terrain chunks each own their lightmap and therefore their material, so
/// every run is length 1 and the draw count equals the loaded chunk count.
/// The structure is what makes Phase 3's probe-lit furniture batch.
pub struct SceneDraw {
    pub mesh: AssetId<Mesh>,
    pub material: AssetId<TerrainMaterial>,
    pub colour_pipeline: CachedRenderPipelineId,
    pub prepass_pipeline: CachedRenderPipelineId,
    pub instances: std::ops::Range<u32>,
}

/// What each view draws this frame, in submission order, by render-world view
/// entity.
///
/// A resource rather than a component on the view: the lists are render-world
/// scratch, rebuilt from nothing every frame, so putting them on the view
/// would buy an archetype move and a command round-trip and nothing else.
/// Clearing the whole map is also how a view that went away stops being drawn.
#[derive(Resource, Default)]
pub struct SceneDraws(pub EntityHashMap<Vec<SceneDraw>>);

/// Bind group 0: bevy's view uniform (dynamic offset) + our instance array.
/// One for the whole frame — the view uniform's offset is what makes it
/// per-view.
#[derive(Resource)]
pub struct SceneBindGroup(pub BindGroup);

/// The layout of [`SceneBindGroup`], as a descriptor so the pipeline cache can
/// hand out the real layout (how `post_process.rs` does it too).
#[derive(Resource, Clone)]
pub struct SceneBindGroupLayout(pub BindGroupLayoutDescriptor);

pub struct ScenePassPlugin;

impl Plugin for ScenePassPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(terrain::TerrainScenePlugin);

        let Some(render_app) = app.get_sub_app_mut(RenderApp) else {
            return;
        };

        render_app
            // Inserted at build time, not in `RenderStartup`: it needs no
            // device, and `init_terrain_scene_pipeline` reads it there. Two
            // unordered `RenderStartup` systems with a resource between them
            // is a coin flip the loser never gets to retry — the schedule
            // runs once.
            .insert_resource(scene_bind_group_layout())
            .init_resource::<SceneSources>()
            .init_resource::<SceneInstances>()
            .init_resource::<SceneDraws>()
            .add_systems(
                Render,
                (
                    clear_scene_draws
                        .in_set(RenderSystems::Queue)
                        .before(SceneQueue),
                    prepare_scene_instances.in_set(RenderSystems::PrepareResources),
                    prepare_scene_bind_group.in_set(RenderSystems::PrepareBindGroups),
                ),
            )
            .add_systems(
                Core3d,
                (
                    // Before bevy's, so we clear the attachments and its depth
                    // copy into `ViewPrepassTextures` includes our geometry.
                    pass::scene_prepass
                        .in_set(Core3dSystems::Prepass)
                        .before(early_prepass),
                    pass::scene_opaque
                        .in_set(Core3dSystems::MainPass)
                        .before(main_opaque_pass_3d),
                ),
            );
    }
}

fn scene_bind_group_layout() -> SceneBindGroupLayout {
    let entries = BindGroupLayoutEntries::sequential(
        // The instance array is only read by the vertex stage today, but
        // `VERTEX_FRAGMENT` costs nothing and Phase 3's toon fragment wants
        // the instance flags for its front/back test.
        ShaderStages::VERTEX_FRAGMENT,
        (
            uniform_buffer::<ViewUniform>(true),
            storage_buffer_read_only::<SceneInstance>(false),
        ),
    );
    SceneBindGroupLayout(BindGroupLayoutDescriptor::new(
        "scene_bind_group_layout",
        &entries,
    ))
}

/// Start the frame's instance list and draw lists empty; the [`SceneQueue`]
/// systems refill them straight after.
fn clear_scene_draws(mut instances: ResMut<SceneInstances>, mut draws: ResMut<SceneDraws>) {
    instances.0.clear();
    draws.0.clear();
}

/// Upload the frame's instances.
///
/// A zero-length buffer has no binding, and a frame with no terrain loaded
/// (the main menu, a world still streaming in) is ordinary — one padding
/// instance keeps bind-group creation unconditional rather than making every
/// later step handle the hole.
fn prepare_scene_instances(
    mut instances: ResMut<SceneInstances>,
    device: Res<RenderDevice>,
    queue: Res<RenderQueue>,
) {
    if instances.0.is_empty() {
        instances.0.push(SceneInstance::default());
    }
    instances.0.write_buffer(&device, &queue);
}

fn prepare_scene_bind_group(
    mut commands: Commands,
    device: Res<RenderDevice>,
    pipeline_cache: Res<PipelineCache>,
    layout: Res<SceneBindGroupLayout>,
    view_uniforms: Res<ViewUniforms>,
    instances: Res<SceneInstances>,
) {
    let (Some(view_binding), Some(instance_binding)) =
        (view_uniforms.uniforms.binding(), instances.0.binding())
    else {
        commands.remove_resource::<SceneBindGroup>();
        return;
    };

    let bind_group = device.create_bind_group(
        "scene_bind_group",
        &pipeline_cache.get_bind_group_layout(&layout.0),
        &BindGroupEntries::sequential((view_binding, instance_binding)),
    );
    commands.insert_resource(SceneBindGroup(bind_group));
}
