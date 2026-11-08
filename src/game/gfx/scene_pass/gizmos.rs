//! 3D line gizmos, drawn by us.
//!
//! Bevy's `GizmoRenderPlugin` installs its 3D line pipeline only when
//! `PbrPlugin` is in the app, and builds it on `MeshPipeline`'s view bind
//! group layout; everything that pipeline uses inside `bevy_gizmos_render` is
//! private. So with `PbrPlugin` gone (`docs/SCENE_PASS_PLAN.md`, Phase 5) the
//! editor's transform gizmo, selection boxes, light markers and avian's
//! collider view would simply vanish. This is the replacement, over the
//! *main-world* half of bevy_gizmos, which is public and unchanged: the
//! `Gizmos` system param still fills a `GizmoAsset` per config group each
//! frame; we extract those, upload them, and draw them with bevy's own line
//! shader (`shaders/gizmos/lines.wesl`, copied verbatim) against *our* bind
//! group 0.
//!
//! Supported: solid, dotted and dashed lines, list and strip topology,
//! perspective width, depth bias, render layers — everything the editor and
//! avian configure. Not supported: line joints (`GizmoLineJoint`, warned
//! once if a group asks) and retained `Gizmo` components (nothing here has
//! one). The draw runs after the transparent pass, as bevy's did, with
//! bevy's depth settings (`Greater`, writing) so a gizmo occludes later
//! gizmos the way it always has.

use bevy::{
    camera::visibility::RenderLayers,
    core_pipeline::{
        Core3d, Core3dSystems,
        core_3d::{CORE_3D_DEPTH_FORMAT, main_transparent_pass_3d},
    },
    ecs::{entity::EntityHashMap, error::BevyError, system::lifetimeless::SRes},
    gizmos::{
        GizmoAsset, GizmoHandles,
        config::{GizmoConfigStore, GizmoLineJoint, GizmoLineStyle},
    },
    log::warn_once,
    mesh::VertexBufferLayout,
    prelude::*,
    render::{
        Extract, ExtractSchedule, Render, RenderApp, RenderStartup, RenderSystems,
        camera::ExtractedCamera,
        diagnostic::RecordDiagnostics,
        extract_component::{ComponentUniforms, DynamicUniformIndex, UniformComponentPlugin},
        render_asset::{PrepareAssetError, RenderAsset, RenderAssetPlugin, RenderAssets},
        render_resource::{
            BindGroup, BindGroupEntries, BindGroupLayoutDescriptor, BindGroupLayoutEntries,
            BlendState, Buffer, BufferInitDescriptor, BufferUsages, CachedRenderPipelineId,
            Canonical, ColorTargetState, ColorWrites, CompareFunction, DepthBiasState,
            DepthStencilState, FragmentState, PipelineCache, RenderPassDescriptor, RenderPipeline,
            RenderPipelineDescriptor, ShaderStages, ShaderType, Specializer, SpecializerKey,
            StencilState, StoreOp, Variants, VertexAttribute, VertexFormat, VertexState,
            VertexStepMode, binding_types::uniform_buffer,
        },
        renderer::{RenderContext, RenderDevice, ViewQuery},
        sync_world::TemporaryRenderEntity,
        view::{ViewDepthStencilTexture, ViewTarget, ViewUniformOffset},
    },
};

use super::{SceneBindGroup, SceneBindGroupLayout, pipeline::SceneViewKey};

pub struct GizmoScenePlugin;

impl Plugin for GizmoScenePlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins((
            RenderAssetPlugin::<GpuLineGizmo>::default(),
            UniformComponentPlugin::<LineGizmoUniform>::default(),
        ));
        let Some(render_app) = app.get_sub_app_mut(RenderApp) else {
            return;
        };
        render_app
            .init_resource::<GizmoDraws>()
            .add_systems(RenderStartup, init_gizmo_pipeline)
            .add_systems(ExtractSchedule, extract_gizmos)
            .add_systems(
                Render,
                (prepare_gizmo_bind_group, prepare_gizmo_draws)
                    .in_set(RenderSystems::PrepareBindGroups),
            )
            .add_systems(
                Core3d,
                gizmo_pass
                    .in_set(Core3dSystems::MainPass)
                    .after(main_transparent_pass_3d),
            );
    }
}

