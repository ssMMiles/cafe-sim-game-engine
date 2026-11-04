//! Atmosphere post-process: distance fog and god rays.
//!
//! Both effects need the same two inputs — the scene colour and the depth
//! prepass — so they share one fullscreen pass, each switched on
//! independently by a flag in the uniform. The pass skips itself entirely
//! when both are off.
//!
//! **Why a post-process rather than fog in the material shaders.** Fog is
//! in-scattered light along the view ray: it depends only on the fragment's
//! world position, which depth gives us. Doing it here means no change to
//! `ToonShading`/`TerrainShading` (no engine-api ABI bump, no third copy of
//! a shared shader block), it covers every surface that wrote depth rather
//! than only the two toon materials, and the sky dome — which writes no
//! depth — is excluded for free, which is what we want since `SkyColors`
//! already carries the horizon.
//!
//! Pass order is `.after(edge_detection_pass).before(fxaa)`, so outlines are
//! laid down first and then fogged along with the geometry they trace.
//!
//! [`AtmosphereStyle`] is the single source of the look (editor: inspector →
//! nothing selected → Fog / God rays, persisted per user in
//! `editor_settings.json`); [`sync_atmosphere_settings`] derives the camera's
//! [`AtmosphereSettings`] uniform from it, the camera matrices and the sun.
//! The model and every knob: `docs/ATMOSPHERE.md`.

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
            binding_types::{texture_2d, texture_depth_2d_multisampled, uniform_buffer},
            *,
        },
        renderer::{RenderContext, ViewQuery},
        view::{ExtractedView, ViewTarget},
    },
};

use serde::{Deserialize, Serialize};

use super::PostProcessPass;
use super::toon_material::ToonLook;

const SHADER_ASSET_PATH: &str = "shaders/atmosphere_post.wesl";

/// Upper bound on god-ray samples; must match `MAX_RAY_SAMPLES` in the shader,
/// which sizes the loop.
pub const MAX_RAY_SAMPLES: u32 = 64;

/// How far off-screen the sun may drift before god rays have faded out, in
/// screen widths. Without a fade they would pop off the moment the sun left
/// the frame.
const RAY_OFFSCREEN_FADE: f32 = 0.35;

pub struct AtmospherePlugin;

impl Plugin for AtmospherePlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<AtmosphereStyle>();
        app.add_plugins((
            ExtractComponentPlugin::<AtmosphereSettings>::default(),
            UniformComponentPlugin::<AtmosphereSettings>::default(),
        ));

        let Some(render_app) = app.get_sub_app_mut(RenderApp) else {
            return;
        };

        render_app
            .add_systems(RenderStartup, init_atmosphere_pipeline)
            .add_systems(
                Render,
                prepare_atmosphere_pipelines.in_set(RenderSystems::Prepare),
            )
            .add_systems(
                Core3d,
                atmosphere_pass
                    .in_set(Core3dSystems::PostProcess)
                    .in_set(PostProcessPass::Atmosphere)
                    .after(PostProcessPass::EdgeDetection)
                    .after(tonemapping)
                    .before(fxaa),
            );
    }
}

// ---------------------------------------------------------------------------
// Pipeline + specialization
// ---------------------------------------------------------------------------

#[derive(Resource)]
struct AtmospherePipeline {
    layout: BindGroupLayoutDescriptor,
    variants: Variants<RenderPipeline, AtmosphereSpecializer>,
}

struct AtmosphereSpecializer;

/// As for the edge pass: the view's target format is the only variable.
#[derive(PartialEq, Eq, Hash, Clone, Copy, SpecializerKey)]
struct AtmosphereKey {
    target_format: TextureFormat,
}

