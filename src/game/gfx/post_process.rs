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
                texture_2d, texture_2d_multisampled, texture_depth_2d,
                texture_depth_2d_multisampled, uniform_buffer,
            },
            *,
        },
        renderer::{RenderContext, ViewQuery},
        view::{ExtractedView, ViewTarget},
    },
    shader::ShaderDefVal,
};

use serde::{Deserialize, Serialize};

use super::profiling::{BenchEffect, EffectOverride};

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
                // The order against the atmosphere pass is the
                // `PostProcessPass` set (edges first, so outlines fog with the
                // geometry they trace). There is no FXAA after either of them
                // any more: the lines antialias themselves by evaluating the
                // edge test per MSAA sample (see the shader).
                edge_detection_pass
                    .in_set(Core3dSystems::PostProcess)
                    .in_set(super::PostProcessPass::EdgeDetection)
                    .after(tonemapping),
            );
    }
}

// ---------------------------------------------------------------------------
// Pipeline + specialization
// ---------------------------------------------------------------------------

#[derive(Resource)]
struct EdgeDetectionPipeline {
    /// Two layouts because the prepass textures are multisampled exactly when
    /// the view's MSAA is on, and the binding type must agree. The pass picks
    /// by the view's `Msaa` ([`EdgeDetectionPipeline::layout`]), the
    /// specializer by the key's sample count — the same number, so the
    /// pipeline and the bind group can't disagree.
    layout_multisampled: BindGroupLayoutDescriptor,
    layout_single_sampled: BindGroupLayoutDescriptor,
    variants: Variants<RenderPipeline, EdgeDetectionSpecializer>,
}

impl EdgeDetectionPipeline {
    fn layout(&self, msaa_samples: u32) -> &BindGroupLayoutDescriptor {
        if msaa_samples > 1 {
            &self.layout_multisampled
        } else {
            &self.layout_single_sampled
        }
    }
}

struct EdgeDetectionSpecializer {
    layout_multisampled: BindGroupLayoutDescriptor,
    layout_single_sampled: BindGroupLayoutDescriptor,
}

/// What varies between views: the format we render into, and the MSAA sample
/// count of the prepass textures. The shader evaluates the edge test once per
/// sample and averages — run after the colour resolve, that averaging is the
/// only antialiasing the outlines get (`docs/EDGE_DETECTION.md` §AA).
#[derive(PartialEq, Eq, Hash, Clone, Copy, SpecializerKey)]
struct EdgeDetectionKey {
    target_format: TextureFormat,
    msaa_samples: u32,
}

impl Specializer<RenderPipeline> for EdgeDetectionSpecializer {
    type Key = EdgeDetectionKey;

    fn specialize(
        &self,
        key: Self::Key,
        descriptor: &mut RenderPipelineDescriptor,
    ) -> Result<Canonical<Self::Key>, BevyError> {
        descriptor.layout = vec![if key.msaa_samples > 1 {
            self.layout_multisampled.clone()
        } else {
            self.layout_single_sampled.clone()
        }];
        let fragment = descriptor.fragment_mut()?;
        fragment.set_target(
            0,
            ColorTargetState {
                format: key.target_format,
                blend: None,
                write_mask: ColorWrites::ALL,
            },
        );
        // `MSAA_SAMPLES` is the shader's loop bound (a `constants::` entry);
        // `MULTISAMPLED` switches the texture declarations, since WebGPU wants
        // the binding type to match the actual textures.
        fragment.shader_defs.push(ShaderDefVal::UInt(
            "MSAA_SAMPLES".into(),
            key.msaa_samples.max(1),
        ));
        if key.msaa_samples > 1 {
            fragment.shader_defs.push("MULTISAMPLED".into());
        }
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
    // Every texture is read with `textureLoad` at the pixel, so no sampler.
    // The prepass bindings' sampledness follows the view's MSAA; one layout
    // for each case.
    let layout_for = |multisampled: bool| {
        let depth = if multisampled {
            texture_depth_2d_multisampled()
        } else {
            texture_depth_2d()
        };
        let normal = if multisampled {
            texture_2d_multisampled(TextureSampleType::Float { filterable: false })
        } else {
            texture_2d(TextureSampleType::Float { filterable: false })
        };
        let entries = BindGroupLayoutEntries::sequential(
            ShaderStages::FRAGMENT,
            (
                texture_2d(TextureSampleType::Float { filterable: true }), // Screen texture
                depth,                                                     // Depth prepass
                normal,                                                    // Normal prepass
                uniform_buffer::<EdgeDetectionSettings>(true),
            ),
        );
        BindGroupLayoutDescriptor::new(
            if multisampled {
                "edge_detection_bind_group_layout_msaa"
            } else {
                "edge_detection_bind_group_layout"
            },
            &entries,
        )
    };
    let layout_multisampled = layout_for(true);
    let layout_single_sampled = layout_for(false);

    // The layout and target format here are placeholders:
    // `EdgeDetectionSpecializer` overwrites both per view, from the view's
    // `Msaa` and `ExtractedView::target_format`.
    let descriptor = RenderPipelineDescriptor {
        label: Some("edge_detection_pipeline".into()),
        layout: vec![layout_multisampled.clone()],
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
        layout_multisampled: layout_multisampled.clone(),
        layout_single_sampled: layout_single_sampled.clone(),
        variants: Variants::new(
            EdgeDetectionSpecializer {
                layout_multisampled,
                layout_single_sampled,
            },
            descriptor,
        ),
    });
}

