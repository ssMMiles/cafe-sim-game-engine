use avian3d::debug_render::PhysicsGizmos;
use bevy::prelude::*;

use crate::game::world::objects::LightGizmos;
use crate::settings::UserSettings;

pub fn toggle_physics_debug(input: Res<ButtonInput<KeyCode>>, mut settings: ResMut<UserSettings>) {
    let shift = input.pressed(KeyCode::ShiftLeft) || input.pressed(KeyCode::ShiftRight);
    if shift && input.just_pressed(KeyCode::KeyP) {
        settings.toggle_physics_debug();
    }
}

pub fn sync_physics_debug(settings: Res<UserSettings>, mut store: ResMut<GizmoConfigStore>) {
    if settings.is_changed() {
        store.config_mut::<PhysicsGizmos>().0.enabled = settings.physics_debug;
    }
}

/// F8: show / hide the placed-light markers (also View ▸ Light gizmos).
pub fn toggle_light_gizmos(input: Res<ButtonInput<KeyCode>>, mut store: ResMut<GizmoConfigStore>) {
    if input.just_pressed(KeyCode::F8) {
        let enabled = &mut store.config_mut::<LightGizmos>().0.enabled;
        *enabled = !*enabled;
    }
}
