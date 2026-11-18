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
    core_pipeline::prepass::NormalPrepass,
    ecs::system::{SystemParamItem, lifetimeless::SRes},
    log::error_once,
    prelude::*,
    render::{
        Extract, ExtractSchedule, Render, RenderApp, RenderStartup, RenderSystems,
        material_bind_groups::FallbackBuffer,
        mesh::RenderMesh,
        render_asset::{
            PrepareAssetError, RenderAsset, RenderAssetPlugin, RenderAssets, prepare_assets,
        },
        render_resource::{
            AsBindGroup, AsBindGroupError, BindGroup, BindGroupLayoutDescriptor, PipelineCache,
            SpecializedMeshPipeline, SpecializedMeshPipelines,
        },
        renderer::RenderDevice,
        storage::GpuShaderBuffer,
        sync_world::MainEntity,
        texture::GpuImage,
        view::{ExtractedView, RenderVisibleEntities, ViewTarget},
    },
};

use bevy::camera::primitives::Aabb;
use bevy::platform::collections::HashMap;
use engine_api::OutlineWeight;

use crate::game::gfx::post_process::EdgeStyle;

use super::{
    SceneDraw, SceneDraws, SceneInstance, SceneInstances, SceneLayer, SceneQueue,
    pipeline::SceneViewKey,
};

/// A material type the scene pass draws: bevy's `Material` bound (for the
/// authoring component `MeshMaterial3d<M>`) plus a pipeline specializer whose
/// key is built from the view and the material's own [`AsBindGroup::Data`].
pub trait SceneMaterial: Material + AsBindGroup<Data: Clone + Hash + Eq + Send + Sync> {
    type Pipeline: ScenePipeline<Self>;
    /// Where in the frame one of this type's materials is drawn, and whether
    /// it writes depth and the normal target. A function of the material's
    /// key, not a per-type const, because `ToonMaterial` answers per material
    /// (`alpha_blend` → [`SceneLayer::Transparent`]); almost everything else
    /// is [`SceneLayer::Opaque`] for the whole type. The pipeline specializer
    /// must derive its blend/depth/mask state from the same key, so a draw's
    /// layer and its pipeline cannot disagree.
    fn layer(_key: &Self::Data) -> SceneLayer {
        SceneLayer::Opaque
    }
}