// ---------------------------------------------------------------------------
// Extract
// ---------------------------------------------------------------------------

/// Mirrors `LineGizmoUniform` in `shaders/gizmos/lines.wesl` field for field.
#[derive(Component, ShaderType, Clone, Copy)]
struct LineGizmoUniform {
    /// `mat3x4` as three rows — bevy's `Affine3::to_transpose` layout. The
    /// immediate-mode gizmos are in world space already, so this is the
    /// identity.
    world_from_local: [Vec4; 3],
    line_width: f32,
    depth_bias: f32,
    joints_resolution: u32,
    gap_scale: f32,
    line_scale: f32,
    animation_offset: f32,
}

/// One config group's lines this frame, on a temporary render entity.
#[derive(Component)]
struct GizmoBatch {
    asset: AssetId<GizmoAsset>,
    perspective: bool,
    style: GizmoLineStyle,
    layers: RenderLayers,
}

/// What bevy's `extract_gizmo_data` does, minus joints.
fn extract_gizmos(
    mut commands: Commands,
    handles: Extract<Res<GizmoHandles>>,
    config: Extract<Res<GizmoConfigStore>>,
) {
    for (group, handle) in handles.handles() {
        let Some((config, _)) = config.get_config_dyn(group) else {
            continue;
        };
        if !config.enabled {
            continue;
        }
        let Some(handle) = handle else {
            continue;
        };
        if config.line.joints != GizmoLineJoint::None {
            warn_once!(
                "a gizmo group asks for line joints, which the scene pass's gizmo renderer \
                 doesn't draw; lines render unjoined"
            );
        }
        let (gap_scale, line_scale) = match config.line.style {
            GizmoLineStyle::Dashed {
                gap_scale,
                line_scale,
            } => (gap_scale, line_scale),
            _ => (1.0, 1.0),
        };
        commands.spawn((
            LineGizmoUniform {
                world_from_local: [Vec4::X, Vec4::Y, Vec4::Z],
                line_width: config.line.width,
                depth_bias: config.depth_bias,
                joints_resolution: 0,
                gap_scale,
                line_scale,
                animation_offset: config.line.animation_offset,
            },
            GizmoBatch {
                asset: handle.id(),
                perspective: config.line.perspective,
                style: config.line.style,
                layers: config.render_layers.clone(),
            },
            TemporaryRenderEntity::default(),
        ));
    }
}

// ---------------------------------------------------------------------------
// GPU asset
// ---------------------------------------------------------------------------

/// A `GizmoAsset`'s four vertex buffers. Positions and colours are separate
/// buffers, instanced per segment (list) or per vertex pair (strip, where the
/// same buffer is bound twice, one element apart).
pub struct GpuLineGizmo {
    list_positions: Buffer,
    list_colors: Buffer,
    list_vertex_count: u32,
    strip_positions: Buffer,
    strip_colors: Buffer,
    strip_vertex_count: u32,
}

impl RenderAsset for GpuLineGizmo {
    type SourceAsset = GizmoAsset;
    type Param = SRes<RenderDevice>;

    fn prepare_asset(
        gizmo: Self::SourceAsset,
        _: AssetId<Self::SourceAsset>,
        device: &mut bevy::ecs::system::SystemParamItem<Self::Param>,
        _: Option<&Self>,
    ) -> Result<Self, PrepareAssetError<Self::SourceAsset>> {
        let buffer = gizmo.buffer();
        let upload = |label, contents: &[u8]| {
            device.create_buffer_with_data(&BufferInitDescriptor {
                label: Some(label),
                contents,
                usage: BufferUsages::VERTEX,
            })
        };
        Ok(Self {
            list_positions: upload(
                "gizmo list positions",
                bytemuck::cast_slice(&buffer.list_positions),
            ),
            list_colors: upload(
                "gizmo list colors",
                bytemuck::cast_slice(&buffer.list_colors),
            ),
            list_vertex_count: buffer.list_positions.len() as u32,
            strip_positions: upload(
                "gizmo strip positions",
                bytemuck::cast_slice(&buffer.strip_positions),
            ),
            strip_colors: upload(
                "gizmo strip colors",
                bytemuck::cast_slice(&buffer.strip_colors),
            ),
            strip_vertex_count: buffer.strip_positions.len() as u32,
        })
    }
}

