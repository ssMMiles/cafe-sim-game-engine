use bevy::prelude::*;

// UI constants for slider system
pub mod slider_constants {
    pub const TRACK_WIDTH: f32 = 300.0;
    pub const HANDLE_WIDTH: f32 = 20.0;
    pub const TRACK_HEIGHT: f32 = 20.0;
}

// Setting value ranges
pub mod setting_ranges {
    pub const MOUSE_SENSITIVITY: (f32, f32) = (0.001, 0.01);
    pub const MOVEMENT_SPEED: (f32, f32) = (0.05, 0.3);
    pub const THIRD_PERSON_DISTANCE: (f32, f32) = (5.0, 50.0);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlacementObjectType {
    Table,
    Cooker,
    Fridge,
    CuttingBoard,
}

impl Default for PlacementObjectType {
    fn default() -> Self {
        PlacementObjectType::Table
    }
}

impl std::fmt::Display for PlacementObjectType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PlacementObjectType::Table => write!(f, "Table"),
            PlacementObjectType::Cooker => write!(f, "Cooker"),
            PlacementObjectType::Fridge => write!(f, "Fridge"),
            PlacementObjectType::CuttingBoard => write!(f, "Cutting Board"),
        }
    }
}

// Trait for abstracting slider setting access
pub trait SliderSetting {
    fn get_value(settings: &UserSettings) -> f32;
    fn set_value(settings: &mut UserSettings, value: f32);
    fn get_range() -> (f32, f32);
    fn get_label() -> &'static str;
    fn get_format() -> &'static str;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MovementMode {
    FreeCam,
    FloorBased,
}

impl Default for MovementMode {
    fn default() -> Self {
        MovementMode::FloorBased
    }
}

impl std::fmt::Display for MovementMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MovementMode::FreeCam => write!(f, "FreeCam"),
            MovementMode::FloorBased => write!(f, "Floor-Based"),
        }
    }
}

#[derive(Resource, Debug)]
pub struct UserSettings {
    pub crosshair_visible: bool,
    pub movement_mode: MovementMode,
    pub mouse_sensitivity: f32,
    pub movement_speed: f32,
    pub edit_mode: bool,
    pub current_placement_object: PlacementObjectType,
    pub outline_visible: bool,
    pub third_person_view: bool,
    pub third_person_distance: f32,
}

impl Default for UserSettings {
    fn default() -> Self {
        Self {
            crosshair_visible: false,
            movement_mode: MovementMode::default(),
            mouse_sensitivity: 0.002,
            movement_speed: 0.1,
            edit_mode: false,
            current_placement_object: PlacementObjectType::default(),
            outline_visible: false,
            third_person_view: true,
            third_person_distance: 20.0,
        }
    }
}

#[derive(Debug)]
pub struct SettingInfo {
    pub name: &'static str,
    pub keybind: &'static str,
    pub current_value: String,
}

impl UserSettings {
    pub fn get_settings_info(&self) -> Vec<SettingInfo> {
        vec![
            SettingInfo {
                name: "Crosshair",
                keybind: "Shift+T",
                current_value: if self.crosshair_visible { "ON" } else { "OFF" }.to_string(),
            },
            SettingInfo {
                name: "Outline",
                keybind: "Shift+O",
                current_value: if self.outline_visible { "ON" } else { "OFF" }.to_string(),
            },
            SettingInfo {
                name: "Movement Mode",
                keybind: "Shift+M",
                current_value: self.movement_mode.to_string(),
            },
            SettingInfo {
                name: "Edit Mode",
                keybind: "Shift+E",
                current_value: if self.edit_mode { "ON" } else { "OFF" }.to_string(),
            },
            SettingInfo {
                name: "Placement Object",
                keybind: "Shift+R",
                current_value: self.current_placement_object.to_string(),
            },
            SettingInfo {
                name: "Mouse Sensitivity",
                keybind: "",
                current_value: format!("{:.3}", self.mouse_sensitivity),
            },
            SettingInfo {
                name: "Movement Speed",
                keybind: "",
                current_value: format!("{:.2}", self.movement_speed),
            },
            SettingInfo {
                name: "Third Person View",
                keybind: "Shift+V",
                current_value: if self.third_person_view { "ON" } else { "OFF" }.to_string(),
            },
        ]
    }

    pub fn toggle_crosshair(&mut self) {
        self.crosshair_visible = !self.crosshair_visible;
    }

    pub fn toggle_outline(&mut self) {
        self.outline_visible = !self.outline_visible;
    }

    pub fn toggle_movement_mode(&mut self) {
        self.movement_mode = match self.movement_mode {
            MovementMode::FreeCam => MovementMode::FloorBased,
            MovementMode::FloorBased => MovementMode::FreeCam,
        };
    }

    pub fn toggle_edit_mode(&mut self) {
        self.edit_mode = !self.edit_mode;
    }

    pub fn cycle_placement_object(&mut self) {
        self.current_placement_object = match self.current_placement_object {
            PlacementObjectType::Table => PlacementObjectType::Cooker,
            PlacementObjectType::Cooker => PlacementObjectType::Fridge,
            PlacementObjectType::Fridge => PlacementObjectType::CuttingBoard,
            PlacementObjectType::CuttingBoard => PlacementObjectType::Table,
        };
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

    fn get_format() -> &'static str {
        "{:.3}"
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

    fn get_format() -> &'static str {
        "{:.2}"
    }
}
