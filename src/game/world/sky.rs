//! The sky dome: a camera-centred sphere whose colours are derived from the
//! level's lighting, so the sky you see is the sky that lights the world.
//!
//! Until 2026-11-02 the dome was a hardcoded midday-blue gradient that read
//! nothing from [`EnvironmentData`]. The brain takes the dome as the light
//! source, so a warm low sun or a dim night ambient under the same bright
//! blue dome made the shading look wrong even when the bake was right, and a
//! night scene was impossible. Now every stop is a function of the ambient
//! and sun radiances, through the same exposure (`ToonCurve::white`) the
//! surfaces use, so turning the lights down turns the sky down with them.
//!
//! The derivation ([`SkyColors::from_environment`]) is a look decision, kept
//! in one place with named constants. The physical anchor is that the sky
//! *is* the ambient term: a sky of uniform radiance `A` delivers `A` to an
//! upward surface in lightmap units, so a sky displayed near `ambient` is
//! coherent with a white surface lit only by it. Real clear skies are also
//! several times dimmer than sunlit white, which is what this reproduces —
//! the dome is no longer brighter than the ground it lights.

use bevy::{
    light::NotShadowCaster,
    // `OpaqueRendererMethod` and `SpecializedMeshPipelineError` moved into the
    // new `bevy_material` crate in Bevy 0.19.
    material::{OpaqueRendererMethod, specialize::SpecializedMeshPipelineError},
    mesh::MeshVertexBufferLayoutRef,
    pbr::{MaterialPipeline, MaterialPipelineKey},
    prelude::*,
    render::render_resource::{AsBindGroup, RenderPipelineDescriptor, ShaderType},
    shader::ShaderRef,
};

use crate::game::{
    GameEntity,
    gfx::camera::PlayerCamera,
    world::terrain::storage::{EnvironmentData, TerrainWorld},
};

/// Marker for the sky dome entity, used by `sync_sky_dome` to keep it centred on the camera.
#[derive(Component)]
pub struct SkyDomeMarker;

/// The dome's colours, already exposed for display. Mirrors `SkyColors` in
/// `sky_dome.wesl` field for field.
#[derive(Clone, Copy, Debug, PartialEq, ShaderType)]
pub struct SkyColors {
    /// At the horizon (w unused).
    pub horizon: Vec4,
    /// Straight up (w unused).
    pub zenith: Vec4,
    /// Below the horizon — hidden by terrain almost everywhere (w unused).
    pub ground: Vec4,
    /// Unit direction *toward* the sun (w unused).
    pub sun_dir: Vec4,
    /// Colour of the sun's disc; `w` is the strength of its halo.
    pub sun: Vec4,
}

/// Zenith radiance as a fraction of the ambient: a clear sky is darkest
/// overhead.
const ZENITH_GAIN: f32 = 0.9;
/// Horizon radiance as a fraction of the ambient, before haze.
const HORIZON_GAIN: f32 = 1.6;
/// How much of the sun's own colour the horizon haze picks up while the sun
/// is up — white at noon, orange at sunset, from the one `sun_color`.
const HAZE: f32 = 0.12;
/// Strength of the glow around the disc, relative to the sun's colour.
const HALO: f32 = 0.25;
/// What lies below the horizon, as a fraction of the ambient.
const GROUND_GAIN: f32 = 0.45;

impl SkyColors {
    /// Derive the dome from the level's lighting. Everything is divided by
    /// the toon curve's `white`, the exposure every surface is displayed
    /// through, so the sky and the ground it lights move together when a
    /// world is re-exposed.
    pub fn from_environment(env: &EnvironmentData) -> Self {
        let exposure = 1.0 / env.toon_curve().white;
        let ambient = env.ambient_radiance();
        let sun_rgb = env.sun_radiance();
        let sun_dir = -env.sun_transform().forward().as_vec3();
        // How much of the sun is up: 1 well above the horizon, 0 once it has
        // set, so a sun parked under the ground for a night scene does not
        // keep lighting the haze.
        let daylight = smoothstep(-0.1, 0.25, sun_dir.y);

        let zenith = ambient * ZENITH_GAIN;
        let horizon = ambient * HORIZON_GAIN + sun_rgb * (HAZE * daylight);
        let ground = ambient * GROUND_GAIN;
        Self {
            horizon: (horizon * exposure).extend(0.0),
            zenith: (zenith * exposure).extend(0.0),
            ground: (ground * exposure).extend(0.0),
            sun_dir: sun_dir.normalize_or(Vec3::Y).extend(0.0),
            sun: (sun_rgb * exposure).extend(HALO),
        }
    }
}

impl Default for SkyColors {
    fn default() -> Self {
        Self::from_environment(&EnvironmentData::default())
    }
}

