use bevy::prelude::*;

pub use engine_api::{CameraView, InputLock, MovementMode, UserSettings};

// Setting value ranges
pub mod setting_ranges {
    pub const THIRD_PERSON_DISTANCE: (f32, f32) = (5.0, 50.0);
}

pub struct SettingsPlugin;

impl Plugin for SettingsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<UserSettings>();
        app.init_resource::<InputLock>();
    }
}