// ---------------------------------------------------------------------------
// Pipeline
// ---------------------------------------------------------------------------

#[derive(Resource)]
struct GizmoPipeline {
    uniform_layout: BindGroupLayoutDescriptor,
    variants: Variants<RenderPipeline, GizmoSpecializer>,
}

struct GizmoSpecializer;

#[derive(PartialEq, Eq, Hash, Clone, SpecializerKey)]
struct GizmoPipelineKey {
    view: SceneViewKey,
    strip: bool,
    perspective: bool,
    style: GizmoLineStyle,
}

fn init_gizmo_pipeline(
    mut commands: Commands,
    asset_server: Res<AssetServer>,
    scene_layout: Res<SceneBindGroupLayout>,
) {
    let uniform_layout = BindGroupLayoutDescriptor::new(
        "gizmo_uniform_layout",
        &BindGroupLayoutEntries::single(
            ShaderStages::VERTEX_FRAGMENT,
            uniform_buffer::<LineGizmoUniform>(true),
        ),
    );
    let shader = asset_server.load("shaders/gizmos/lines.wesl");
    let descriptor = RenderPipelineDescriptor {
        label: Some("gizmo_lines_pipeline".into()),
        // Group 0 is the scene pass's; the shader declares only its `view`
        // binding, which a layout with more entries is allowed to satisfy.
        layout: vec![scene_layout.0.clone(), uniform_layout.clone()],
        vertex: VertexState {
            shader: shader.clone(),
            ..default()
        },
        fragment: Some(FragmentState {
            shader,
            ..default()
        }),
        depth_stencil: Some(DepthStencilState {
            format: CORE_3D_DEPTH_FORMAT,
            depth_write_enabled: Some(true),
            depth_compare: Some(CompareFunction::Greater),
            stencil: StencilState::default(),
            bias: DepthBiasState::default(),
        }),
        ..default()
    };
    commands.insert_resource(GizmoPipeline {
        uniform_layout,
        variants: Variants::new(GizmoSpecializer, descriptor),
    });
}

impl Specializer<RenderPipeline> for GizmoSpecializer {
    type Key = GizmoPipelineKey;

    fn specialize(
        &self,
        key: Self::Key,
        descriptor: &mut RenderPipelineDescriptor,
    ) -> Result<Canonical<Self::Key>, BevyError> {
        descriptor.vertex.buffers = line_vertex_buffer_layouts(key.strip);
        descriptor.multisample.count = key.view.msaa_samples;
        let fragment = descriptor.fragment_mut()?;
        if key.perspective {
            fragment.shader_defs.push("PERSPECTIVE".into());
        }
        fragment.entry_point = Some(
            match key.style {
                GizmoLineStyle::Solid => "fragment_solid",
                GizmoLineStyle::Dotted => "fragment_dotted",
                GizmoLineStyle::Dashed { .. } => "fragment_dashed",
                // `#[non_exhaustive]`: a style bevy adds later draws solid
                // rather than failing pipeline creation.
                _ => "fragment_solid",
            }
            .into(),
        );
        fragment.set_target(
            0,
            ColorTargetState {
                format: key.view.target_format,
                blend: Some(BlendState::ALPHA_BLENDING),
                write_mask: ColorWrites::ALL,
            },
        );
        Ok(key)
    }
}

