//! Our own geometry passes for the 3D view — see `docs/SCENE_PASS_PLAN.md`.
//!
//! Both of our materials — `TerrainMaterial` (Phase 1) and `ToonMaterial`
//! (Phase 3) — draw through here, not through bevy_pbr's `MaterialPlugin`.
//! Everything bevy_pbr did for them — extract, specialize, queue, bin, batch,
//! GPU-preprocess, build the `mesh_view` bind group — happens here instead, in
//! a handful of systems and two render passes. What still goes through
//! bevy_pbr: the sky dome (Phase 4), the `StandardMaterial` stragglers (ghosts,
//! blob shadows, brush disc, thumbnails) and skinning's joint-matrix upload,
//! which we bind rather than port. A material type migrates wholesale or not
//! at all: bevy has no per-entity opt-out of a registered `MaterialPlugin`, so
//! an entity reachable by both paths would be drawn twice.
//!
//! The dataflow, all in the render world, generic over the material type `M`
//! (`material.rs`):
//!
//! 1. `extract_instances::<M>` (`ExtractSchedule`) — every visible `M`
//!    entity's mesh id, material id and transform, rebuilt each frame into
//!    `SceneSources<M>`.
//! 2. `queue_draws::<M>` (`RenderSystems::Queue`) — per view, walks that
//!    view's `RenderVisibleEntities` (so render layers and frustum culling are
//!    bevy's, and the thumbnail rig on layer 7 sees none of it), appends to
//!    the frame's instance list grouped by `(material, mesh)`, specializes the
//!    colour and prepass pipelines (`pipeline.rs`), and appends to the view's
//!    entry in [`SceneDraws`].
//! 3. [`prepare_scene_instances`] (`PrepareResources`) uploads the instance
//!    list and the per-scene uniform ([`SceneUniform`]: sun, toon curve, probe
//!    lattice — extracted from the main world's `ToonLook`);
//!    [`prepare_scene_bind_group`] (`PrepareBindGroups`) builds bind group 0
//!    from both plus bevy's own view uniform and joint-matrix buffer.
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
    ecs::entity::EntityHashMap,
    pbr::SkinUniforms,
    prelude::*,
    render::{
        Render, RenderApp, RenderSystems,
        extract_resource::ExtractResourcePlugin,
        render_resource::{
            BindGroup, BindGroupEntries, BindGroupLayoutDescriptor, BindGroupLayoutEntries,
            BufferUsages, BufferVec, CachedRenderPipelineId, PipelineCache, ShaderStages,
            ShaderType, UniformBuffer,
            binding_types::{
                storage_buffer_read_only, storage_buffer_read_only_sized, uniform_buffer,
            },
        },
        renderer::{RenderDevice, RenderQueue},
        view::{ViewUniform, ViewUniforms},
    },
};
use engine_api::{
    ToonMaterial,
    toon_material::{PROBE_CELL_SIZE, PROBE_VOLUME_DIMS},
};

use crate::game::gfx::toon_material::ToonLook;

use crate::game::world::terrain::material::TerrainMaterial;

pub mod material;
pub mod pass;
pub mod pipeline;

use material::{SceneMaterial, SceneMaterialPlugin};
use pipeline::{TerrainScenePipeline, ToonScenePipeline};

impl SceneMaterial for TerrainMaterial {
    type Pipeline = TerrainScenePipeline;
}

impl SceneMaterial for ToonMaterial {
    type Pipeline = ToonScenePipeline;
}

/// Every system that appends to [`SceneInstances`] and writes a view's
/// [`SceneDraws`], so [`clear_scene_draws`] can be ordered before all of them
/// at once: one `queue_draws::<M>` per material type.
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
    /// Where this instance's joint matrices start in bevy's `SkinUniforms`
    /// buffer (bind group 0, binding 2). Set at queue time; 0 when not
    /// skinned, and then never read.
    pub skin_index: u32,
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
            skin_index: 0,
        }
    }
}

