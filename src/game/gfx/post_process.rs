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
//!
//! What the shader measures and what each knob means: `docs/EDGE_DETECTION.md`.
//! [`EdgeStyle`] is the one source of the look; [`sync_edge_detection_settings`]
//! derives the camera's [`EdgeDetectionSettings`] uniform from it.

use bevy::{
    anti_alias::fxaa::fxaa,
    core_pipeline::{
        Core3d, Core3dSystems, FullscreenShader, prepass::ViewPrepassTextures,
        tonemapping::tonemapping,
    },
    ecs::error::BevyError,
    prelude::*,
    render::{
        Render, RenderApp, RenderStartup, RenderSystems,
        camera::ExtractedCamera,
        diagnostic::RecordDiagnostics,
        extract_component::{
            ComponentUniforms, DynamicUniformIndex, ExtractComponent, ExtractComponentPlugin,
            UniformComponentPlugin,
        },
        render_resource::{
            binding_types::{
                texture_2d, texture_2d_multisampled, texture_depth_2d_multisampled, uniform_buffer,
            },
            *,
        },
        renderer::{RenderContext, ViewQuery},
        view::{ExtractedView, ViewTarget},
    },
};

use serde::{Deserialize, Serialize};

const SHADER_ASSET_PATH: &str = "shaders/edge_detection_post.wesl";

pub struct EdgeDetectionPlugin;

impl Plugin for EdgeDetectionPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<EdgeStyle>();
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
                // Before FXAA, so the lines are antialiased with the rest of
                // the image; both only said `.after(tonemapping)` before,
                // which left their order to the scheduler.
                edge_detection_pass
                    .in_set(Core3dSystems::PostProcess)
                    .in_set(super::PostProcessPass::EdgeDetection)
                    .after(tonemapping)
                    .before(fxaa),
            );
    }
}

// ---------------------------------------------------------------------------
// Pipeline + specialization
// ---------------------------------------------------------------------------

#[derive(Resource)]
struct EdgeDetectionPipeline {
    layout: BindGroupLayoutDescriptor,
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
    asset_server: Res<AssetServer>,
    fullscreen_shader: Res<FullscreenShader>,
) {
    let layout_entries = BindGroupLayoutEntries::sequential(
        ShaderStages::FRAGMENT,
        (
            // Every texture is read with `textureLoad` at the pixel, so no
            // sampler. The prepass bindings are multisampled because the
            // camera keeps the default `Msaa::Sample4`; turning MSAA off would
            // need a second layout.
            texture_2d(TextureSampleType::Float { filterable: true }), // Screen texture
            texture_depth_2d_multisampled(),                           // Depth prepass
            texture_2d_multisampled(TextureSampleType::Float { filterable: false }), // Normal prepass
            uniform_buffer::<EdgeDetectionSettings>(true),
        ),
    );

    let layout =
        BindGroupLayoutDescriptor::new("edge_detection_bind_group_layout", &layout_entries);

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
    let Some(depth_view) = prepass_textures.depth_only_view() else {
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
            settings_binding.clone(),
        )),
    );

    let diagnostics = ctx.diagnostic_recorder();
    let diagnostics = diagnostics.as_deref();
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

    // Named span so the editor's GPU timings panel can show what this costs.
    let span = diagnostics.pass_span(&mut render_pass, "edge detection");
    render_pass.set_render_pipeline(render_pipeline);
    render_pass.set_bind_group(0, &bind_group, &[settings_index.index()]);
    render_pass.draw(0..3, 0..1);
    span.end(&mut render_pass);
}

// ---------------------------------------------------------------------------
// Settings
// ---------------------------------------------------------------------------

/// How outlines look. The single source of every edge knob — the editor edits
/// it (inspector → nothing selected → Edges) and
/// [`sync_edge_detection_settings`] turns it into the camera's uniform. A
/// resource rather than the camera component, so edits survive the camera
/// being respawned on every entry to the game. Persisted per user in
/// `editor_settings.json` (`editor::ui::persist`); every field defaults, so a
/// file from before a field existed still loads.
///
/// Units and failure modes of each field: `docs/EDGE_DETECTION.md`.
#[derive(Resource, Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct EdgeStyle {
    /// Sample offset in pixels. Lines come out about twice this wide (both
    /// sides of a boundary are marked); above 1, features thinner than the
    /// offset can slip between the taps.
    pub thickness: f32,
    /// Smallest depth step that draws a line, as `1 − near/far` of the two
    /// surfaces: 0.05 ≈ the far one is 5% further away. Distance-independent.
    pub depth_threshold: f32,
    /// How much a surface's own pixel-to-pixel depth slope adds to the
    /// threshold. Suppresses lines on bumpy ground seen at a grazing angle;
    /// 0 turns the compensation off.
    pub grazing_bias: f32,
    /// Smallest angle between neighbouring normals that draws a line, degrees.
    pub crease_angle: f32,
    /// Half-width of the fade-in band around the thresholds, as a fraction of
    /// them, so a near-threshold edge fades instead of popping.
    pub softness: f32,
    /// Lines fade out between these distances from the camera, metres.
    pub fade_start: f32,
    pub fade_end: f32,
    /// Line colour, linear RGBA; alpha is its opacity.
    pub color: [f32; 4],
}

