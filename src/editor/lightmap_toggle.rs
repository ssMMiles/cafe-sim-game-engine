//! Shift+L: flip every toon and terrain material between baked lighting and
//! a flat, unlit debug view.
//!
//! This is purely an authoring aid for inspecting base colours and UV seams.
//! There is no second lighting path: all light (sun, ambient, and soon
//! point/spot) lives in the baked lightmaps, so "off" simply means unlit.

use bevy::{pbr::ExtendedMaterial, prelude::*};

use crate::game::gfx::toon_material::ToonMaterial;
use crate::game::world::terrain::material::TerrainMaterial;
use crate::settings::UserSettings;

pub fn toggle_lightmap_mode(input: Res<ButtonInput<KeyCode>>, mut settings: ResMut<UserSettings>) {
    let shift = input.pressed(KeyCode::ShiftLeft) || input.pressed(KeyCode::ShiftRight);
    if shift && input.just_pressed(KeyCode::KeyL) {
        settings.toggle_use_lightmap();
    }
}

pub fn sync_lightmap_mode(
    settings: Res<UserSettings>,
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
}
