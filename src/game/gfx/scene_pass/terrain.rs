//! Terrain's half of the scene pass: extraction, the material bind group, and
//! the per-view draw list.
//!
//! `MaterialPlugin::<TerrainMaterial>` used to do all three. What it also did,
//! and what this does *not*, is the part the plan is after: a per-type
//! specialization check and queue system every frame, binned-phase
//! construction, batching, a GPU preprocessing dispatch, and the `mesh_view`
//! bind group with its lights, clusters, shadow maps, fog and SSAO bindings —
//! none of which a fully baked renderer reads.
//!
//! The material bind group is still built by bevy's own [`AsBindGroup`] derive
//! on `TerrainMaterial`, through a [`RenderAsset`] impl. That keeps every
//! existing path that mutates a terrain material working unchanged — a bake
//! landing a lightmap, the terrain bootstrap, Shift+L flipping `use_lightmap`,
//! the inspector editing the environment — because `RenderAssetPlugin`
//! re-prepares on the same `AssetEvent`s `MaterialPlugin` listened to.

use bevy::{
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
        render_resource::{AsBindGroup, BindGroup, PipelineCache, SpecializedMeshPipelines},
        renderer::RenderDevice,
        storage::GpuShaderBuffer,
        sync_world::MainEntity,
        texture::GpuImage,
        view::{RenderVisibleEntities, ViewTarget},
    },
};

use crate::game::world::terrain::material::TerrainMaterial;

use super::{
    SceneDraw, SceneDraws, SceneInstance, SceneInstances, SceneQueue, SceneSources,
    pipeline::{
        ScenePassKind, ScenePipelineKey, TerrainScenePipeline, init_terrain_scene_pipeline,
    },
};

/// What extraction keeps about one visible terrain chunk.
pub struct TerrainSource {
    pub mesh: AssetId<Mesh>,
    pub material: AssetId<TerrainMaterial>,
    pub instance: SceneInstance,
}

pub struct TerrainScenePlugin;

impl Plugin for TerrainScenePlugin {
    fn build(&self, app: &mut App) {
        // `GpuImage` as the AFTER parameter: the bind group binds the chunk's
        // lightmap and the stone texture, so those have to be on the GPU
        // before `prepare_asset` runs or every chunk would spend a frame
        // retrying.
        app.add_plugins(RenderAssetPlugin::<PreparedTerrainMaterial, GpuImage>::default());

        let Some(render_app) = app.get_sub_app_mut(RenderApp) else {
            return;
        };

        render_app
            .init_resource::<SpecializedMeshPipelines<TerrainScenePipeline>>()
            .add_systems(RenderStartup, init_terrain_scene_pipeline)
            .add_systems(ExtractSchedule, extract_terrain_instances)
            .add_systems(
                Render,
                queue_terrain_draws
                    .in_set(RenderSystems::Queue)
                    .in_set(SceneQueue)
                    // The vertex layout a pipeline is specialized against
                    // comes from the prepared mesh.
                    .after(prepare_assets::<RenderMesh>),
            );
    }
}

// ---------------------------------------------------------------------------
// The material bind group
// ---------------------------------------------------------------------------

/// `TerrainMaterial` as bind group 1.
pub struct PreparedTerrainMaterial {
    pub bind_group: BindGroup,
}

impl RenderAsset for PreparedTerrainMaterial {
    type SourceAsset = TerrainMaterial;

    /// The last element is `AsBindGroup`'s own parameter tuple, nested so it
    /// can be handed to [`AsBindGroup::as_bind_group`] whole. `TerrainScenePipeline`
    /// supplies the layout, so the bind group and the pipeline that binds it
    /// are built from literally the same descriptor.
    type Param = (
        SRes<RenderDevice>,
        SRes<PipelineCache>,
        SRes<FallbackBuffer>,
        SRes<RenderAssets<GpuShaderBuffer>>,
        SRes<TerrainScenePipeline>,
        <TerrainMaterial as AsBindGroup>::Param,
    );

    fn prepare_asset(
        source_asset: Self::SourceAsset,
        _asset_id: AssetId<Self::SourceAsset>,
        (device, pipeline_cache, fallback_buffer, shader_buffers, pipeline, as_bind_group): &mut SystemParamItem<Self::Param>,
        _previous: Option<&Self>,
    ) -> Result<Self, PrepareAssetError<Self::SourceAsset>> {
        source_asset
            .as_bind_group(
                &pipeline.material_layout,
                device,
                pipeline_cache,
                fallback_buffer,
                shader_buffers,
                as_bind_group,
            )
            .map(|prepared| Self {
                bind_group: prepared.bind_group,
            })
            // A texture that hasn't finished uploading: try again next frame.
            .map_err(PrepareAssetError::AsBindGroupError)
    }
}

