use bevy::prelude::*;

pub use engine_api::{CameraView, MovementMode, SettingInfo, UserSettings};

// UI constants for slider system
pub mod slider_constants {
    pub const TRACK_WIDTH: f32 = 300.0;
    pub const HANDLE_WIDTH: f32 = 20.0;
}

// Setting value ranges
pub mod setting_ranges {
    pub const MOUSE_SENSITIVITY: (f32, f32) = (0.001, 0.01);
    pub const MOVEMENT_SPEED: (f32, f32) = (0.05, 5.0);
    pub const THIRD_PERSON_DISTANCE: (f32, f32) = (5.0, 50.0);
    pub const EDGE_DEPTH_SENSITIVITY: (f32, f32) = (0.1, 10.0);
    pub const EDGE_NORMAL_SENSITIVITY: (f32, f32) = (0.1, 10.0);
}

// Trait for abstracting slider setting access
pub trait SliderSetting {
    fn get_value(settings: &UserSettings) -> f32;
    fn set_value(settings: &mut UserSettings, value: f32);
    fn get_range() -> (f32, f32);
    fn get_label() -> &'static str;
    fn format_value(value: f32) -> String {
        format!("{value:.1}")
    }
}

pub struct SettingsPlugin;

impl Plugin for SettingsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<UserSettings>();
    }
}

// Marker types for different slider settings
#[derive(Component)]
pub struct MouseSensitivitySetting;

#[derive(Component)]
pub struct MovementSpeedSetting;

// Trait implementations for each setting type
impl SliderSetting for MouseSensitivitySetting {
    fn get_value(settings: &UserSettings) -> f32 {
        settings.mouse_sensitivity
    }

    fn set_value(settings: &mut UserSettings, value: f32) {
        settings.mouse_sensitivity = value;
    }

    fn get_range() -> (f32, f32) {
        setting_ranges::MOUSE_SENSITIVITY
    }

    fn get_label() -> &'static str {
        "Mouse Sensitivity"
    }

    fn format_value(value: f32) -> String {
        format!("{value:.3}")
    }
}

impl SliderSetting for MovementSpeedSetting {
    fn get_value(settings: &UserSettings) -> f32 {
        settings.movement_speed
    }

    fn set_value(settings: &mut UserSettings, value: f32) {
        settings.movement_speed = value;
    }

    fn get_range() -> (f32, f32) {
        setting_ranges::MOVEMENT_SPEED
    }

    fn get_label() -> &'static str {
        "Movement Speed"
    }

    fn format_value(value: f32) -> String {
        format!("{value:.2}")
    }
}

#[derive(Component)]
pub struct EdgeDepthSensitivitySetting;

impl SliderSetting for EdgeDepthSensitivitySetting {
    fn get_value(settings: &UserSettings) -> f32 {
        settings.edge_depth_sensitivity
    }

    fn set_value(settings: &mut UserSettings, value: f32) {
        settings.edge_depth_sensitivity = value;
    }

    fn get_range() -> (f32, f32) {
        setting_ranges::EDGE_DEPTH_SENSITIVITY
    }

    fn get_label() -> &'static str {
        "Edge Depth Sensitivity"
    }
}

#[derive(Component)]
pub struct EdgeNormalSensitivitySetting;

impl SliderSetting for EdgeNormalSensitivitySetting {
    fn get_value(settings: &UserSettings) -> f32 {
        settings.edge_normal_sensitivity
    }

    fn set_value(settings: &mut UserSettings, value: f32) {
        settings.edge_normal_sensitivity = value;
    }

    fn get_range() -> (f32, f32) {
        setting_ranges::EDGE_NORMAL_SENSITIVITY
    }

    fn get_label() -> &'static str {
        "Edge Normal Sensitivity"
    }
}
