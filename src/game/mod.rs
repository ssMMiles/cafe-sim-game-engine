use bevy::{diagnostic::{FrameTimeDiagnosticsPlugin, LogDiagnosticsPlugin}, pbr::ExtendedMaterial, prelude::*};

use crate::{
    AppState, game::{crosshair::{toggle_crosshair, update_crosshair_display}, gfx::GfxPlugin, interaction::{handle_interactable_clicks, show_interaction_text}, world::WorldPlugin}, settings::{EdgeDepthSensitivitySetting, EdgeNormalSensitivitySetting, MouseSensitivitySetting, MovementSpeedSetting, UserSettings}
};

pub mod camera;
mod animation;
mod crosshair;
mod icons;
mod interaction;
mod materials;
pub mod pause_menu;
pub mod physics;
mod simulation;
mod ui_displays;
pub mod world;
mod gfx;

use animation::{init_animation_player, pause_animations, resume_animations, setup_character_animations, update_character_animation};
use camera::{adjust_camera_distance, apply_freecam_movement, camera_follow, handle_camera_input, resize_render_texture_sprite};
use physics::character_physics;
use icons::SvgTextureCache;
use interaction::{InteractableClicked, check_interactions, detect_interactable_clicks};
use pause_menu::{
    PauseMenuState, cleanup_pause_menu, cleanup_pause_settings, handle_pause_input,
    handle_pause_menu, handle_settings_menu, handle_slider, setup_pause_menu, setup_pause_settings,
    update_slider_label,
};
use simulation::{
    exit, setup
};
use ui_displays::{
    setup_fps_display, setup_interaction_info_display, setup_keybinds_display,
    setup_position_display, update_dynamic_backgrounds, update_fps_display,
    update_interaction_info_display, update_keybinds_display, update_position_display,
};

pub struct SimulationPlugin;

#[derive(Default, Clone, Copy, PartialEq, Eq, Hash, Debug, States)]
pub enum SimulationState {
    #[default]
    Running,
    Paused,
}

#[derive(Resource, Debug)]
pub struct SimulationConfig {
    pub speed: f32,
}

impl Default for SimulationConfig {
    fn default() -> Self {
        SimulationConfig { speed: 1.0 }
    }
}

#[derive(Component)]
pub struct GameEntity;

fn edit_mode_active(settings: Res<UserSettings>) -> bool {
    settings.edit_mode
}

impl Plugin for SimulationPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(WorldPlugin);
        app.add_plugins(GfxPlugin);
        app.add_plugins(FrameTimeDiagnosticsPlugin::default());
        app.add_plugins(LogDiagnosticsPlugin::default());
        app.add_message::<InteractableClicked>();
        app.add_systems(
            OnEnter(AppState::InGame),
            (
                setup,
                setup_keybinds_display,
                setup_fps_display,
                setup_interaction_info_display,
                setup_position_display,
                setup_character_animations.after(setup),
            ),
        );
        app.add_systems(OnExit(AppState::InGame), exit);

        app.add_systems(
            Update,
            (
                handle_camera_input,
                apply_freecam_movement,
                character_physics,
                camera_follow,
                update_character_animation,
            )
                .chain()
                .run_if(in_state(AppState::InGame).and(in_state(SimulationState::Running))),
        );
        app.add_systems(
            Update,
            init_animation_player.run_if(in_state(AppState::InGame)),
        );
        app.add_systems(
            Update,
            resize_render_texture_sprite.run_if(in_state(AppState::InGame)),
        );
        app.add_systems(
            Update,
            adjust_camera_distance.run_if(in_state(AppState::InGame)),
        );
        app.add_systems(
            Update,
            (check_interactions, detect_interactable_clicks)
                .run_if(in_state(AppState::InGame).and(in_state(SimulationState::Running))),
        );
        app.add_systems(
            Update,
            show_interaction_text
                .run_if(in_state(AppState::InGame).and(in_state(SimulationState::Running))),
        );
        app.add_systems(Update, toggle_crosshair.run_if(in_state(AppState::InGame)));
        app.add_systems(
            Update,
            update_crosshair_display.run_if(in_state(AppState::InGame)),
        );
        app.add_systems(
            Update,
            update_keybinds_display.run_if(in_state(AppState::InGame)),
        );
        app.add_systems(
            Update,
            update_fps_display.run_if(in_state(AppState::InGame)),
        );
        app.add_systems(
            Update,
            update_interaction_info_display.run_if(in_state(AppState::InGame)),
        );
        app.add_systems(
            Update,
            update_position_display.run_if(in_state(AppState::InGame)),
        );
        // Interactable and menu systems
        app.add_systems(
            Update,
            (
                handle_interactable_clicks,
            )
                .run_if(in_state(AppState::InGame).and(in_state(SimulationState::Running))),
        );
        // Dynamic background system - runs less frequently to avoid issues
        app.add_systems(
            FixedUpdate,
            update_dynamic_backgrounds
                .run_if(in_state(AppState::InGame).and(in_state(SimulationState::Running))),
        );
        app.add_systems(
            Update,
            handle_pause_input.run_if(in_state(AppState::InGame)),
        );

        app.add_systems(OnEnter(SimulationState::Paused), (setup_pause_menu, pause_animations));
        app.add_systems(OnExit(SimulationState::Paused), (cleanup_pause_menu, resume_animations));
        app.add_systems(
            Update,
            handle_pause_menu
                .run_if(in_state(SimulationState::Paused).and(in_state(PauseMenuState::Main))),
        );

        app.add_systems(
            OnEnter(PauseMenuState::Settings),
            (cleanup_pause_menu, setup_pause_settings),
        );
        app.add_systems(OnExit(PauseMenuState::Settings), cleanup_pause_settings);
        app.add_systems(
            OnEnter(PauseMenuState::Main),
            (cleanup_pause_settings, setup_pause_menu).run_if(in_state(SimulationState::Paused)),
        );
        app.add_systems(
            Update,
            handle_settings_menu
                .run_if(in_state(SimulationState::Paused).and(in_state(PauseMenuState::Settings))),
        );
        app.add_systems(
            Update,
            (
                handle_slider::<MouseSensitivitySetting>,
                handle_slider::<MovementSpeedSetting>,
                handle_slider::<EdgeDepthSensitivitySetting>,
                handle_slider::<EdgeNormalSensitivitySetting>,
                update_slider_label::<MouseSensitivitySetting>,
                update_slider_label::<MovementSpeedSetting>,
                update_slider_label::<EdgeDepthSensitivitySetting>,
                update_slider_label::<EdgeNormalSensitivitySetting>,
            )
                .run_if(in_state(SimulationState::Paused).and(in_state(PauseMenuState::Settings))),
        );

        app.init_state::<SimulationState>();
        app.init_state::<PauseMenuState>();
        app.init_resource::<SimulationConfig>();
        app.init_resource::<SvgTextureCache>();
    }
}
