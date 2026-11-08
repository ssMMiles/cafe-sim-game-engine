//! The material-generic half of the scene pass: for a material type `M` that
//! the pass owns, extraction of its visible instances, its bind group as a
//! [`RenderAsset`], and the per-view draw list. [`SceneMaterialPlugin`] wires
//! one instance of each up for a material; the only thing that differs between
//! materials is the pipeline specializer `P` (`pipeline.rs`).
//!
//! `MaterialPlugin::<M>` used to do all of this. What it also did, and what
//! this does *not*, is the part the plan is after: a per-type specialization
//! check and queue system every frame, binned-phase construction, batching, a
//! GPU preprocessing dispatch, and the `mesh_view` bind group with its lights,
//! clusters, shadow maps, fog and SSAO bindings — none of which a fully baked
//! renderer reads.
//!
//! The material bind group is still built by bevy's own [`AsBindGroup`] derive
//! on `M`, through a [`RenderAsset`] impl. That keeps every existing path that
//! mutates a material working unchanged — a bake landing a lightmap, Shift+L
//! flipping `unlit`, `sync_dynamic_lit_source` swapping a probe-lit copy in,
//! the inspector editing the environment — because `RenderAssetPlugin`
//! re-prepares on the same `AssetEvent`s `MaterialPlugin` listened to.

use std::{hash::Hash, marker::PhantomData};

use bevy::{
    ecs::system::{SystemParamItem, lifetimeless::SRes},
    log::error_once,
    pbr::SkinUniforms,
    prelude::*,
    render::{
        Extract, ExtractSchedule, Render, RenderApp, RenderStartup, RenderSystems,
        material_bind_groups::FallbackBuffer,
        mesh::RenderMesh,
        render_asset::{
            PrepareAssetError, RenderAsset, RenderAssetPlugin, RenderAssets, prepare_assets,
        },
        render_resource::{
            AsBindGroup, BindGroup, BindGroupLayoutDescriptor, PipelineCache,
            SpecializedMeshPipeline, SpecializedMeshPipelines,
        },
        renderer::RenderDevice,
        storage::GpuShaderBuffer,
        sync_world::MainEntity,
        texture::GpuImage,
        view::{ExtractedView, RenderVisibleEntities, ViewTarget},
    },
};

use bevy::platform::collections::HashMap;

use super::{
    SceneDraw, SceneDraws, SceneInstance, SceneInstances, SceneLayer, SceneQueue,
    pipeline::{ScenePassKind, SceneViewKey},
};

/// A material type the scene pass draws: bevy's `Material` bound (for the
/// authoring component `MeshMaterial3d<M>`) plus a pipeline specializer whose
/// key is built from the view and the material's own [`AsBindGroup::Data`].
pub trait SceneMaterial: Material + AsBindGroup<Data: Clone + Hash + Eq + Send + Sync> {
    type Pipeline: ScenePipeline<Self>;
    /// Where in the frame this material's geometry is drawn, and whether it
    /// has a prepass variant at all. Almost everything is [`SceneLayer::Opaque`].
    const LAYER: SceneLayer = SceneLayer::Opaque;
}

/// The specializer for one [`SceneMaterial`]: built once in `RenderStartup`
/// from the bind group layouts, then asked for a pipeline per
/// `(view, material key, mesh layout)` by [`queue_draws`].
pub trait ScenePipeline<M: AsBindGroup>:
    SpecializedMeshPipeline<Key: Send + Sync + 'static> + FromWorld + Resource
{
    fn key(view: SceneViewKey, pass: ScenePassKind, material: &M::Data) -> Self::Key;
}

pub struct SceneMaterialPlugin<M>(PhantomData<fn() -> M>);

impl<M> Default for SceneMaterialPlugin<M> {
    fn default() -> Self {
        Self(PhantomData)
    }
}

impl<M: SceneMaterial> Plugin for SceneMaterialPlugin<M> {
    fn build(&self, app: &mut App) {
        // What `MaterialPlugin` registered that still has to exist; the rest of
        // what it added (extraction, specialization, queueing) is replaced
        // below. A type migrates wholesale: with `MaterialPlugin::<M>` still
        // on, every `M` entity would be drawn twice.
        app.init_asset::<M>()
            .register_type::<MeshMaterial3d<M>>()
            // `GpuImage` as the AFTER parameter: the bind group binds textures
            // (lightmaps, base colour, the probe volume), so those have to be
            // on the GPU before `prepare_asset` runs or every material would
            // spend a frame retrying.
            .add_plugins(RenderAssetPlugin::<PreparedMaterial<M>, GpuImage>::default());

        let Some(render_app) = app.get_sub_app_mut(RenderApp) else {
            return;
        };
        render_app
            .init_resource::<SceneSources<M>>()
            .init_resource::<SpecializedMeshPipelines<M::Pipeline>>()
            .add_systems(RenderStartup, init_scene_material::<M>)
            .add_systems(ExtractSchedule, extract_instances::<M>)
            .add_systems(
                Render,
                queue_draws::<M>
                    .in_set(RenderSystems::Queue)
                    .in_set(SceneQueue)
                    // The vertex layout a pipeline is specialized against
                    // comes from the prepared mesh.
                    .after(prepare_assets::<RenderMesh>),
            );
    }
}