impl Default for SceneInstance {
    fn default() -> Self {
        Self {
            world_from_local: Mat4::IDENTITY,
            normal_from_local: Mat3::IDENTITY,
            flags: Self::SIGN_DETERMINANT,
            skin_index: 0,
        }
    }
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
/// Terrain chunks and lightmapped objects each own their material, so their
/// runs are length 1; probe-lit furniture sharing a material and a mesh is one
/// instanced draw. The material is its prepared bind group (cheap `Arc`
/// clone), so the pass needs to know nothing about material types.
pub struct SceneDraw {
    pub mesh: AssetId<Mesh>,
    pub material: BindGroup,
    pub colour_pipeline: CachedRenderPipelineId,
    pub prepass_pipeline: CachedRenderPipelineId,
    pub instances: std::ops::Range<u32>,
}

/// Everything a toon surface needs from the level that isn't in its material:
/// one buffer for every draw of every material. Mirrors `SceneUniform` in
/// `assets/shaders/scene/bindings.wesl` field for field.
///
/// Filled from the main world's `ToonLook` each frame. Having this be one
/// uniform rather than fields on every material is the whole point: an
/// environment edit used to rewrite every toon *and* terrain material
/// (`sync_toon_look`), which under the scene pass would have rebuilt every
/// material bind group; now it is one 64-byte write.
#[derive(Clone, Copy, Debug, ShaderType)]
pub struct SceneUniform {
    /// Unit direction *toward* the sun (w unused).
    pub sun_dir: Vec4,
    /// Sun colour × intensity in lightmap units (w unused).
    pub sun_rgb: Vec4,
    /// The toon ramp as `(bands, floor, white, gamma)` — `ToonCurve`.
    pub curve: Vec4,
    /// `PROBE_VOLUME_DIMS` as floats in xyz, `PROBE_CELL_SIZE` in w, so the
    /// shader and the bake agree on the lattice.
    pub probe: Vec4,
}

impl From<&ToonLook> for SceneUniform {
    fn from(look: &ToonLook) -> Self {
        let [w, h, d] = PROBE_VOLUME_DIMS;
        Self {
            sun_dir: look.sun_dir.normalize_or(Vec3::Y).extend(0.0),
            sun_rgb: look.sun_rgb.extend(0.0),
            curve: look.curve.to_vec4(),
            probe: Vec4::new(w as f32, h as f32, d as f32, PROBE_CELL_SIZE),
        }
    }
}

impl Default for SceneUniform {
    fn default() -> Self {
        Self::from(&ToonLook::default())
    }
}

/// The GPU side of [`SceneUniform`].
#[derive(Resource, Default)]
pub struct SceneUniformBuffer(pub UniformBuffer<SceneUniform>);

/// What each view draws this frame, in submission order, by render-world view
/// entity.
///
/// A resource rather than a component on the view: the lists are render-world
/// scratch, rebuilt from nothing every frame, so putting them on the view
/// would buy an archetype move and a command round-trip and nothing else.
/// Clearing the whole map is also how a view that went away stops being drawn.
#[derive(Resource, Default)]
pub struct SceneDraws(pub EntityHashMap<Vec<SceneDraw>>);

/// Bind group 0: bevy's view uniform (dynamic offset), our instance array,
/// bevy's joint-matrix buffer, and our scene uniform. One for the whole frame — the view
/// uniform's offset is what makes it per-view. Rebuilt every frame because
/// `prepare_skins` swaps its current and previous buffers each frame.
#[derive(Resource)]
pub struct SceneBindGroup(pub BindGroup);

/// The layout of [`SceneBindGroup`], as a descriptor so the pipeline cache can
/// hand out the real layout (how `post_process.rs` does it too).
#[derive(Resource, Clone)]
pub struct SceneBindGroupLayout(pub BindGroupLayoutDescriptor);

pub struct ScenePassPlugin;

impl Plugin for ScenePassPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins((
            SceneMaterialPlugin::<TerrainMaterial>::default(),
            SceneMaterialPlugin::<ToonMaterial>::default(),
            ExtractResourcePlugin::<ToonLook>::default(),
        ));

        let Some(render_app) = app.get_sub_app_mut(RenderApp) else {
            return;
        };

        render_app
            // Inserted at build time, not in `RenderStartup`: it needs no
            // device, and the material pipelines read it there (see
            // `material::init_scene_material` for why that ordering matters).
            .insert_resource(scene_bind_group_layout())
            .init_resource::<SceneInstances>()
            .init_resource::<SceneUniformBuffer>()
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
        // The toon fragment reads the instance flags for its front/back test,
        // so the instance array is visible to both stages.
        ShaderStages::VERTEX_FRAGMENT,
        (
            uniform_buffer::<ViewUniform>(true),
            storage_buffer_read_only::<SceneInstance>(false),
            // bevy's joint matrices: a runtime array of mat4x4, read by the
            // `SKINNED` vertex variant only.
            storage_buffer_read_only_sized(false, None),
            uniform_buffer::<SceneUniform>(false),
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

/// Upload the frame's instances and the scene uniform.
///
/// A zero-length buffer has no binding, and a frame with nothing of ours in
/// view (the main menu, a world still streaming in) is ordinary — one padding
/// instance keeps bind-group creation unconditional rather than making every
/// later step handle the hole. The scene uniform is 64 bytes and written
/// every frame rather than on change; `ToonLook` is a `Copy` resource the
/// extract step copies whole anyway.
fn prepare_scene_instances(
    mut instances: ResMut<SceneInstances>,
    mut scene: ResMut<SceneUniformBuffer>,
    look: Option<Res<ToonLook>>,
    device: Res<RenderDevice>,
    queue: Res<RenderQueue>,
) {
    if instances.0.is_empty() {
        instances.0.push(SceneInstance::default());
    }
    instances.0.write_buffer(&device, &queue);

    // `ToonLook` is extracted every frame once `GfxPlugin` has initialised it;
    // until then (the very first frame) the default look is as good as any.
    let look = look.as_deref().copied().unwrap_or_default();
    scene.0.set(SceneUniform::from(&look));
    scene.0.write_buffer(&device, &queue);
}

fn prepare_scene_bind_group(
    mut commands: Commands,
    device: Res<RenderDevice>,
    pipeline_cache: Res<PipelineCache>,
    layout: Res<SceneBindGroupLayout>,
    view_uniforms: Res<ViewUniforms>,
    instances: Res<SceneInstances>,
    skins: Res<SkinUniforms>,
    scene: Res<SceneUniformBuffer>,
) {
    let (Some(view_binding), Some(instance_binding), Some(scene_binding)) = (
        view_uniforms.uniforms.binding(),
        instances.0.binding(),
        scene.0.binding(),
    ) else {
        commands.remove_resource::<SceneBindGroup>();
        return;
    };

    let bind_group = device.create_bind_group(
        "scene_bind_group",
        &pipeline_cache.get_bind_group_layout(&layout.0),
        &BindGroupEntries::sequential((
            view_binding,
            instance_binding,
            skins.current_buffer.as_entire_binding(),
            scene_binding,
        )),
    );
    commands.insert_resource(SceneBindGroup(bind_group));
}