impl Default for EdgeStyle {
    fn default() -> Self {
        Self {
            thickness: 1.0,
            depth_threshold: 0.04,
            grazing_bias: 1.0,
            crease_angle: 30.0,
            softness: 0.25,
            fade_start: 120.0,
            fade_end: 240.0,
            color: [0.0, 0.0, 0.0, 1.0],
        }
    }
}

/// The uniform the shader reads, derived from [`EdgeStyle`]. Field order is
/// the WGSL `EdgeSettings` struct's.
#[derive(Component, Default, Clone, Copy, PartialEq, ExtractComponent, ShaderType)]
#[extract_app(RenderApp)]
pub struct EdgeDetectionSettings {
    pub edge_color: LinearRgba,
    pub depth_threshold: f32,
    pub grazing_bias: f32,
    /// `1 − cos(crease_angle)`: what the shader compares `1 − n·n'` against.
    pub normal_threshold: f32,
    pub softness: f32,
    pub thickness: f32,
    pub fade_start: f32,
    pub fade_end: f32,
    /// The camera's near plane: prepass depth is `near / distance`.
    pub near: f32,
    /// 0 skips the pass entirely.
    pub enabled: u32,
}

impl EdgeDetectionSettings {
    /// Every divisor and smoothstep edge the shader derives is kept away from
    /// zero here, so no knob position can produce NaN.
    pub fn new(style: &EdgeStyle, near: f32, enabled: bool) -> Self {
        let crease = style.crease_angle.clamp(1.0, 180.0).to_radians();
        let fade_start = style.fade_start.max(0.0);
        Self {
            edge_color: LinearRgba::from_f32_array(style.color),
            depth_threshold: style.depth_threshold.max(1e-4),
            grazing_bias: style.grazing_bias.max(0.0),
            normal_threshold: 1.0 - crease.cos(),
            softness: style.softness.clamp(1e-3, 0.99),
            thickness: style.thickness.max(1.0),
            fade_start,
            fade_end: style.fade_end.max(fade_start + 0.01),
            near,
            enabled: enabled as u32,
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

/// Keep every edge-detecting camera's uniform in step with [`EdgeStyle`], the
/// F2 toggle and the camera's near plane. Compared every frame rather than
/// gated on change detection: the uniform used to be seeded by the camera and
/// then overwritten only when `UserSettings` changed, so which values were
/// live depended on timing.
pub fn sync_edge_detection_settings(
    user_settings: Res<crate::settings::UserSettings>,
    style: Res<EdgeStyle>,
    mut cameras: Query<(&mut EdgeDetectionSettings, &Projection)>,
) {
    for (mut settings, projection) in &mut cameras {
        // The depth term assumes reverse-Z perspective (`near / distance`).
        let Projection::Perspective(perspective) = projection else {
            continue;
        };
        settings.set_if_neq(EdgeDetectionSettings::new(
            &style,
            perspective.near,
            user_settings.edge_detection_enabled,
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::render::render_resource::ShaderType;

    const SHADER_SRC: &str = include_str!("../../../assets/shaders/edge_detection_post.wesl");

    /// The shader as plain WGSL: its one import replaced by the struct it
    /// names (bevy_core_pipeline's fullscreen vertex output).
    fn shader_as_wgsl() -> String {
        const IMPORT: &str =
            "import bevy_core_pipeline::fullscreen_vertex_shader::FullscreenVertexOutput;";
        assert!(SHADER_SRC.contains(IMPORT), "the shader's import changed");
        SHADER_SRC.replace(
            IMPORT,
            "struct FullscreenVertexOutput { @builtin(position) position: vec4<f32>, \
             @location(0) uv: vec2<f32> }",
        )
    }

    #[test]
    fn edge_shader_validates() {
        let src = shader_as_wgsl();
        let module = naga::front::wgsl::parse_str(&src)
            .unwrap_or_else(|e| panic!("edge shader failed to parse: {}", e.emit_to_string(&src)));
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::empty(),
        )
        .validate(&module)
        .unwrap_or_else(|e| panic!("edge shader failed validation: {e:?}"));
    }

    /// Every field past a mismatch would be read from the wrong offset.
    #[test]
    fn edge_settings_match_the_shader_struct() {
        let module = naga::front::wgsl::parse_str(&shader_as_wgsl()).unwrap();
        let (_, ty) = module
            .types
            .iter()
            .find(|(_, t)| t.name.as_deref() == Some("EdgeSettings"))
            .expect("shader should declare EdgeSettings");
        let naga::TypeInner::Struct { span, .. } = ty.inner else {
            panic!("EdgeSettings is not a struct");
        };
        assert_eq!(
            span as u64,
            EdgeDetectionSettings::min_size().get(),
            "EdgeSettings (shader) and EdgeDetectionSettings (Rust) differ in size"
        );
    }

    /// No knob position may produce a zero divisor or an empty smoothstep.
    #[test]
    fn sanitised_settings_have_no_degenerate_divisors() {
        let style = EdgeStyle {
            thickness: 0.0,
            depth_threshold: 0.0,
            grazing_bias: -1.0,
            crease_angle: 0.0,
            softness: 0.0,
            fade_start: 50.0,
            fade_end: 10.0,
            color: [0.0, 0.0, 0.0, 1.0],
        };
        let s = EdgeDetectionSettings::new(&style, 0.1, true);
        assert!(s.depth_threshold > 0.0 && s.normal_threshold > 0.0);
        assert!(s.softness > 0.0 && s.thickness >= 1.0 && s.grazing_bias >= 0.0);
        assert!(s.fade_end > s.fade_start);
    }

    // ── The depth term's geometry ────────────────────────────────────────────

    const NEAR: f32 = 0.1;

    /// `depth_strength` in the shader, transliterated.
    fn depth_strength(a: f32, c: f32, b: f32, threshold: f32, grazing_bias: f32) -> f32 {
        let nearest = c.max(a).max(b);
        if nearest <= 0.0 {
            return 0.0;
        }
        let step = (a + b - 2.0 * c).abs() / nearest;
        let slope = (a - c).abs().min((b - c).abs()) / nearest;
        step / (threshold + grazing_bias * slope)
    }

    /// Reverse-Z prepass depth of the ray through screen column `x` (pixels
    /// from the centre, 1080p at the game camera's fov) hitting the plane
    /// `dot(p, normal) = offset`. Camera at the origin looking down −Z.
    fn plane_depth(x: f32, normal: Vec3, offset: f32) -> f32 {
        let pixel = (std::f32::consts::PI / 5.0 / 2.0).tan() / 540.0;
        let dir = Vec3::new(x * pixel, 0.0, -1.0);
        let t = offset / dir.dot(normal);
        NEAR / (t * -dir.z)
    }

    /// The property the depth term is built on: a plane reads as zero at any
    /// angle and distance, so flat ground seen edge-on never outlines itself.
    /// (The raw first difference the shader used to threshold does not have
    /// it, and was also ~`near/z²` small — dead beyond a couple of metres.)
    #[test]
    fn planes_read_as_no_edge_at_any_angle() {
        for degrees in [0.0f32, 30.0, 60.0, 80.0, 88.0] {
            // Rotating the normal away from the view axis tilts the plane
            // towards grazing.
            let normal = Quat::from_rotation_y(degrees.to_radians()) * Vec3::Z;
            for distance in [2.0f32, 20.0, 200.0] {
                let offset = -distance * normal.z;
                let [a, c, b] = [-1.0, 0.0, 1.0].map(|x| plane_depth(x, normal, offset));
                let s = depth_strength(a, c, b, 0.04, 0.0);
                assert!(s < 0.05, "{degrees}° at {distance} m reads {s}");
            }
        }
    }

    /// A step from distance z to z′ reads `1 − z/z′` wherever it is, so the
    /// threshold is a fraction, not a raw depth.
    #[test]
    fn depth_steps_read_as_relative_distance() {
        for (near_z, far_z) in [(1.0f32, 1.5), (5.0, 5.75), (40.0, 60.0)] {
            let [a, c, b] = [near_z, near_z, far_z].map(|z| NEAR / z);
            let measured = depth_strength(a, c, b, 1.0, 0.0);
            let expected = 1.0 - near_z / far_z;
            assert!(
                (measured - expected).abs() < 1e-4,
                "{near_z} → {far_z}: {measured} vs {expected}"
            );
        }
    }
}