/// The material's bind group layout (group 1), as the descriptor the pipeline
/// cache hands real layouts out for. Built once from the device; both the
/// pipeline and [`PreparedMaterial`] use literally this descriptor, so the two
/// cannot disagree.
#[derive(Resource)]
pub struct MaterialLayout<M>(pub BindGroupLayoutDescriptor, PhantomData<fn() -> M>);

/// One exclusive system rather than two: the pipeline's `FromWorld` reads the
/// layout, and two unordered `RenderStartup` systems with a resource between
/// them is a coin flip the loser never gets to retry (the schedule runs once;
/// a missing `Res` just skips the system).
fn init_scene_material<M: SceneMaterial>(world: &mut World) {
    let layout = M::bind_group_layout_descriptor(world.resource::<RenderDevice>());
    world.insert_resource(MaterialLayout::<M>(layout, PhantomData));
    let pipeline = M::Pipeline::from_world(world);
    world.insert_resource(pipeline);
}

// ---------------------------------------------------------------------------
// The material bind group
// ---------------------------------------------------------------------------

/// `M` as bind group 1, plus the key its pipeline is specialized on.
pub struct PreparedMaterial<M: AsBindGroup> {
    pub bind_group: BindGroup,
    pub key: M::Data,
}

impl<M: SceneMaterial> RenderAsset for PreparedMaterial<M> {
    type SourceAsset = M;

    /// The last element is `AsBindGroup`'s own parameter tuple, nested so it
    /// can be handed to [`AsBindGroup::as_bind_group`] whole.
    type Param = (
        SRes<RenderDevice>,
        SRes<PipelineCache>,
        SRes<FallbackBuffer>,
        SRes<RenderAssets<GpuShaderBuffer>>,
        SRes<MaterialLayout<M>>,
        M::Param,
    );

    fn prepare_asset(
        material: Self::SourceAsset,
        _asset_id: AssetId<Self::SourceAsset>,
        (device, pipeline_cache, fallback_buffer, shader_buffers, layout, as_bind_group): &mut SystemParamItem<Self::Param>,
        _previous: Option<&Self>,
    ) -> Result<Self, PrepareAssetError<Self::SourceAsset>> {
        material
            .as_bind_group(
                &layout.0,
                device,
                pipeline_cache,
                fallback_buffer,
                shader_buffers,
                as_bind_group,
            )
            .map(|prepared| Self {
                bind_group: prepared.bind_group,
                key: material.bind_group_data(),
            })
            // A texture that hasn't finished uploading: try again next frame.
            .map_err(PrepareAssetError::AsBindGroupError)
    }
}

// ---------------------------------------------------------------------------
// Extract
// ---------------------------------------------------------------------------

/// What extraction keeps about one visible entity of material `M`.
pub struct SourceInstance<M: Asset> {
    pub mesh: AssetId<Mesh>,
    pub material: AssetId<M>,
    pub instance: SceneInstance,
}

/// Every visible `M` entity, keyed by its main-world entity so a view's
/// `RenderVisibleEntities` can be joined against it directly.
///
/// Rebuilt every frame (`clear` + fill). At the hundreds of entities this
/// project has that is microseconds; a retained map is the upgrade if the
/// Systems table ever says otherwise.
#[derive(Resource)]
pub struct SceneSources<M: Asset>(pub HashMap<MainEntity, SourceInstance<M>>);

impl<M: Asset> Default for SceneSources<M> {
    fn default() -> Self {
        Self(HashMap::default())
    }
}

/// `ViewVisibility` is the cheap pre-filter (hierarchy visibility and whether
/// *any* view can see it); the per-view frustum and render-layer decision is
/// bevy's `RenderVisibleEntities`, read in [`queue_draws`].
fn extract_instances<M: SceneMaterial>(
    mut sources: ResMut<SceneSources<M>>,
    entities: Extract<
        Query<(
            Entity,
            &ViewVisibility,
            &GlobalTransform,
            &Mesh3d,
            &MeshMaterial3d<M>,
        )>,
    >,
) {
    sources.0.clear();
    for (entity, visibility, transform, mesh, material) in &entities {
        if !visibility.get() {
            continue;
        }
        sources.0.insert(
            MainEntity::from(entity),
            SourceInstance {
                mesh: mesh.id(),
                material: material.id(),
                instance: SceneInstance::from_transform(transform),
            },
        );
    }
}

