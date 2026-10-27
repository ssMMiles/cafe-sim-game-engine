//! Edge-detection post-process pass.
//!
//! Bevy 0.19 deleted the render graph: passes are plain systems in the
//! [`Core3d`] schedule, ordered against the *actual* pass functions rather
//! than `Node3d` labels. This one runs in [`Core3dSystems::PostProcess`]
//! `.after(tonemapping)`, which is where the old graph edge
//! `Tonemapping -> EdgeDetection -> EndMainPassPostProcessing` put it.
//!
//! The pipeline is **specialized on the view's target format** rather than
//! assuming `Rgba16Float`. `TextureFormat::bevy_default()` and
//! `ViewTarget::is_hdr` are gone in 0.19; `ExtractedView::target_format` is
//! the supported source. Specializing on it means the pass keeps working if
//! the camera ever loses `Hdr` or renders to a differently-formatted target,
//! instead of failing at pipeline creation with no compile-time signal.
//!
//! Structure mirrors `bevy_core_pipeline::fullscreen_material`, which is the
//! engine's own reference implementation of a fullscreen post-process effect.
//! We can't use `FullscreenMaterial` directly because it only binds the screen
//! texture, a sampler and the settings uniform — we also need the depth and
//! normal prepass textures.

use bevy::{
    core_pipeline::{
        Core3d, Core3dSystems, FullscreenShader, prepass::ViewPrepassTextures,
        tonemapping::tonemapping,
    },
    ecs::error::BevyError,
    prelude::*,
    render::{
        Render, RenderApp, RenderStartup, RenderSystems,
        camera::ExtractedCamera,
        extract_component::{
            ComponentUniforms, DynamicUniformIndex, ExtractComponent, ExtractComponentPlugin,
            UniformComponentPlugin,
        },
        render_resource::{
            binding_types::{
                sampler, texture_2d, texture_2d_multisampled, texture_depth_2d_multisampled,
                uniform_buffer,
            },
            *,
        },
        renderer::{RenderContext, RenderDevice, ViewQuery},
        view::{ExtractedView, ViewTarget},
    },
};

const SHADER_ASSET_PATH: &str = "shaders/edge_detection_post.wgsl";

pub struct EdgeDetectionPlugin;

impl Plugin for EdgeDetectionPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins((
            ExtractComponentPlugin::<EdgeDetectionSettings>::default(),
            UniformComponentPlugin::<EdgeDetectionSettings>::default(),
        ));

        let Some(render_app) = app.get_sub_app_mut(RenderApp) else {
            return;
        };

        render_app
            .add_systems(RenderStartup, init_edge_detection_pipeline)
            .add_systems(
                Render,
                prepare_edge_detection_pipelines.in_set(RenderSystems::Prepare),
            )
            .add_systems(
                Core3d,
                edge_detection_pass
                    .in_set(Core3dSystems::PostProcess)
                    .after(tonemapping),
            );
    }
}

// ---------------------------------------------------------------------------
// Pipeline + specialization
// ---------------------------------------------------------------------------

#[derive(Resource)]
struct EdgeDetectionPipeline {
    layout: BindGroupLayoutDescriptor,
    sampler: Sampler,
    variants: Variants<RenderPipeline, EdgeDetectionSpecializer>,
}

struct EdgeDetectionSpecializer;

/// The only thing that varies between views: the format we render into.
#[derive(PartialEq, Eq, Hash, Clone, Copy, SpecializerKey)]
struct EdgeDetectionKey {
    target_format: TextureFormat,
}

impl Specializer<RenderPipeline> for EdgeDetectionSpecializer {
    type Key = EdgeDetectionKey;

    fn specialize(
        &self,
        key: Self::Key,
        descriptor: &mut RenderPipelineDescriptor,
    ) -> Result<Canonical<Self::Key>, BevyError> {
        descriptor.fragment_mut()?.set_target(
            0,
            ColorTargetState {
                format: key.target_format,
                blend: None,
                write_mask: ColorWrites::ALL,
            },
        );
        Ok(key)
    }
}

/// The pipeline specialized for one view, written by
/// [`prepare_edge_detection_pipelines`].
#[derive(Component)]
struct EdgeDetectionPipelineId(CachedRenderPipelineId);

fn init_edge_detection_pipeline(
    mut commands: Commands,
    render_device: Res<RenderDevice>,
    asset_server: Res<AssetServer>,
    fullscreen_shader: Res<FullscreenShader>,
) {
    let layout_entries = BindGroupLayoutEntries::sequential(
        ShaderStages::FRAGMENT,
        (
            texture_2d(TextureSampleType::Float { filterable: true }), // Screen texture
            texture_depth_2d_multisampled(), // Depth prepass (multisampled)
            texture_2d_multisampled(TextureSampleType::Float { filterable: false }), // Normal prepass (multisampled)
            sampler(SamplerBindingType::Filtering),
            uniform_buffer::<EdgeDetectionSettings>(true),
        ),
    );

    let layout =
        BindGroupLayoutDescriptor::new("edge_detection_bind_group_layout", &layout_entries);

    let sampler = render_device.create_sampler(&SamplerDescriptor::default());

    // The target format here is a placeholder: `EdgeDetectionSpecializer`
    // overwrites it per view from `ExtractedView::target_format`.
    let descriptor = RenderPipelineDescriptor {
        label: Some("edge_detection_pipeline".into()),
        layout: vec![layout.clone()],
        vertex: fullscreen_shader.to_vertex_state(),
        fragment: Some(FragmentState {
            shader: asset_server.load(SHADER_ASSET_PATH),
            targets: vec![Some(ColorTargetState {
                format: TextureFormat::Rgba8UnormSrgb,
                blend: None,
                write_mask: ColorWrites::ALL,
            })],
            ..default()
        }),
        ..default()
    };

    commands.insert_resource(EdgeDetectionPipeline {
        layout,
        sampler,
        variants: Variants::new(EdgeDetectionSpecializer, descriptor),
    });
}

