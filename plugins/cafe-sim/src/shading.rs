//! How the things this plugin spawns are lit.
//!
//! Everything cafe-sim puts in the world exists only while a game runs, so
//! none of it is in an editor bake: no lightmap of its own, no baked shadow
//! on the floor under it. It is lit instead from the engine's irradiance
//! probe volume at its own position, through the same toon shading the
//! level uses, so a customer under a placed lamp looks lit by it and a
//! table matches the room around it. (`docs/PLUGIN_LIGHTING_PLAN.md`.)
//!
//! - glTF pieces go through the host's conversion: `(ConvertToToon,
//!   DynamicLit)` on the scene root, see [`crate::placement::spawn_piece`].
//! - Tinted pieces and the plain cubes (customers, staff, drinks) build
//!   their toon material here, from a colour. One material per colour,
//!   shared — a probe-lit material carries nothing per instance, and
//!   same-mesh-same-material instances batch into one draw.
//!
//! The ghost preview stays `StandardMaterial`: the toon extension forces
//! opaque, so a translucent toon ghost is not possible.

use bevy::{pbr::ExtendedMaterial, platform::collections::HashMap, prelude::*};
use engine_api::{DefaultToonLightmap, ProbeVolumeImages, ToonMat, ToonMaterial, ToonSource};

/// Shared probe-lit toon materials by colour.
#[derive(Resource, Default)]
pub struct ProbeLitColors(HashMap<[u8; 4], Handle<ToonMat>>);

/// The probe-lit toon material for `color`, building it on first use.
pub fn probe_lit(
    color: Color,
    cache: &mut ProbeLitColors,
    materials: &mut Assets<ToonMat>,
    lightmap: &DefaultToonLightmap,
    probes: &ProbeVolumeImages,
) -> Handle<ToonMat> {
    cache
        .0
        .entry(color.to_srgba().to_u8_array())
        .or_insert_with(|| {
            materials.add(ExtendedMaterial {
                base: StandardMaterial {
                    base_color: color,
                    ..default()
                },
                extension: ToonMaterial::new(ToonSource::ProbeVolume, lightmap.0.clone(), probes),
            })
        })
        .clone()
}

/// [`probe_lit`] for exclusive-`World` setup code. [`ProbeLitColors`] must
/// already be a resource.
pub fn probe_lit_in(world: &mut World, color: Color) -> Handle<ToonMat> {
    let lightmap = world.resource::<DefaultToonLightmap>().clone();
    let probes = world.resource::<ProbeVolumeImages>().clone();
    world.resource_scope(|world, mut cache: Mut<ProbeLitColors>| {
        probe_lit(
            color,
            &mut cache,
            &mut world.resource_mut::<Assets<ToonMat>>(),
            &lightmap,
            &probes,
        )
    })
}
