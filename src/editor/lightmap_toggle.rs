use bevy::{pbr::ExtendedMaterial, prelude::*};

use crate::game::gfx::toon_material::ToonMaterial;
use crate::game::world::baking::BakedSun;
use crate::game::world::terrain::material::TerrainMaterial;
use crate::settings::UserSettings;

pub fn toggle_lightmap_mode(
    input: Res<ButtonInput<KeyCode>>,
    mut settings: ResMut<UserSettings>,
) {
    let shift = input.pressed(KeyCode::ShiftLeft) || input.pressed(KeyCode::ShiftRight);
    if shift && input.just_pressed(KeyCode::KeyL) {
        settings.toggle_use_lightmap();
    }
}

pub fn sync_lightmap_mode(
    settings: Res<UserSettings>,
    mut commands: Commands,
    sun_query: Query<(Entity, Has<DirectionalLight>), With<BakedSun>>,
    mut toon_materials: ResMut<Assets<ExtendedMaterial<StandardMaterial, ToonMaterial>>>,
    mut terrain_materials: ResMut<Assets<ExtendedMaterial<StandardMaterial, TerrainMaterial>>>,
) {
    if !settings.is_changed() {
        return;
    }

    let use_lm = if settings.use_lightmap { 1u32 } else { 0u32 };

    for (_id, mat) in toon_materials.iter_mut() {
        mat.extension.use_lightmap = use_lm;
    }

    for (_id, mat) in terrain_materials.iter_mut() {
        mat.extension.use_lightmap = use_lm;
    }

    for (entity, has_light) in &sun_query {
        if settings.use_lightmap {
            if has_light {
                commands.entity(entity).remove::<DirectionalLight>();
            }
        } else if !has_light {
            commands.entity(entity).insert(DirectionalLight {
                illuminance: 10_000.0,
                shadows_enabled: true,
                ..default()
            });
        }
    }
}