/// The specializer for one [`SceneMaterial`]: built once in `RenderStartup`
/// from the bind group layouts, then asked for a pipeline per
/// `(view, material key, mesh layout)` by [`queue_draws`].
pub trait ScenePipeline<M: AsBindGroup>:
    SpecializedMeshPipeline<Key: Send + Sync + 'static> + FromWorld + Resource
{
    fn key(view: SceneViewKey, material: &M::Data) -> Self::Key;
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
            .map_err(|err| match err {
                // A texture that hasn't finished uploading. This has to be
                // bevy's *retry* variant: `PrepareAssetError::AsBindGroupError`
                // is logged and the asset dropped for good, which is how every
                // textured glTF material whose image landed a frame late
                // (the cafe walls) rendered as nothing at all.
                AsBindGroupError::RetryNextUpdate => PrepareAssetError::RetryNextUpdate(material),
                other => PrepareAssetError::AsBindGroupError(other),
            })
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
///
/// Each instance also gets its **outline level** (2 bits of
/// `SceneInstance::flags`): an [`OutlineWeight`] on the mesh or the nearest
/// ancestor wins, else the automatic size rule ([`auto_outline_level`] over
/// the mesh's world-space AABB, thresholds from `EdgeStyle`). The toon
/// fragment writes it into the normal target's alpha for the edge detector.
/// The ancestor walk is a few `get`s per visible mesh per frame — the same
/// "retained maps are the upgrade if the Systems table ever says otherwise"
/// trade as [`SceneSources`] itself.
fn extract_instances<M: SceneMaterial>(
    mut sources: ResMut<SceneSources<M>>,
    entities: Extract<
        Query<(
            Entity,
            &ViewVisibility,
            &GlobalTransform,
            &Mesh3d,
            &MeshMaterial3d<M>,
            Option<&Aabb>,
        )>,
    >,
    weights: Extract<Query<&OutlineWeight>>,
    parents: Extract<Query<&ChildOf>>,
    edge_style: Extract<Option<Res<EdgeStyle>>>,
) {
    sources.0.clear();
    for (entity, visibility, transform, mesh, material, aabb) in &entities {
        if !visibility.get() {
            continue;
        }
        let level = outline_level(
            entity,
            &weights,
            &parents,
            aabb,
            transform,
            edge_style.as_deref(),
        );
        sources.0.insert(
            MainEntity::from(entity),
            SourceInstance {
                mesh: mesh.id(),
                material: material.id(),
                instance: SceneInstance::from_transform(transform).with_outline_level(level),
            },
        );
    }
}

/// A mesh's outline level: the nearest [`OutlineWeight`] on the entity or an
/// ancestor, else the automatic size rule. No AABB (not computed yet, or a
/// meshless oddity) reads as full — the conservative default is today's look.
fn outline_level(
    entity: Entity,
    weights: &Query<&OutlineWeight>,
    parents: &Query<&ChildOf>,
    aabb: Option<&Aabb>,
    transform: &GlobalTransform,
    edge_style: Option<&EdgeStyle>,
) -> u32 {
    let mut current = entity;
    // Bounded walk: hierarchies are shallow and this guards against cycles.
    for _ in 0..64 {
        if let Ok(weight) = weights.get(current) {
            return weight.level();
        }
        match parents.get(current) {
            Ok(child_of) => current = child_of.parent(),
            Err(_) => break,
        }
    }
    let (Some(aabb), Some(style)) = (aabb, edge_style) else {
        return SceneInstance::OUTLINE_LEVEL_FULL;
    };
    auto_outline_level(
        world_size(aabb, transform),
        style.auto_weight_small,
        style.auto_weight_large,
    )
}

/// The largest world-space dimension of `aabb` under `transform`, in metres.
fn world_size(aabb: &Aabb, transform: &GlobalTransform) -> f32 {
    let m = Mat3::from(transform.affine().matrix3);
    let he = Vec3::from(aabb.half_extents);
    // Standard AABB transform: each world half-extent is the sum of the
    // rotated/scaled axes' absolute contributions.
    let world_half = m.x_axis.abs() * he.x + m.y_axis.abs() * he.y + m.z_axis.abs() * he.z;
    2.0 * world_half.max_element()
}

/// The automatic rule: small scenery gets faint lines, middling things
/// reduced ones, anything at or past `large` keeps the full look. Level 0
/// (no outline) is never assigned automatically — that's [`OutlineWeight`]'s
/// job.
pub fn auto_outline_level(size: f32, small: f32, large: f32) -> u32 {
    if size < small {
        1
    } else if size < large {
        2
    } else {
        SceneInstance::OUTLINE_LEVEL_FULL
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
/// `Has<NormalPrepass>` is bevy's own marker on the render view
/// (`extract_camera_prepass_phase` copies it from the camera), and the same
/// condition `prepare_prepass_textures` creates the normal texture on — so a
/// pipeline specialized with a normal target always meets an attachment.
///
/// Skinning: an entity `skin::extract_skins` uploaded joints for (it runs in
/// the same `ExtractSchedule`) gets their offset in the joint buffer as its
/// `skin_index`, which the `SKINNED` vertex variant indexes bind group 0's
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
    skins: Res<super::skin::SceneSkins>,
    views: Query<
        (
            Entity,
            &ExtractedView,
            &RenderVisibleEntities,
            &Msaa,
            &ViewTarget,
            Has<NormalPrepass>,
        ),
        With<Camera3d>,
    >,
) {
    if sources.0.is_empty() {
        return;
    }

    for (view_entity, view, visible, msaa, target, normal_target) in &views {
        let view_position = view.world_from_view.translation();
        let Some(class) = visible.get::<Mesh3d>() else {
            continue;
        };
        let view_key = SceneViewKey {
            msaa_samples: msaa.samples(),
            target_format: target.main_texture_format(),
            normal_target,
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

            let Ok(pipeline_id) = pipelines.specialize(
                &pipeline_cache,
                &pipeline,
                M::Pipeline::key(view_key, &material.key),
                &render_mesh.layout,
            ) else {
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
                layer: M::layer(&material.key),
                pipeline: pipeline_id,
                instances: start..instances.0.len() as u32,
                distance,
            });
        }

        if !draws.is_empty() {
            view_draws.0.entry(view_entity).or_default().extend(draws);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::ecs::system::SystemState;

    /// The automatic rule: faint under `small`, reduced under `large`, full
    /// past it. Level 0 is reserved for the explicit override.
    #[test]
    fn auto_level_follows_the_size_thresholds() {
        assert_eq!(auto_outline_level(0.3, 0.6, 1.5), 1);
        assert_eq!(auto_outline_level(1.0, 0.6, 1.5), 2);
        assert_eq!(auto_outline_level(1.5, 0.6, 1.5), 3);
        assert_eq!(auto_outline_level(12.0, 0.6, 1.5), 3);
    }

    /// `world_size` measures the mesh in world space: the instance's scale
    /// (a grass tuft blown up to a hedge) changes its level.
    #[test]
    fn world_size_scales_with_the_transform() {
        let aabb = Aabb {
            center: Vec3A::ZERO.into(),
            half_extents: bevy::math::Vec3A::new(0.2, 0.15, 0.2),
        };
        let unit = GlobalTransform::IDENTITY;
        assert!((world_size(&aabb, &unit) - 0.4).abs() < 1e-5);
        let scaled = GlobalTransform::from(Transform::from_scale(Vec3::splat(5.0)));
        assert!((world_size(&aabb, &scaled) - 2.0).abs() < 1e-4);
    }

    /// An [`OutlineWeight`] on the mesh or the nearest ancestor beats the
    /// size rule; without one, the rule decides; without an AABB, full.
    #[test]
    fn override_on_an_ancestor_wins_over_the_size_rule() {
        let mut world = World::new();
        let root = world.spawn(OutlineWeight::None).id();
        let mesh = world.spawn_empty().id();
        world.entity_mut(root).add_child(mesh);
        // Nearer override wins over a further one.
        let marked_mesh = world.spawn(OutlineWeight::Reduced).id();
        world.entity_mut(root).add_child(marked_mesh);
        let plain_mesh = world.spawn_empty().id();

        let mut state: SystemState<(Query<&OutlineWeight>, Query<&ChildOf>)> =
            SystemState::new(&mut world);
        let (weights, parents) = state.get(&world).unwrap();

        let tuft = Aabb {
            center: bevy::math::Vec3A::ZERO.into(),
            half_extents: bevy::math::Vec3A::splat(0.15),
        };
        let style = EdgeStyle::default();
        let transform = GlobalTransform::IDENTITY;

        let level = |e, aabb: Option<&Aabb>| {
            outline_level(e, &weights, &parents, aabb, &transform, Some(&style))
        };
        assert_eq!(level(mesh, Some(&tuft)), 0, "root override inherited");
        assert_eq!(level(marked_mesh, Some(&tuft)), 2, "own override wins");
        assert_eq!(level(plain_mesh, Some(&tuft)), 1, "size rule: faint");
        assert_eq!(level(plain_mesh, None), 3, "no AABB reads as full");
    }
}