/// Specialize the pipeline for each view's target format and MSAA sample
/// count once per frame.
fn prepare_edge_detection_pipelines(
    mut commands: Commands,
    pipeline_cache: Res<PipelineCache>,
    mut pipeline: ResMut<EdgeDetectionPipeline>,
    views: Query<
        (Entity, &ExtractedView, &Msaa, Has<EdgeDetectionSettings>),
        With<ExtractedCamera>,
    >,
) -> Result<(), BevyError> {
    for (entity, view, msaa, has_settings) in &views {
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
                msaa_samples: msaa.samples(),
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
        &Msaa,
    )>,
    pipeline: Res<EdgeDetectionPipeline>,
    pipeline_cache: Res<PipelineCache>,
    settings_uniforms: Res<ComponentUniforms<EdgeDetectionSettings>>,
    mut ctx: RenderContext,
) {
    let (view_target, settings, settings_index, prepass_textures, pipeline_id, msaa) =
        view.into_inner();

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
        &pipeline_cache.get_bind_group_layout(pipeline.layout(msaa.samples())),
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
    /// Crease lines (the normal test) fade out between these distances —
    /// separately from silhouettes, and much sooner by default: a crease is
    /// surface detail, and dense geometry (foliage) turns into scribble at
    /// range if its creases persist as far as its silhouette does.
    pub crease_fade_start: f32,
    pub crease_fade_end: f32,
    /// The automatic outline-weight rule's thresholds, metres of world-space
    /// mesh size (largest AABB dimension): below `small` a mesh's lines draw
    /// at a third strength, below `large` at two-thirds, at or past `large`
    /// in full. An `OutlineWeight` component on the mesh or an ancestor
    /// overrides the rule. `docs/EDGE_DETECTION.md`.
    pub auto_weight_small: f32,
    pub auto_weight_large: f32,
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
            crease_fade_start: 40.0,
            crease_fade_end: 80.0,
            auto_weight_small: 0.6,
            auto_weight_large: 1.5,
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
    /// The crease test's own fade band (silhouettes use the one above).
    pub crease_fade_start: f32,
    pub crease_fade_end: f32,
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
        let crease_fade_start = style.crease_fade_start.max(0.0);
        Self {
            edge_color: LinearRgba::from_f32_array(style.color),
            depth_threshold: style.depth_threshold.max(1e-4),
            grazing_bias: style.grazing_bias.max(0.0),
            normal_threshold: 1.0 - crease.cos(),
            softness: style.softness.clamp(1e-3, 0.99),
            thickness: style.thickness.max(1.0),
            fade_start,
            fade_end: style.fade_end.max(fade_start + 0.01),
            crease_fade_start,
            crease_fade_end: style.crease_fade_end.max(crease_fade_start + 0.01),
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
    bench: Res<EffectOverride>,
    mut cameras: Query<(&mut EdgeDetectionSettings, &Projection)>,
) {
    let enabled =
        user_settings.edge_detection_enabled && !bench.suppressed(BenchEffect::EdgeDetection);
    for (mut settings, projection) in &mut cameras {
        // The depth term assumes reverse-Z perspective (`near / distance`).
        let Projection::Perspective(perspective) = projection else {
            continue;
        };
        settings.set_if_neq(EdgeDetectionSettings::new(
            &style,
            perspective.near,
            enabled,
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::render::render_resource::ShaderType;

    // The shader itself (both the multisampled and the single-sampled
    // variant) is compiled and naga-validated by
    // `shader_check::edge_detection_compiles_multisampled_and_not`, which runs
    // the real WESL pipeline — the plain-WGSL parse that used to live here
    // stopped working when the shader gained `@if` branches.

    /// Every field past a mismatch would be read from the wrong offset.
    #[test]
    fn edge_settings_match_the_shader_struct() {
        let wgsl = crate::game::gfx::shader_check::check(
            "shaders::edge_detection_post",
            &["MULTISAMPLED"],
        )
        .expect("edge shader should compile");
        let module = naga::front::wgsl::parse_str(&wgsl).unwrap();
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
            crease_fade_start: 50.0,
            crease_fade_end: 10.0,
            auto_weight_small: 0.6,
            auto_weight_large: 1.5,
            color: [0.0, 0.0, 0.0, 1.0],
        };
        let s = EdgeDetectionSettings::new(&style, 0.1, true);
        assert!(s.depth_threshold > 0.0 && s.normal_threshold > 0.0);
        assert!(s.softness > 0.0 && s.thickness >= 1.0 && s.grazing_bias >= 0.0);
        assert!(s.fade_end > s.fade_start);
        assert!(s.crease_fade_end > s.crease_fade_start);
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