// ---------------------------------------------------------------------------
// Queue
// ---------------------------------------------------------------------------

/// Build each 3D view's draw list for `M` and append its instances.
///
/// Draws are grouped by `(material, mesh)`, so probe-lit furniture sharing one
/// material and one mesh is a single instanced draw; a lightmapped object owns
/// its material and is a run of one, as is every terrain chunk.
///
/// Skinning: an entity bevy's `extract_skins` allocated joints for (it runs in
/// the same `ExtractSchedule`) gets their offset in `SkinUniforms`' buffer as
/// its `skin_index`, which the `SKINNED` vertex variant indexes bind group 0's
/// joint-matrix binding with. Nothing here decides whether a mesh *is*
/// skinned — the pipeline does, from the vertex layout.
fn queue_draws<M: SceneMaterial>(
    sources: Res<SceneSources<M>>,
    mut view_draws: ResMut<SceneDraws>,
    mut instances: ResMut<SceneInstances>,
    mut pipelines: ResMut<SpecializedMeshPipelines<M::Pipeline>>,
    pipeline: Res<M::Pipeline>,
    pipeline_cache: Res<PipelineCache>,
    meshes: Res<RenderAssets<RenderMesh>>,
    materials: Res<RenderAssets<PreparedMaterial<M>>>,
    skins: Res<SkinUniforms>,
    views: Query<
        (
            Entity,
            &ExtractedView,
            &RenderVisibleEntities,
            &Msaa,
            &ViewTarget,
        ),
        With<Camera3d>,
    >,
) {
    if sources.0.is_empty() {
        return;
    }

    for (view_entity, view, visible, msaa, target) in &views {
        let view_position = view.world_from_view.translation();
        let Some(class) = visible.get::<Mesh3d>() else {
            continue;
        };
        let view_key = SceneViewKey {
            msaa_samples: msaa.samples(),
            target_format: target.main_texture_format(),
        };

        // Sort so equal (material, mesh) pairs are adjacent; the run-length
        // grouping below is what turns them into one instanced draw.
        let mut visible_sources: Vec<(MainEntity, &SourceInstance<M>)> = class
            .entities_cpu_culling
            .iter()
            .filter_map(|(_, main)| sources.0.get(main).map(|s| (*main, s)))
            .collect();
        visible_sources.sort_unstable_by_key(|(_, s)| (s.material, s.mesh));

        let mut draws = Vec::new();
        let mut index = 0;
        while index < visible_sources.len() {
            let head = visible_sources[index].1;
            let end = visible_sources[index..]
                .iter()
                .position(|(_, s)| (s.material, s.mesh) != (head.material, head.mesh))
                .map_or(visible_sources.len(), |offset| index + offset);
            let run = &visible_sources[index..end];
            index = end;

            // A material whose textures are still uploading, or a mesh still
            // being written to the GPU, simply doesn't draw this frame.
            let (Some(material), Some(render_mesh)) =
                (materials.get(head.material), meshes.get(head.mesh))
            else {
                continue;
            };

            let mut specialize = |pass| {
                pipelines.specialize(
                    &pipeline_cache,
                    &pipeline,
                    M::Pipeline::key(view_key, pass, &material.key),
                    &render_mesh.layout,
                )
            };
            let colour = specialize(ScenePassKind::Colour);
            // A background material has no prepass variant at all: see
            // `SceneLayer::Background`.
            let prepass = (M::LAYER == SceneLayer::Opaque)
                .then(|| specialize(ScenePassKind::Prepass))
                .transpose();
            let (Ok(colour_pipeline), Ok(prepass_pipeline)) = (colour, prepass) else {
                // A mesh missing an attribute the pipeline binds (a toon mesh
                // with no normals, say). Bevy would have skipped it silently.
                error_once!(
                    "scene pipeline for {} failed to specialize for a mesh layout",
                    M::label()
                );
                continue;
            };

            let start = instances.0.len() as u32;
            let mut distance = 0.0f32;
            for (main_entity, source) in run {
                let mut instance = source.instance;
                instance.skin_index = skins.skin_index(*main_entity).unwrap_or(0);
                let origin = instance.world_from_local.w_axis.truncate();
                distance = distance.max(origin.distance(view_position));
                instances.0.push(instance);
            }
            draws.push(SceneDraw {
                mesh: head.mesh,
                material: material.bind_group.clone(),
                layer: M::LAYER,
                colour_pipeline,
                prepass_pipeline,
                instances: start..instances.0.len() as u32,
                distance,
            });
        }

        if !draws.is_empty() {
            view_draws.0.entry(view_entity).or_default().extend(draws);
        }
    }
}
