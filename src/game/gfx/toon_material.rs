//! Host-side half of the toon material: the shared resources every toon
//! material binds, and the systems that keep their sun uniform in step with
//! the level.
//!
//! The types themselves live in `engine-api` so plugin dylibs can build toon
//! materials (`ToonMaterial::new`) against the same `TypeId`s.

pub use engine_api::toon_material::{
    DefaultToonLightmap, ProbeVolumeImages, ToonMaterial, ToonSource, create_default_toon_lightmap,
    create_probe_volume_images,
};

pub use engine_api::toon_curve::ToonCurve;

use bevy::{prelude::*, render::RenderApp, render::extract_resource::ExtractResource};

use crate::game::world::terrain::storage::{EnvironmentData, TerrainWorld};

/// Everything a toon surface needs from the level that isn't baked into its
/// lightmap: the sun (which probe-lit surfaces evaluate per pixel) and the
/// [`ToonCurve`] (which every surface is quantised through).
///
/// One resource for both because they must reach *every* toon surface,
/// terrain and objects alike, in the same frame: a curve that differed
/// between the two would draw a step along every place they meet. It reaches
/// them as the scene pass's per-scene uniform (`scene_pass::SceneUniform`,
/// extracted from this each frame) — not by writing materials, which under
/// the scene pass would rebuild every material bind group on each edit.
#[derive(Resource, Debug, Clone, Copy, PartialEq)]
pub struct ToonLook {
    /// Unit direction *toward* the sun.
    pub sun_dir: Vec3,
    /// Sun colour × intensity, in lightmap units.
    pub sun_rgb: Vec3,
    pub curve: ToonCurve,
}

impl Default for ToonLook {
    fn default() -> Self {
        Self::from_environment(&EnvironmentData::default())
    }
}

impl ToonLook {
    pub fn from_environment(env: &EnvironmentData) -> Self {
        Self {
            sun_dir: -env.sun_transform().forward().as_vec3(),
            sun_rgb: env.sun_radiance(),
            curve: env.toon_curve(),
        }
    }
}

impl ExtractResource<RenderApp> for ToonLook {
    type Source = Self;
    fn extract_resource(source: &Self) -> Self {
        *source
    }
}

/// Follow the level's environment into [`ToonLook`].
pub fn update_toon_look(terrain_world: Option<Res<TerrainWorld>>, mut look: ResMut<ToonLook>) {
    let Some(world) = terrain_world else {
        return;
    };
    if !world.is_changed() {
        return;
    }
    let next = ToonLook::from_environment(&world.metadata.environment);
    if *look != next {
        *look = next;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const OBJECT_SHADER: &str = include_str!("../../../assets/shaders/toon_lighting.wesl");
    const TERRAIN_SHADER: &str = include_str!("../../../assets/shaders/terrain_toon.wesl");
    const PREPASS_SHADER: &str = include_str!("../../../assets/shaders/toon_prepass.wesl");

    /// `fn facing_normal` through its closing brace.
    fn facing_normal(src: &str) -> Option<&str> {
        let start = src.find("fn facing_normal(")?;
        let end = src[start..].find("\n}\n")? + start;
        Some(&src[start..end])
    }

    /// The colour pass and the normal prepass each flip a double-sided back
    /// face's normal, with their own copy of the function. If they disagree,
    /// probe lighting and outlines describe different surfaces.
    #[test]
    fn both_passes_flip_normals_the_same_way() {
        let colour = facing_normal(OBJECT_SHADER).expect("toon_lighting.wesl has facing_normal");
        let prepass = facing_normal(PREPASS_SHADER).expect("toon_prepass.wesl has facing_normal");
        assert_eq!(
            colour, prepass,
            "facing_normal has drifted between the passes"
        );
        assert!(colour.contains(&format!("@if({})", engine_api::TOON_DOUBLE_SIDED_DEF)));
    }

    const BEGIN: &str = "// ─── TOON CURVE";
    const END: &str = "// ─── END TOON CURVE";

    /// The toon curve between the markers, or `None` if a marker is missing.
    fn curve_block(src: &str) -> Option<&str> {
        let start = src.find(BEGIN)?;
        let end = src[start..].find(END)? + start;
        Some(&src[start..end])
    }

    /// The transfer function exists three times — twice in WESL (asset-path
    /// shaders have no shared import module) and once in Rust so it can be
    /// tested and tabulated. Terrain and the objects standing on it meet
    /// along every contact shadow, so a ramp that differs between them draws
    /// a step right at the join, and a Rust copy that has drifted makes
    /// every number in docs/LIGHTING.md a lie.
    #[test]
    fn toon_curve_matches_the_shaders() {
        let object = curve_block(OBJECT_SHADER).expect("toon_lighting.wesl has the curve markers");
        let terrain = curve_block(TERRAIN_SHADER).expect("terrain_toon.wesl has the curve markers");
        assert_eq!(
            object, terrain,
            "the toon curve block has drifted between the two shaders"
        );

        // The WESL has no constants left to compare — the curve travels in
        // the scene uniform — so what must match is the *shape*: every operation
        // the Rust mirror performs has to appear in the shader.
        for needle in [
            "pow(clamp(lum / toon_white(curve), 0.0, 1.0), inv_gamma)",
            "pow(clamp(toon_floor(curve), 0.0, 1.0), inv_gamma)",
            "pow(floor_x + (1.0 - floor_x) * band, toon_gamma(curve))",
            "smoothstep(0.0, 1.0 / bands, band)",
            "vec3<f32>(0.2126, 0.7152, 0.0722)",
        ] {
            assert!(
                object.contains(needle),
                "the shader no longer performs `{needle}`; \
                 engine-api's ToonCurve mirrors it and must change too"
            );
        }
    }

    /// `ToonLook` is the one place a world's ramp reaches the GPU, and it
    /// must sanitise: the values arrive from an inspector drag and from
    /// disk, and a zero `bands` would divide by zero in the neutral fade.
    #[test]
    fn the_look_sanitises_what_it_reads_from_the_level() {
        let env = EnvironmentData {
            toon_curve: [0.0, -1.0, 0.0, 0.0],
            ..default()
        };
        let look = ToonLook::from_environment(&env);
        assert!(look.curve.bands >= 1.0);
        assert!(look.curve.floor >= 0.0);
        assert!(look.curve.white > 0.0);
        assert!(look.curve.gamma > 0.0);
    }
}
