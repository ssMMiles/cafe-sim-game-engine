//! Host-side half of the toon material: the shared resources every toon
//! material binds, and the systems that keep their sun uniform in step with
//! the level.
//!
//! The types themselves live in `engine-api` so plugin dylibs can build toon
//! materials (`ToonMaterial::new`) against the same `TypeId`s.

pub use engine_api::toon_material::{
    DefaultToonLightmap, ProbeVolumeImages, ToonMaterial, ToonShading, ToonSource,
    create_default_toon_lightmap, create_probe_volume_images,
};

use bevy::{pbr::ExtendedMaterial, prelude::*};

use crate::game::world::terrain::storage::{EnvironmentData, TerrainWorld};

pub type ToonMat = ExtendedMaterial<StandardMaterial, ToonMaterial>;

/// The sun as probe-lit materials evaluate it: direction *toward* the sun
/// and its radiance in lightmap units. Derived from the level's
/// `EnvironmentData` whenever that changes.
#[derive(Resource, Debug, Clone, Copy, PartialEq)]
pub struct ToonSun {
    pub dir: Vec3,
    pub rgb: Vec3,
}

impl Default for ToonSun {
    fn default() -> Self {
        Self::from_environment(&EnvironmentData::default())
    }
}

impl ToonSun {
    pub fn from_environment(env: &EnvironmentData) -> Self {
        Self {
            dir: -env.sun_transform().forward().as_vec3(),
            rgb: env.sun_radiance(),
        }
    }

    pub fn apply(&self, shading: &mut ToonShading) {
        shading.set_sun(self.dir, self.rgb);
    }
}

/// Follow the level's environment into [`ToonSun`].
pub fn update_toon_sun(terrain_world: Option<Res<TerrainWorld>>, mut sun: ResMut<ToonSun>) {
    let Some(world) = terrain_world else {
        return;
    };
    if !world.is_changed() {
        return;
    }
    let next = ToonSun::from_environment(&world.metadata.environment);
    if *sun != next {
        *sun = next;
    }
}

/// Write the sun into every toon material when it changes, and into any
/// material added since (a plugin building its own gets it a frame late).
///
/// Only `Added` events are watched, never `Modified`: writing a material
/// here raises `Modified`, so reacting to that would loop every frame.
pub fn sync_toon_sun(
    sun: Res<ToonSun>,
    mut events: MessageReader<AssetEvent<ToonMat>>,
    mut materials: ResMut<Assets<ToonMat>>,
) {
    if sun.is_changed() {
        events.clear();
        for (_, mat) in materials.iter_mut() {
            sun.apply(&mut mat.extension.shading);
        }
        return;
    }
    for event in events.read() {
        if let AssetEvent::Added { id } = event
            && let Some(mut mat) = materials.get_mut(*id)
        {
            sun.apply(&mut mat.extension.shading);
        }
    }
}