fn smoothstep(low: f32, high: f32, x: f32) -> f32 {
    let t = ((x - low) / (high - low)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// A simple unlit forward material that renders a toon-gradient sky.
#[derive(Asset, TypePath, AsBindGroup, Clone, Default)]
pub struct SkyDomeMaterial {
    #[uniform(0)]
    pub colors: SkyColors,
}

impl Material for SkyDomeMaterial {
    fn fragment_shader() -> ShaderRef {
        "shaders/sky_dome.wesl".into()
    }

    fn alpha_mode(&self) -> AlphaMode {
        AlphaMode::Opaque
    }

    fn opaque_render_method(&self) -> OpaqueRendererMethod {
        // Use forward rendering so it bypasses the deferred g-buffer pipeline.
        OpaqueRendererMethod::Forward
    }

    fn specialize(
        _pipeline: &MaterialPipeline,
        descriptor: &mut RenderPipelineDescriptor,
        _layout: &MeshVertexBufferLayoutRef,
        _key: MaterialPipelineKey<Self>,
    ) -> Result<(), SpecializedMeshPipelineError> {
        // The camera is *inside* the sphere, so disable back-face culling.
        descriptor.primitive.cull_mode = None;
        // Disable depth write so the sky dome always sits behind all other geometry
        // without poisoning the depth buffer.
        if let Some(ref mut ds) = descriptor.depth_stencil {
            ds.depth_write_enabled = Some(false);
        }
        Ok(())
    }
}

/// Spawns the sky dome sphere.
/// Runs `OnEnter(AppState::InGame)` after the main `setup` system.
pub fn setup_sky(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut sky_materials: ResMut<Assets<SkyDomeMaterial>>,
) {
    // A large UV sphere centred on the world origin. The `sync_sky_dome` system
    // re-centres it on the camera every frame so the horizon band stays at eye
    // level; the shader reads its direction from the sphere's radial normals,
    // so the horizon is at eye level however high the camera is.
    commands.spawn((
        GameEntity,
        SkyDomeMarker,
        Mesh3d(meshes.add(Sphere::new(900.0).mesh().uv(32, 16))),
        MeshMaterial3d(sky_materials.add(SkyDomeMaterial::default())),
        NotShadowCaster,
    ));
}

/// Keeps the sky dome centred on the player camera so the horizon band stays at eye level.
pub fn sync_sky_dome(
    camera_query: Query<&Transform, With<PlayerCamera>>,
    mut sky_query: Query<&mut Transform, (With<SkyDomeMarker>, Without<PlayerCamera>)>,
) {
    let Ok(camera_transform) = camera_query.single() else {
        return;
    };
    let Ok(mut sky_transform) = sky_query.single_mut() else {
        return;
    };
    sky_transform.translation = camera_transform.translation;
}

/// Follow the level's lighting into the dome: on every environment change
/// (the inspector writes through `ResMut<TerrainWorld>`) and when the dome
/// is first spawned, which can be before or after the world loads.
pub fn sync_sky_colors(
    terrain_world: Option<Res<TerrainWorld>>,
    domes: Query<&MeshMaterial3d<SkyDomeMaterial>, With<SkyDomeMarker>>,
    spawned: Query<(), Added<SkyDomeMarker>>,
    mut materials: ResMut<Assets<SkyDomeMaterial>>,
) {
    let Some(world) = terrain_world else {
        return;
    };
    if !world.is_changed() && spawned.is_empty() {
        return;
    }
    let colors = SkyColors::from_environment(&world.metadata.environment);
    for handle in &domes {
        if let Some(mut material) = materials.get_mut(handle.id())
            && material.colors != colors
        {
            material.colors = colors;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine_api::toon_curve::luminance;

    fn lum4(v: Vec4) -> f32 {
        luminance(v.truncate())
    }

    /// The whole point: the dome tracks the lights. Dim the ambient and the
    /// sky dims; set the sun under the horizon and the haze it lit goes with
    /// it. A night scene must be able to go dark.
    #[test]
    fn the_sky_follows_the_ambient_and_the_sun() {
        let day = SkyColors::default();
        let night = SkyColors::from_environment(&EnvironmentData {
            ambient_intensity: 0.02,
            sun_intensity: 0.0,
            ..default()
        });
        assert!(lum4(night.zenith) < lum4(day.zenith) * 0.2, "{night:?}");
        assert!(lum4(night.horizon) < lum4(day.horizon) * 0.2, "{night:?}");

        // Same lights, sun below the horizon: the haze it lent the horizon
        // is gone, the sky colour itself is unchanged.
        let mut set = EnvironmentData::default();
        let rot = Quat::from_rotation_arc(Vec3::NEG_Z, Vec3::new(0.3, 0.5, 0.8).normalize());
        set.sun_rotation = [rot.x, rot.y, rot.z, rot.w];
        let dusk = SkyColors::from_environment(&set);
        assert!(
            dusk.sun_dir.y < 0.0,
            "sun should be below the horizon: {dusk:?}"
        );
        assert!(lum4(dusk.horizon) < lum4(day.horizon));
        assert_eq!(dusk.zenith, day.zenith);
    }

    /// The sky is displayed through the same exposure as every surface, or
    /// re-exposing a world would move the ground and leave the dome behind.
    #[test]
    fn the_sky_shares_the_toon_curves_exposure() {
        let base = EnvironmentData::default();
        let mut curve = base.toon_curve();
        curve.white *= 2.0;
        let mut darker = base.clone();
        darker.set_toon_curve(curve);
        let (a, b) = (
            SkyColors::from_environment(&base),
            SkyColors::from_environment(&darker),
        );
        assert!((lum4(b.zenith) * 2.0 - lum4(a.zenith)).abs() < 1e-5);
        assert!((lum4(b.sun) * 2.0 - lum4(a.sun)).abs() < 1e-5);
        assert_eq!(a.sun_dir, b.sun_dir);
    }

    /// Shape invariants the shader leans on: the horizon is the bright end
    /// of the gradient, the ground is darker than the sky, and the sun
    /// direction is a unit vector.
    #[test]
    fn the_gradient_is_bright_at_the_horizon_and_dark_underground() {
        let sky = SkyColors::default();
        assert!(lum4(sky.horizon) > lum4(sky.zenith));
        assert!(lum4(sky.ground) < lum4(sky.horizon));
        assert!((sky.sun_dir.truncate().length() - 1.0).abs() < 1e-5);
        assert!(sky.sun.w > 0.0);
    }
}