/// Specialize the pipeline for each view's target format once per frame.
fn prepare_edge_detection_pipelines(
    mut commands: Commands,
    pipeline_cache: Res<PipelineCache>,
    mut pipeline: ResMut<EdgeDetectionPipeline>,
    views: Query<(Entity, &ExtractedView, Has<EdgeDetectionSettings>), With<ExtractedCamera>>,
) -> Result<(), BevyError> {
    for (entity, view, has_settings) in &views {
        if !has_settings {
            // The settings component can be removed at runtime; don't leave a
            // stale pipeline id behind on the view.
            commands.entity(entity).remove::<EdgeDetectionPipelineId>();
            continue;
        }

        let id = pipeline.variants.specialize(
            &pipeline_cache,
            EdgeDetectionKey {
                target_format: view.target_format,
            },
        )?;
        commands.entity(entity).insert(EdgeDetectionPipelineId(id));
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// The pass
// ---------------------------------------------------------------------------

fn edge_detection_pass(
    view: ViewQuery<(
        &ViewTarget,
        &EdgeDetectionSettings,
        &DynamicUniformIndex<EdgeDetectionSettings>,
        &ViewPrepassTextures,
        &EdgeDetectionPipelineId,
    )>,
    pipeline: Res<EdgeDetectionPipeline>,
    pipeline_cache: Res<PipelineCache>,
    settings_uniforms: Res<ComponentUniforms<EdgeDetectionSettings>>,
    mut ctx: RenderContext,
) {
    let (view_target, settings, settings_index, prepass_textures, pipeline_id) = view.into_inner();

    // Must come before `post_process_write()`, which swaps the ping-pong
    // buffers whether or not we draw.
    if settings.enabled == 0 {
        return;
    }

    let Some(render_pipeline) = pipeline_cache.get_render_pipeline(pipeline_id.0) else {
        return;
    };
    let Some(settings_binding) = settings_uniforms.uniforms().binding() else {
        return;
    };
    let Some(depth_view) = prepass_textures.depth_view() else {
        return; // Skip if no depth prepass
    };
    let Some(normal_view) = prepass_textures.normal_view() else {
        return; // Skip if no normal prepass
    };

    let post_process = view_target.post_process_write();

    let bind_group = ctx.render_device().create_bind_group(
        "edge_detection_bind_group",
        &pipeline_cache.get_bind_group_layout(&pipeline.layout),
        &BindGroupEntries::sequential((
            post_process.source,
            depth_view,
            normal_view,
            &pipeline.sampler,
            settings_binding.clone(),
        )),
    );

    let mut render_pass = ctx.begin_tracked_render_pass(RenderPassDescriptor {
        label: Some("edge_detection_pass"),
        color_attachments: &[Some(RenderPassColorAttachment {
            view: post_process.destination,
            depth_slice: None,
            resolve_target: None,
            ops: Operations::default(),
        })],
        depth_stencil_attachment: None,
        timestamp_writes: None,
        occlusion_query_set: None,
        multiview_mask: None,
    });

    render_pass.set_render_pipeline(render_pipeline);
    render_pass.set_bind_group(0, &bind_group, &[settings_index.index()]);
    render_pass.draw(0..3, 0..1);
}

// ---------------------------------------------------------------------------
// Settings
// ---------------------------------------------------------------------------

#[derive(Component, Default, Clone, Copy, ExtractComponent, ShaderType)]
pub struct EdgeDetectionSettings {
    pub edge_thickness: f32,
    pub depth_sensitivity: f32,
    pub normal_sensitivity: f32,
    pub edge_color: LinearRgba,
    /// Set to 0 to disable edge detection, non-zero to enable
    pub enabled: u32,
}

impl EdgeDetectionSettings {
    pub fn new() -> Self {
        Self {
            edge_thickness: 1.0,
            depth_sensitivity: 2.0, // Detects depth discontinuities (geometric edges)
            normal_sensitivity: 1.5, // Detects surface angle changes
            edge_color: LinearRgba::BLACK,
            enabled: 1,
        }
    }
}

pub fn toggle_edge_detection(
    mut settings: ResMut<crate::settings::UserSettings>,
    input: Res<ButtonInput<KeyCode>>,
) {
    if input.just_pressed(KeyCode::F2) {
        settings.toggle_edge_detection();
    }
}

pub fn sync_edge_detection_settings(
    user_settings: Res<crate::settings::UserSettings>,
    mut query: Query<&mut EdgeDetectionSettings>,
) {
    if !user_settings.is_changed() {
        return;
    }
    let enabled = if user_settings.edge_detection_enabled {
        1
    } else {
        0
    };
    for mut settings in &mut query {
        settings.enabled = enabled;
        settings.depth_sensitivity = user_settings.edge_depth_sensitivity;
        settings.normal_sensitivity = user_settings.edge_normal_sensitivity;
    }
}