/// bevy's `line_gizmo_vertex_buffer_layouts`: a line list is one instance
/// per segment with both endpoints in one stride; a strip is the same buffer
/// bound twice, offset by one vertex, so instance `i` sees vertices `i` and
/// `i + 1`.
fn line_vertex_buffer_layouts(strip: bool) -> Vec<VertexBufferLayout> {
    use VertexFormat::{Float32x3, Float32x4};
    let position = VertexBufferLayout {
        array_stride: Float32x3.size(),
        step_mode: VertexStepMode::Instance,
        attributes: vec![VertexAttribute {
            format: Float32x3,
            offset: 0,
            shader_location: 0,
        }],
    };
    let color = VertexBufferLayout {
        array_stride: Float32x4.size(),
        step_mode: VertexStepMode::Instance,
        attributes: vec![VertexAttribute {
            format: Float32x4,
            offset: 0,
            shader_location: 2,
        }],
    };
    if strip {
        let mut position_b = position.clone();
        position_b.attributes[0].shader_location = 1;
        let mut color_b = color.clone();
        color_b.attributes[0].shader_location = 3;
        vec![position, position_b, color, color_b]
    } else {
        let mut position = position;
        position.array_stride *= 2;
        position.attributes.push(VertexAttribute {
            format: Float32x3,
            offset: Float32x3.size(),
            shader_location: 1,
        });
        let mut color = color;
        color.array_stride *= 2;
        color.attributes.push(VertexAttribute {
            format: Float32x4,
            offset: Float32x4.size(),
            shader_location: 3,
        });
        vec![position, color]
    }
}

// ---------------------------------------------------------------------------
// Prepare
// ---------------------------------------------------------------------------

#[derive(Resource)]
struct GizmoBindGroup(BindGroup);

fn prepare_gizmo_bind_group(
    mut commands: Commands,
    device: Res<RenderDevice>,
    pipeline_cache: Res<PipelineCache>,
    pipeline: Res<GizmoPipeline>,
    uniforms: Res<ComponentUniforms<LineGizmoUniform>>,
) {
    let Some(binding) = uniforms.uniforms().binding() else {
        commands.remove_resource::<GizmoBindGroup>();
        return;
    };
    commands.insert_resource(GizmoBindGroup(device.create_bind_group(
        "gizmo_uniform_bind_group",
        &pipeline_cache.get_bind_group_layout(&pipeline.uniform_layout),
        &BindGroupEntries::single(binding),
    )));
}

/// One group's lines as a view will draw them.
struct GizmoDraw {
    asset: AssetId<GizmoAsset>,
    uniform_index: u32,
    list: Option<CachedRenderPipelineId>,
    strip: Option<CachedRenderPipelineId>,
}

/// Per render-world view entity, rebuilt each frame.
#[derive(Resource, Default)]
struct GizmoDraws(EntityHashMap<Vec<GizmoDraw>>);