// ---------------------------------------------------------------------------
// Extract
// ---------------------------------------------------------------------------

/// Every visible terrain chunk, by main-world entity.
///
/// `ViewVisibility` is the cheap pre-filter (hierarchy visibility and whether
/// *any* view can see it); the per-view frustum and render-layer decision is
/// bevy's `RenderVisibleEntities`, read in [`queue_terrain_draws`].
fn extract_terrain_instances(
    mut sources: ResMut<SceneSources>,
    chunks: Extract<
        Query<(
            Entity,
            &ViewVisibility,
            &GlobalTransform,
            &Mesh3d,
            &MeshMaterial3d<TerrainMaterial>,
        )>,
    >,
) {
    sources.terrain.clear();
    for (entity, visibility, transform, mesh, material) in &chunks {
        if !visibility.get() {
            continue;
        }
        sources.terrain.insert(
            MainEntity::from(entity),
            TerrainSource {
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

/// Build each 3D view's terrain draw list and append its instances.
///
/// Draws are grouped by `(material, mesh)`. Every chunk owns its lightmap and
/// therefore its material, so in practice each group is one instance and the
/// draw count is the loaded chunk count — the grouping exists for Phase 3,
/// where probe-lit furniture shares a material and must keep batching.
fn queue_terrain_draws(
    sources: Res<SceneSources>,
    mut view_draws: ResMut<SceneDraws>,
    mut instances: ResMut<SceneInstances>,
    mut pipelines: ResMut<SpecializedMeshPipelines<TerrainScenePipeline>>,
    pipeline: Res<TerrainScenePipeline>,
    pipeline_cache: Res<PipelineCache>,
    meshes: Res<RenderAssets<RenderMesh>>,
    materials: Res<RenderAssets<PreparedTerrainMaterial>>,
    views: Query<(Entity, &RenderVisibleEntities, &Msaa, &ViewTarget), With<Camera3d>>,
) {
    if sources.terrain.is_empty() {
        return;
    }

    for (view_entity, visible, msaa, target) in &views {
        let Some(class) = visible.get::<Mesh3d>() else {
            continue;
        };

        // Sort so equal (material, mesh) pairs are adjacent; the run-length
        // grouping below is what turns them into one instanced draw.
        let mut visible_terrain: Vec<&TerrainSource> = class
            .entities_cpu_culling
            .iter()
            .filter_map(|(_, main_entity)| sources.terrain.get(main_entity))
            .filter(|source| materials.get(source.material).is_some())
            .collect();
        visible_terrain.sort_unstable_by_key(|source| (source.material, source.mesh));

        let mut draws = Vec::new();
        let mut index = 0;
        while index < visible_terrain.len() {
            let head = visible_terrain[index];
            let end = visible_terrain[index..]
                .iter()
                .position(|s| (s.material, s.mesh) != (head.material, head.mesh))
                .map_or(visible_terrain.len(), |offset| index + offset);

            // A mesh still uploading simply doesn't draw this frame.
            let Some(render_mesh) = meshes.get(head.mesh) else {
                index = end;
                continue;
            };

            let key = |pass| ScenePipelineKey {
                pass,
                msaa_samples: msaa.samples(),
                target_format: target.main_texture_format(),
            };
            let colour = pipelines.specialize(
                &pipeline_cache,
                &pipeline,
                key(ScenePassKind::Colour),
                &render_mesh.layout,
            );
            let prepass = pipelines.specialize(
                &pipeline_cache,
                &pipeline,
                key(ScenePassKind::Prepass),
                &render_mesh.layout,
            );
            let (Ok(colour_pipeline), Ok(prepass_pipeline)) = (colour, prepass) else {
                // Only reachable if a chunk mesh loses an attribute the
                // pipeline binds, which would be a bug in terrain/mesh.rs.
                error_once!("terrain scene pipeline failed to specialize");
                index = end;
                continue;
            };

            let start = instances.0.len() as u32;
            for source in &visible_terrain[index..end] {
                instances.0.push(source.instance);
            }
            draws.push(SceneDraw {
                mesh: head.mesh,
                material: head.material,
                colour_pipeline,
                prepass_pipeline,
                instances: start..instances.0.len() as u32,
            });
            index = end;
        }

        if !draws.is_empty() {
            view_draws.0.entry(view_entity).or_default().extend(draws);
        }
    }
}
