use bevy::prelude::*;

#[cfg(feature = "physics_debug")]
use avian3d::debug_render::PhysicsGizmos;

use crate::settings::UserSettings;

pub fn toggle_physics_debug(input: Res<ButtonInput<KeyCode>>, mut settings: ResMut<UserSettings>) {
    let shift = input.pressed(KeyCode::ShiftLeft) || input.pressed(KeyCode::ShiftRight);
    if shift && input.just_pressed(KeyCode::KeyP) {
        settings.toggle_physics_debug();
    }
}

#[cfg(feature = "physics_debug")]
pub fn sync_physics_debug(settings: Res<UserSettings>, mut store: ResMut<GizmoConfigStore>) {
    if settings.is_changed() {
        store.config_mut::<PhysicsGizmos>().0.enabled = settings.physics_debug;
    }
}

#[cfg(not(feature = "physics_debug"))]
pub fn sync_physics_debug(_settings: Res<UserSettings>, _store: ResMut<GizmoConfigStore>) {}
