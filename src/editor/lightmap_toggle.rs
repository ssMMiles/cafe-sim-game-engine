//! Shift+L: flip every toon and terrain material between baked lighting and
//! a flat, unlit debug view.
//!
//! This is purely an authoring aid for inspecting base colours and UV seams.
//! There is no second lighting path: all light lives in the baked lightmaps
//! and probes, so "off" simply means unlit. Toon materials keep their
//! *source* (lightmap or probe volume) and only flip the `unlit` flag.

use bevy::prelude::*;

use crate::game::gfx::toon_material::ToonMaterial;
use crate::game::world::terrain::material::TerrainMaterial;
use crate::settings::UserSettings;

/// What the terrain fragment shader draws instead of (or over) the painted
/// ground — View menu, editor only. Not persisted: a debug view is a thing
/// you switch on to look at something, not a setting.
#[derive(Resource, Default, Clone, Copy, PartialEq, Eq)]
pub enum TerrainDebugMode {
    #[default]
    Off,
    /// Flat per-layer-id colours (hashed hues), so two layers with similar
    /// tints still read as distinct.
    Layers,
    /// Tint tiles the paint rules own (`PAINT_FLAG_AUTO`) magenta.
    AutoTiles,
}

/// Debug views are authoring aids: entering gameplay switches them off, so a
/// session never plays with layer-id colours on the ground.
pub fn reset_terrain_debug_view(mut mode: ResMut<TerrainDebugMode>) {
    mode.set_if_neq(TerrainDebugMode::Off);
}

/// Write [`TerrainDebugMode`] into every terrain material when it changes.
pub fn sync_terrain_debug_view(
    mode: Res<TerrainDebugMode>,
    mut terrain_materials: ResMut<Assets<TerrainMaterial>>,
) {
    if !mode.is_changed() {
        return;
    }
    let debug = *mode as u32;
    for (_id, mat) in terrain_materials.iter_mut() {
        mat.shading.debug = debug;
    }
}

pub fn toggle_lightmap_mode(input: Res<ButtonInput<KeyCode>>, mut settings: ResMut<UserSettings>) {
    let shift = input.pressed(KeyCode::ShiftLeft) || input.pressed(KeyCode::ShiftRight);
    if shift && input.just_pressed(KeyCode::KeyL) {
        settings.toggle_use_lightmap();
    }
}

pub fn sync_lightmap_mode(
    settings: Res<UserSettings>,
    mut toon_materials: ResMut<Assets<ToonMaterial>>,
    mut terrain_materials: ResMut<Assets<TerrainMaterial>>,
) {
    if !settings.is_changed() {
        return;
    }

    let use_lm = if settings.use_lightmap { 1u32 } else { 0u32 };

    for (_id, mat) in toon_materials.iter_mut() {
        mat.shading.unlit = 1 - use_lm;
    }

    for (_id, mat) in terrain_materials.iter_mut() {
        mat.shading.use_lightmap = use_lm;
    }
}
