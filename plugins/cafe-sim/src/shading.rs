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
//! The ghost preview is the engine's `UnlitMaterial` (translucent, unlit, the
//! scene pass's transparent layer); the shop thumbnails are a flat toon
//! colour. Neither is lit — bevy's PBR isn't in the app at all.

use bevy::{platform::collections::HashMap, prelude::*};
use engine_api::{DefaultToonLightmap, ProbeVolumeImages, ToonMaterial, ToonSource};

/// Shared toon materials by colour: probe-lit ones and flat (unlit) ones.
#[derive(Resource, Default)]
pub struct ProbeLitColors {
    probe_lit: HashMap<[u8; 4], Handle<ToonMaterial>>,
    flat: HashMap<[u8; 4], Handle<ToonMaterial>>,
}

/// The probe-lit toon material for `color`, building it on first use.
pub fn probe_lit(
    color: Color,
    cache: &mut ProbeLitColors,
    materials: &mut Assets<ToonMaterial>,
    lightmap: &DefaultToonLightmap,
    probes: &ProbeVolumeImages,
) -> Handle<ToonMaterial> {
    cache
        .probe_lit
        .entry(color.to_srgba().to_u8_array())
        .or_insert_with(|| {
            materials.add(
                ToonMaterial::new(ToonSource::ProbeVolume, lightmap.0.clone(), probes)
                    .with_base_color(color),
            )
        })
        .clone()
}

/// The flat, unlit toon material for `color` — exactly the colour, no
/// shading — building it on first use. For the shop thumbnails, which have no
/// lights and want to read as swatches.
pub fn flat(
    color: Color,
    cache: &mut ProbeLitColors,
    materials: &mut Assets<ToonMaterial>,
    lightmap: &DefaultToonLightmap,
    probes: &ProbeVolumeImages,
) -> Handle<ToonMaterial> {
    cache
        .flat
        .entry(color.to_srgba().to_u8_array())
        .or_insert_with(|| {
            let mut material = ToonMaterial::new(ToonSource::Lightmap, lightmap.0.clone(), probes)
                .with_base_color(color);
            material.shading.unlit = 1;
            materials.add(material)
        })
        .clone()
}

/// [`probe_lit`] for exclusive-`World` setup code. [`ProbeLitColors`] must
/// already be a resource.
pub fn probe_lit_in(world: &mut World, color: Color) -> Handle<ToonMaterial> {
    let lightmap = world.resource::<DefaultToonLightmap>().clone();
    let probes = world.resource::<ProbeVolumeImages>().clone();
    world.resource_scope(|world, mut cache: Mut<ProbeLitColors>| {
        probe_lit(
            color,
            &mut cache,
            &mut world.resource_mut::<Assets<ToonMaterial>>(),
            &lightmap,
            &probes,
        )
    })
}