fn prepare_gizmo_draws(
    mut draws: ResMut<GizmoDraws>,
    mut pipeline: ResMut<GizmoPipeline>,
    pipeline_cache: Res<PipelineCache>,
    assets: Res<RenderAssets<GpuLineGizmo>>,
    batches: Query<(&GizmoBatch, &DynamicUniformIndex<LineGizmoUniform>)>,
    views: Query<(Entity, &Msaa, &ViewTarget, Option<&RenderLayers>), With<Camera3d>>,
) -> Result<(), BevyError> {
    draws.0.clear();
    for (view_entity, msaa, target, view_layers) in &views {
        let view = SceneViewKey {
            msaa_samples: msaa.samples(),
            target_format: target.main_texture_format(),
            // The gizmo pass binds the colour target alone (`gizmo_pass`);
            // lines never write the edge detector's normals.
            normal_target: false,
        };
        let view_layers = view_layers.cloned().unwrap_or_default();
        let mut list_for_view = Vec::new();
        for (batch, uniform_index) in &batches {
            if !batch.layers.intersects(&view_layers) {
                continue;
            }
            let Some(gpu) = assets.get(batch.asset) else {
                continue;
            };
            let mut specialize = |strip| {
                pipeline.variants.specialize(
                    &pipeline_cache,
                    GizmoPipelineKey {
                        view,
                        strip,
                        perspective: batch.perspective,
                        style: batch.style,
                    },
                )
            };
            let list = (gpu.list_vertex_count >= 2)
                .then(|| specialize(false))
                .transpose()?;
            let strip = (gpu.strip_vertex_count >= 2)
                .then(|| specialize(true))
                .transpose()?;
            if list.is_some() || strip.is_some() {
                list_for_view.push(GizmoDraw {
                    asset: batch.asset,
                    uniform_index: uniform_index.index(),
                    list,
                    strip,
                });
            }
        }
        if !list_for_view.is_empty() {
            draws.0.insert(view_entity, list_for_view);
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Draw
// ---------------------------------------------------------------------------

/// After the transparent pass, like bevy's gizmo items were, over the
/// finished image and against the finished depth.
fn gizmo_pass(
    view: ViewQuery<(
        &ViewTarget,
        &ViewDepthStencilTexture,
        &ViewUniformOffset,
        &ExtractedCamera,
    )>,
    draws: Res<GizmoDraws>,
    scene_bind_group: Option<Res<SceneBindGroup>>,
    gizmo_bind_group: Option<Res<GizmoBindGroup>>,
    pipeline_cache: Res<PipelineCache>,
    assets: Res<RenderAssets<GpuLineGizmo>>,
    mut ctx: RenderContext,
) {
    let view_entity = view.entity();
    let (target, depth, view_offset, camera) = view.into_inner();
    let (Some(scene_bind_group), Some(gizmo_bind_group), Some(draws)) = (
        scene_bind_group,
        gizmo_bind_group,
        draws.0.get(&view_entity),
    ) else {
        return;
    };

    let color_attachments = [Some(target.get_color_attachment())];
    let diagnostics = ctx.diagnostic_recorder();
    let diagnostics = diagnostics.as_deref();
    let mut pass = ctx.begin_tracked_render_pass(RenderPassDescriptor {
        label: Some("scene_gizmos"),
        color_attachments: &color_attachments,
        depth_stencil_attachment: Some(depth.get_attachment(StoreOp::Store)),
        timestamp_writes: None,
        occlusion_query_set: None,
        multiview_mask: None,
    });
    let span = diagnostics.pass_span(&mut pass, "scene gizmos");
    if let Some(viewport) = camera.viewport.as_ref() {
        pass.set_camera_viewport(viewport);
    }
    pass.set_bind_group(0, &scene_bind_group.0, &[view_offset.offset]);

    for draw in draws {
        let Some(gpu) = assets.get(draw.asset) else {
            continue;
        };
        pass.set_bind_group(1, &gizmo_bind_group.0, &[draw.uniform_index]);

        if let Some(pipeline) = draw
            .list
            .and_then(|id| pipeline_cache.get_render_pipeline(id))
        {
            pass.set_render_pipeline(pipeline);
            pass.set_vertex_buffer(0, gpu.list_positions.slice(..));
            pass.set_vertex_buffer(1, gpu.list_colors.slice(..));
            pass.draw(0..6, 0..gpu.list_vertex_count / 2);
        }
        if let Some(pipeline) = draw
            .strip
            .and_then(|id| pipeline_cache.get_render_pipeline(id))
        {
            pass.set_render_pipeline(pipeline);
            let position = VertexFormat::Float32x3.size();
            let color = VertexFormat::Float32x4.size();
            let positions_len = gpu.strip_positions.size() - position;
            let colors_len = gpu.strip_colors.size() - color;
            pass.set_vertex_buffer(0, gpu.strip_positions.slice(..positions_len));
            pass.set_vertex_buffer(1, gpu.strip_positions.slice(position..));
            pass.set_vertex_buffer(2, gpu.strip_colors.slice(..colors_len));
            pass.set_vertex_buffer(3, gpu.strip_colors.slice(color..));
            pass.draw(0..6, 0..gpu.strip_vertex_count - 1);
        }
    }
    span.end(&mut pass);
}