impl Specializer<RenderPipeline> for AtmosphereSpecializer {
    type Key = AtmosphereKey;

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

#[derive(Component)]
struct AtmospherePipelineId(CachedRenderPipelineId);

fn init_atmosphere_pipeline(
    mut commands: Commands,
    asset_server: Res<AssetServer>,
    fullscreen_shader: Res<FullscreenShader>,
) {
    let layout_entries = BindGroupLayoutEntries::sequential(
        ShaderStages::FRAGMENT,
        (
            // Both textures are read with `textureLoad`, so no sampler. Depth
            // is multisampled because the camera keeps `Msaa::Sample4`.
            texture_2d(TextureSampleType::Float { filterable: true }), // Screen texture
            texture_depth_2d_multisampled(),                           // Depth prepass
            uniform_buffer::<AtmosphereSettings>(true),
        ),
    );

    let layout = BindGroupLayoutDescriptor::new("atmosphere_bind_group_layout", &layout_entries);

    // Placeholder format; `AtmosphereSpecializer` overwrites it per view.
    let descriptor = RenderPipelineDescriptor {
        label: Some("atmosphere_pipeline".into()),
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

    commands.insert_resource(AtmospherePipeline {
        layout,
        variants: Variants::new(AtmosphereSpecializer, descriptor),
    });
}

fn prepare_atmosphere_pipelines(
    mut commands: Commands,
    pipeline_cache: Res<PipelineCache>,
    mut pipeline: ResMut<AtmospherePipeline>,
    views: Query<(Entity, &ExtractedView, Has<AtmosphereSettings>), With<ExtractedCamera>>,
) -> Result<(), BevyError> {
    for (entity, view, has_settings) in &views {
        if !has_settings {
            commands.entity(entity).remove::<AtmospherePipelineId>();
            continue;
        }
        let id = pipeline.variants.specialize(
            &pipeline_cache,
            AtmosphereKey {
                target_format: view.target_format,
            },
        )?;
        commands.entity(entity).insert(AtmospherePipelineId(id));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// The pass
// ---------------------------------------------------------------------------

fn atmosphere_pass(
    view: ViewQuery<(
        &ViewTarget,
        &AtmosphereSettings,
        &DynamicUniformIndex<AtmosphereSettings>,
        &ViewPrepassTextures,
        &AtmospherePipelineId,
    )>,
    pipeline: Res<AtmospherePipeline>,
    pipeline_cache: Res<PipelineCache>,
    settings_uniforms: Res<ComponentUniforms<AtmosphereSettings>>,
    mut ctx: RenderContext,
) {
    let (view_target, settings, settings_index, prepass_textures, pipeline_id) = view.into_inner();

    // Before `post_process_write()`, which swaps the ping-pong buffers
    // whether or not we draw. Nothing on means no pass at all, so the GPU
    // timing below reads zero when both effects are off.
    if !settings.any_enabled() {
        return;
    }

    let Some(render_pipeline) = pipeline_cache.get_render_pipeline(pipeline_id.0) else {
        return;
    };
    let Some(settings_binding) = settings_uniforms.uniforms().binding() else {
        return;
    };
    let Some(depth_view) = prepass_textures.depth_only_view() else {
        return; // No depth prepass: nothing to reconstruct positions from.
    };

    let post_process = view_target.post_process_write();

    let bind_group = ctx.render_device().create_bind_group(
        "atmosphere_bind_group",
        &pipeline_cache.get_bind_group_layout(&pipeline.layout),
        &BindGroupEntries::sequential((post_process.source, depth_view, settings_binding.clone())),
    );

    let diagnostics = ctx.diagnostic_recorder();
    let diagnostics = diagnostics.as_deref();
    let mut render_pass = ctx.begin_tracked_render_pass(RenderPassDescriptor {
        label: Some("atmosphere_pass"),
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
    let span = diagnostics.pass_span(&mut render_pass, "atmosphere");
    render_pass.set_render_pipeline(render_pipeline);
    render_pass.set_bind_group(0, &bind_group, &[settings_index.index()]);
    render_pass.draw(0..3, 0..1);
    span.end(&mut render_pass);
}

// ---------------------------------------------------------------------------
// Settings
// ---------------------------------------------------------------------------

/// How the atmosphere looks. The single source of every fog and god-ray knob
/// — the editor edits it and [`sync_atmosphere_settings`] turns it into the
/// camera's uniform. A resource rather than a camera component, so edits
/// survive the camera being respawned on every entry to the game. Persisted
/// per user in `editor_settings.json` (`editor::ui::persist`); every field
/// defaults, so a file written before a field existed still loads.
///
/// Fog is arguably *world* data, like the toon curve — see
/// `docs/ATMOSPHERE.md` §6.
#[derive(Resource, Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AtmosphereStyle {
    pub fog_enabled: bool,
    /// Linear RGB.
    pub fog_color: [f32; 3],
    /// Extinction per metre at the fog base height.
    pub fog_density: f32,
    /// Distance in metres before any fog accumulates.
    pub fog_start: f32,
    /// Ceiling on fog opacity, so the far distance never fully flattens.
    pub fog_max: f32,
    /// Height over which the density falls by `1/e`. 0 = uniform fog.
    pub fog_height_falloff: f32,
    /// World Y at which the density equals `fog_density`.
    pub fog_height_base: f32,
    /// How much of the sun's colour the fog picks up facing into it.
    pub fog_sun_tint: f32,
    /// Tightness of that tint around the sun; higher is tighter.
    pub fog_sun_exponent: f32,
    /// Quantise the fog amount into this many steps, like the toon curve
    /// does for brightness. 0 leaves it smooth.
    pub fog_steps: u32,

    pub rays_enabled: bool,
    /// Brightness of the shafts, as a multiple of the sun colour.
    pub rays_intensity: f32,
    /// Per-sample falloff along the march; below 1 the shafts shorten.
    pub rays_decay: f32,
    /// Fraction of the distance to the sun the march covers.
    pub rays_spread: f32,
    /// Depth taps per pixel. The whole cost of the effect.
    pub rays_samples: u32,
}

impl Default for AtmosphereStyle {
    fn default() -> Self {
        Self {
            fog_enabled: true,
            // The warm haze the camera's old `DistanceFog` carried, in linear
            // RGB (it was sRGB 0.85, 0.80, 0.72).
            fog_color: [0.69, 0.60, 0.48],
            fog_density: 0.0012,
            fog_start: 15.0,
            fog_max: 0.9,
            fog_height_falloff: 0.0,
            fog_height_base: 0.0,
            fog_sun_tint: 0.35,
            fog_sun_exponent: 6.0,
            fog_steps: 0,

            // Off by design: this is the experiment, not the baseline.
            rays_enabled: false,
            rays_intensity: 0.35,
            rays_decay: 0.96,
            rays_spread: 0.7,
            rays_samples: 32,
        }
    }
}

/// The uniform the shader reads, derived from [`AtmosphereStyle`], the camera
/// and the sun. Field order is the WGSL `AtmosphereSettings` struct's; every
/// field is a `vec4` so the two layouts can't drift through padding.
#[derive(Component, Default, Clone, Copy, PartialEq, ExtractComponent, ShaderType)]
#[extract_app(RenderApp)]
pub struct AtmosphereSettings {
    /// Unprojects clip space straight to world space.
    pub world_from_clip: Mat4,
    /// `xyz` = camera world position.
    pub camera: Vec4,
    /// `rgb` = fog colour, `w` = maximum opacity.
    pub fog_color: Vec4,
    /// density, start, height falloff, height base.
    pub fog_params: Vec4,
    /// `rgb` = sun colour × tint strength, `w` = tint exponent.
    pub fog_sun: Vec4,
    /// `xyz` = world direction *to* the sun.
    pub sun_dir: Vec4,
    /// `xy` = the sun in uv, `z` = visibility, `w` = ray spread.
    pub sun_screen: Vec4,
    /// `rgb` = sun colour × ray intensity, `w` = per-sample decay.
    pub ray_color: Vec4,
    /// fog on, rays on, fog steps, ray samples.
    pub flags: UVec4,
}

impl AtmosphereSettings {
    fn any_enabled(&self) -> bool {
        self.flags.x != 0 || self.flags.y != 0
    }
}

/// Where the sun sits on screen, and how much the god rays should count.
///
/// Returns uv plus a 0..1 visibility that is 0 behind the camera and fades
/// as the sun leaves the frame. Split out so it can be tested without a
/// render world.
fn sun_on_screen(clip_from_world: Mat4, sun_dir: Vec3) -> (Vec2, f32) {
    // A direction is a point at infinity: w = 0.
    let clip = clip_from_world * sun_dir.extend(0.0);
    if clip.w <= 1e-6 {
        return (Vec2::ZERO, 0.0); // behind the camera
    }
    let ndc = clip.truncate() / clip.w;
    let uv = Vec2::new(ndc.x * 0.5 + 0.5, 0.5 - ndc.y * 0.5);
    // 0 while the sun is on screen, growing as it leaves.
    let outside = ((uv - Vec2::splat(0.5)).abs().max_element() * 2.0 - 1.0).max(0.0);
    let visibility = (1.0 - outside / RAY_OFFSCREEN_FADE).clamp(0.0, 1.0);
    (uv, visibility)
}

/// Keep every atmosphere camera's uniform in step with [`AtmosphereStyle`],
/// the sun and the camera's own matrices. Recomputed every frame rather than
/// gated on change detection, because the camera moves.
pub fn sync_atmosphere_settings(
    style: Res<AtmosphereStyle>,
    look: Res<ToonLook>,
    mut cameras: Query<(&mut AtmosphereSettings, &GlobalTransform, &Projection)>,
) {
    for (mut settings, transform, projection) in &mut cameras {
        let clip_from_view = projection.get_clip_from_view();
        let world_from_view = transform.to_matrix();
        let view_from_world = world_from_view.inverse();

        let (sun_uv, sun_visibility) =
            sun_on_screen(clip_from_view * view_from_world, look.sun_dir);

        let fog_sun = look.sun_rgb * style.fog_sun_tint.max(0.0);
        let ray_rgb = look.sun_rgb * style.rays_intensity.max(0.0);

        settings.set_if_neq(AtmosphereSettings {
            world_from_clip: world_from_view * clip_from_view.inverse(),
            camera: transform.translation().extend(1.0),
            fog_color: Vec3::from(style.fog_color).extend(style.fog_max.clamp(0.0, 1.0)),
            fog_params: Vec4::new(
                style.fog_density.max(0.0),
                style.fog_start.max(0.0),
                style.fog_height_falloff.max(0.0),
                style.fog_height_base,
            ),
            fog_sun: fog_sun.extend(style.fog_sun_exponent.max(0.01)),
            sun_dir: look.sun_dir.extend(0.0),
            sun_screen: sun_uv
                .extend(sun_visibility)
                .extend(style.rays_spread.max(0.0)),
            ray_color: ray_rgb.extend(style.rays_decay.clamp(0.0, 1.0)),
            flags: UVec4::new(
                style.fog_enabled as u32,
                style.rays_enabled as u32,
                style.fog_steps,
                style.rays_samples.clamp(1, MAX_RAY_SAMPLES),
            ),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::camera::CameraProjection;
    use bevy::render::render_resource::ShaderType;

    const SHADER_SRC: &str = include_str!("../../../assets/shaders/atmosphere_post.wesl");

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
    fn atmosphere_shader_validates() {
        let src = shader_as_wgsl();
        let module = naga::front::wgsl::parse_str(&src).unwrap_or_else(|e| {
            panic!(
                "atmosphere shader failed to parse: {}",
                e.emit_to_string(&src)
            )
        });
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::empty(),
        )
        .validate(&module)
        .unwrap_or_else(|e| panic!("atmosphere shader failed validation: {e:?}"));
    }

    /// Every field past a mismatch would be read from the wrong offset.
    #[test]
    fn atmosphere_settings_match_the_shader_struct() {
        let module = naga::front::wgsl::parse_str(&shader_as_wgsl()).unwrap();
        let (_, ty) = module
            .types
            .iter()
            .find(|(_, t)| t.name.as_deref() == Some("AtmosphereSettings"))
            .expect("shader should declare AtmosphereSettings");
        let naga::TypeInner::Struct { span, .. } = ty.inner else {
            panic!("AtmosphereSettings is not a struct");
        };
        assert_eq!(
            span as u64,
            AtmosphereSettings::min_size().get(),
            "AtmosphereSettings (shader) and the Rust struct differ in size"
        );
    }

    /// The shader's loop bound and the style's cap have to agree, or a
    /// higher sample count would silently do nothing.
    #[test]
    fn ray_sample_cap_matches_the_shader() {
        let needle = format!("const MAX_RAY_SAMPLES: u32 = {MAX_RAY_SAMPLES}u;");
        assert!(
            SHADER_SRC.contains(&needle),
            "shader's MAX_RAY_SAMPLES differs from MAX_RAY_SAMPLES ({MAX_RAY_SAMPLES})"
        );
    }

    // ── The fog model ────────────────────────────────────────────────────────

    /// `optical_depth` in the shader, transliterated. The shader is the copy
    /// that ships; this is the copy the tests can measure.
    fn optical_depth(density: f32, len: f32, y0: f32, y1: f32, falloff: f32, base: f32) -> f32 {
        const MAX_OPTICAL_DEPTH: f32 = 50.0;
        const MAX_HEIGHT_EXPONENT: f32 = 30.0;
        if falloff <= 0.0 {
            return (density * len).min(MAX_OPTICAL_DEPTH);
        }
        let exponent = (-(y0 - base) / falloff).clamp(-MAX_HEIGHT_EXPONENT, MAX_HEIGHT_EXPONENT);
        let rho0 = density * exponent.exp();
        let k = ((y1 - y0) / falloff).clamp(-MAX_HEIGHT_EXPONENT, MAX_HEIGHT_EXPONENT);
        let factor = if k.abs() > 1e-4 {
            (1.0 - (-k).exp()) / k
        } else {
            1.0 - 0.5 * k
        };
        (rho0 * factor * len).min(MAX_OPTICAL_DEPTH)
    }

    /// The closed form must match brute-force integration of
    /// `density · exp(−(y − base)/falloff)` along the segment. This is the
    /// whole reason height fog can be trusted.
    #[test]
    fn height_fog_matches_numerical_integration() {
        let cases = [
            // (density, len, y0, y1, falloff, base)
            (0.02f32, 100.0f32, 0.0f32, 50.0f32, 20.0f32, 0.0f32),
            (0.02, 100.0, 50.0, 0.0, 20.0, 0.0), // descending
            (0.05, 30.0, 5.0, 5.0, 10.0, 0.0),   // level: the k→0 limit
            (0.01, 250.0, 2.0, 80.0, 35.0, 10.0),
            (0.03, 10.0, -5.0, 12.0, 8.0, -2.0),
        ];
        for (density, len, y0, y1, falloff, base) in cases {
            let n = 20_000;
            let mut reference = 0.0f64;
            for i in 0..n {
                let t = (i as f64 + 0.5) / n as f64;
                let y = y0 as f64 + (y1 - y0) as f64 * t;
                reference += density as f64 * (-(y - base as f64) / falloff as f64).exp();
            }
            reference *= len as f64 / n as f64;

            let got = optical_depth(density, len, y0, y1, falloff, base) as f64;
            let tolerance = reference.abs().max(1.0) * 1e-3;
            assert!(
                (got - reference).abs() < tolerance,
                "closed form {got} vs numerical {reference} for \
                 density={density} len={len} y0={y0} y1={y1} H={falloff} base={base}"
            );
        }
    }

    /// With no height falloff the model is plain Beer–Lambert, and the
    /// closed form must not drift from it.
    #[test]
    fn uniform_fog_is_beer_lambert() {
        for (density, len) in [(0.001f32, 100.0f32), (0.01, 50.0), (0.0005, 1000.0)] {
            assert!(
                (optical_depth(density, len, 0.0, 30.0, 0.0, 0.0) - density * len).abs() < 1e-6
            );
        }
    }

    /// No knob position may produce a NaN, an infinity or a negative depth —
    /// the saturation guards exist for exactly these.
    #[test]
    fn extreme_inputs_stay_finite() {
        let cases = [
            (1e6f32, 1e6f32, 0.0f32, 0.0f32, 0.0f32, 0.0f32),
            (1.0, 1000.0, -5000.0, 5000.0, 0.001, 0.0),
            (1.0, 1000.0, 5000.0, -5000.0, 0.001, 0.0),
            (0.0, 0.0, 0.0, 0.0, 1.0, 0.0),
        ];
        for (density, len, y0, y1, falloff, base) in cases {
            let od = optical_depth(density, len, y0, y1, falloff, base);
            assert!(
                od.is_finite() && (0.0..=50.0).contains(&od),
                "optical depth {od} out of range for \
                 density={density} len={len} y0={y0} y1={y1} H={falloff}"
            );
        }
    }

    // ── The sun on screen ────────────────────────────────────────────────────

    fn clip_from_world(camera: Transform) -> Mat4 {
        let projection = PerspectiveProjection {
            fov: std::f32::consts::PI / 5.0,
            aspect_ratio: 16.0 / 9.0,
            ..default()
        };
        projection.get_clip_from_view() * camera.to_matrix().inverse()
    }

    #[test]
    fn god_rays_follow_the_sun_across_the_screen() {
        // Camera at the origin looking down -Z, sun straight ahead.
        let m = clip_from_world(Transform::IDENTITY);
        let (uv, visibility) = sun_on_screen(m, Vec3::NEG_Z);
        assert!(visibility > 0.99, "sun dead ahead should be fully visible");
        assert!(
            (uv - Vec2::splat(0.5)).length() < 1e-4,
            "sun dead ahead should sit at the centre, got {uv:?}"
        );

        // Behind the camera: no rays at all.
        let (_, behind) = sun_on_screen(m, Vec3::Z);
        assert_eq!(behind, 0.0);

        // Off to one side: still on screen, but left of centre.
        let m = clip_from_world(Transform::IDENTITY.with_rotation(Quat::from_rotation_y(-0.15)));
        let (uv, visibility) = sun_on_screen(m, Vec3::NEG_Z);
        assert!(
            visibility > 0.0 && uv.x < 0.5,
            "got {uv:?} vis {visibility}"
        );

        // Far enough to the side and the rays have faded out entirely.
        let m = clip_from_world(
            Transform::IDENTITY.with_rotation(Quat::from_rotation_y(-std::f32::consts::FRAC_PI_2)),
        );
        let (_, visibility) = sun_on_screen(m, Vec3::NEG_Z);
        assert_eq!(visibility, 0.0, "sun at 90° should not light any shafts");
    }
}
