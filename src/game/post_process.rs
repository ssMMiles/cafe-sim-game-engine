use bevy::{
    core_pipeline::{
        FullscreenShader,
        core_3d::graph::{Core3d, Node3d},
        prepass::ViewPrepassTextures,
    },
    ecs::query::QueryItem,
    prelude::*,
    render::{
        RenderApp, RenderStartup,
        extract_component::{
            ComponentUniforms, DynamicUniformIndex, ExtractComponent, ExtractComponentPlugin,
            UniformComponentPlugin,
        },
        render_graph::{
            NodeRunError, RenderGraphContext, RenderGraphExt, RenderLabel, ViewNode, ViewNodeRunner,
        },
        render_resource::{
            binding_types::{sampler, texture_2d, texture_2d_multisampled, texture_depth_2d_multisampled, uniform_buffer},
            *,
        },
        renderer::{RenderContext, RenderDevice},
        view::ViewTarget,
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

        render_app.add_systems(RenderStartup, init_edge_detection_pipeline);

        render_app
            .add_render_graph_node::<ViewNodeRunner<EdgeDetectionNode>>(Core3d, EdgeDetectionLabel)
            .add_render_graph_edges(
                Core3d,
                (
                    Node3d::Tonemapping,
                    EdgeDetectionLabel,
                    Node3d::EndMainPassPostProcessing,
                ),
            );
    }
}

#[derive(Debug, Hash, PartialEq, Eq, Clone, RenderLabel)]
struct EdgeDetectionLabel;

#[derive(Default)]
struct EdgeDetectionNode;

impl ViewNode for EdgeDetectionNode {
    type ViewQuery = (
        &'static ViewTarget,
        &'static EdgeDetectionSettings,
        &'static DynamicUniformIndex<EdgeDetectionSettings>,
        &'static ViewPrepassTextures,
    );

    fn run(
        &self,
        _graph: &mut RenderGraphContext,
        render_context: &mut RenderContext,
        (view_target, _settings, settings_index, prepass_textures): QueryItem<Self::ViewQuery>,
        world: &World,
    ) -> Result<(), NodeRunError> {
        let edge_pipeline = world.resource::<EdgeDetectionPipeline>();
        let pipeline_cache = world.resource::<PipelineCache>();

        let Some(render_pipeline) = pipeline_cache.get_render_pipeline(edge_pipeline.pipeline_id)
        else {
            return Ok(());
        };

        let settings_uniforms = world.resource::<ComponentUniforms<EdgeDetectionSettings>>();
        let Some(settings_binding) = settings_uniforms.uniforms().binding() else {
            return Ok(());
        };

        // Get prepass texture views (depth and normal)
        let Some(depth_view) = prepass_textures.depth_view() else {
            return Ok(()); // Skip if no depth prepass
        };
        let Some(normal_view) = prepass_textures.normal_view() else {
            return Ok(()); // Skip if no normal prepass
        };

        let post_process = view_target.post_process_write();

        let bind_group = render_context.render_device().create_bind_group(
            "edge_detection_bind_group",
            &edge_pipeline.layout,
            &BindGroupEntries::sequential((
                post_process.source,
                depth_view,
                normal_view,
                &edge_pipeline.sampler,
                settings_binding.clone(),
            )),
        );

        let mut render_pass = render_context.begin_tracked_render_pass(RenderPassDescriptor {
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
        });

        render_pass.set_render_pipeline(render_pipeline);
        render_pass.set_bind_group(0, &bind_group, &[settings_index.index()]);
        render_pass.draw(0..3, 0..1);

        Ok(())
    }
}

#[derive(Resource)]
struct EdgeDetectionPipeline {
    layout: BindGroupLayout,
    sampler: Sampler,
    pipeline_id: CachedRenderPipelineId,
}

fn init_edge_detection_pipeline(
    mut commands: Commands,
    render_device: Res<RenderDevice>,
    asset_server: Res<AssetServer>,
    fullscreen_shader: Res<FullscreenShader>,
    pipeline_cache: Res<PipelineCache>,
) {
    // Create bind group layout entries
    let layout_entries = BindGroupLayoutEntries::sequential(
        ShaderStages::FRAGMENT,
        (
            texture_2d(TextureSampleType::Float { filterable: true }),  // Screen texture
            texture_depth_2d_multisampled(),                            // Depth prepass (multisampled)
            texture_2d_multisampled(TextureSampleType::Float { filterable: false }), // Normal prepass (multisampled)
            sampler(SamplerBindingType::Filtering),
            uniform_buffer::<EdgeDetectionSettings>(true),
        ),
    );

    // Create the actual bind group layout
    let layout =
        render_device.create_bind_group_layout("edge_detection_bind_group_layout", &layout_entries);

    let sampler = render_device.create_sampler(&SamplerDescriptor::default());

    let shader = asset_server.load(SHADER_ASSET_PATH);
    let vertex_state = fullscreen_shader.to_vertex_state();
    let pipeline_id = pipeline_cache.queue_render_pipeline(RenderPipelineDescriptor {
        label: Some("edge_detection_pipeline".into()),
        layout: vec![layout.clone()],
        vertex: vertex_state,
        fragment: Some(FragmentState {
            shader,
            targets: vec![Some(ColorTargetState {
                format: TextureFormat::bevy_default(),
                blend: None,
                write_mask: ColorWrites::ALL,
            })],
            ..default()
        }),
        ..default()
    });

    commands.insert_resource(EdgeDetectionPipeline {
        layout,
        sampler,
        pipeline_id,
    });
}

#[derive(Component, Default, Clone, Copy, ExtractComponent, ShaderType)]
pub struct EdgeDetectionSettings {
    pub edge_thickness: f32,
    pub depth_sensitivity: f32,
    pub normal_sensitivity: f32,
    pub edge_color: LinearRgba,
}

impl EdgeDetectionSettings {
    pub fn new() -> Self {
        Self {
            edge_thickness: 1.0,
            depth_sensitivity: 2.0,   // Detects depth discontinuities (geometric edges)
            normal_sensitivity: 1.5,  // Detects surface angle changes
            edge_color: LinearRgba::BLACK,
        }
    }
}
