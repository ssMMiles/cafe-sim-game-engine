use bevy::{
    diagnostic::{FrameTimeDiagnosticsPlugin, LogDiagnosticsPlugin},
    pbr::ExtendedMaterial,
    prelude::*,
};

use crate::{
    AppState,
    game::world::ToonMaterial,
    settings::{MouseSensitivitySetting, MovementSpeedSetting, UserSettings},
};

pub mod camera;
mod cutting_board;
mod day_night;
mod edit_mode;
mod icons;
mod interaction;
mod materials;
mod outline;
pub mod pause_menu;
pub mod physics;
mod post_process;
mod simulation;
mod ui_displays;
mod world;

use camera::{
    adjust_camera_distance, apply_freecam_movement, camera_follow, handle_camera_input,
    resize_render_texture_sprite,
};
use cutting_board::{
    ActiveCuttingBoardMenu, animate_card_hover, check_menu_distances, update_card_shadows,
    update_cutting_board_menu_rotation,
};
use day_night::{TimeOfDay, update_sun_properties, update_sun_transform, update_time_of_day};
use edit_mode::{
    PlacementPreviewEntity, cleanup_placement_preview, handle_edit_mode_click,
    setup_placement_preview, toggle_edit_mode, toggle_placement_object, update_placement_preview,
};
use icons::SvgTextureCache;
use interaction::{InteractableClicked, check_interactions, detect_interactable_clicks};
use outline::{toggle_outline, update_outline};
use pause_menu::{
    PauseMenuState, cleanup_pause_menu, cleanup_pause_settings, handle_pause_input,
    handle_pause_menu, handle_settings_menu, handle_slider, setup_pause_menu, setup_pause_settings,
    update_slider_label,
};
use physics::character_physics;
use post_process::{EdgeDetectionPlugin, sync_edge_detection_settings, toggle_edge_detection};
use simulation::{
    display_cooking_progress, exit, handle_interactable_clicks, setup, show_interaction_text,
    toggle_crosshair, update_cooking_display_rotation, update_cooking_displays,
    update_cooking_timers, update_crosshair_display,
};
use ui_displays::{
    setup_fps_display, setup_interaction_info_display, setup_keybinds_display,
    setup_position_display, setup_time_display, update_dynamic_backgrounds, update_fps_display,
    update_interaction_info_display, update_keybinds_display, update_position_display,
    update_time_display,
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
        app.add_plugins(FrameTimeDiagnosticsPlugin::default());
        app.add_plugins(LogDiagnosticsPlugin::default());
        app.add_plugins(MaterialPlugin::<
            ExtendedMaterial<StandardMaterial, ToonMaterial>,
        >::default());
        app.add_plugins(EdgeDetectionPlugin);
        app.add_message::<InteractableClicked>();
        app.add_systems(
            OnEnter(AppState::InGame),
            (
                setup,
                setup_keybinds_display,
                setup_fps_display,
                setup_time_display,
                setup_interaction_info_display,
                setup_position_display,
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
            )
                .chain()
                .run_if(in_state(AppState::InGame).and(in_state(SimulationState::Running))),
        );
        app.add_systems(
            Update,
            (
                update_time_of_day,
                update_sun_transform,
                update_sun_properties,
            )
                .run_if(in_state(AppState::InGame).and(in_state(SimulationState::Running))),
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
            handle_edit_mode_click.run_if(
                edit_mode_active
                    .and(in_state(AppState::InGame))
                    .and(in_state(SimulationState::Running)),
            ),
        );
        app.add_systems(
            Update,
            show_interaction_text
                .run_if(in_state(AppState::InGame).and(in_state(SimulationState::Running))),
        );
        app.add_systems(
            Update,
            update_outline
                .run_if(in_state(AppState::InGame).and(in_state(SimulationState::Running))),
        );
        app.add_systems(Update, toggle_crosshair.run_if(in_state(AppState::InGame)));
        app.add_systems(Update, toggle_outline.run_if(in_state(AppState::InGame)));
        app.add_systems(Update, toggle_edit_mode.run_if(in_state(AppState::InGame)));
        app.add_systems(
            Update,
            toggle_edge_detection.run_if(in_state(AppState::InGame)),
        );
        app.add_systems(
            Update,
            sync_edge_detection_settings.run_if(in_state(AppState::InGame)),
        );
        app.add_systems(
            Update,
            toggle_placement_object.run_if(in_state(AppState::InGame)),
        );
        app.add_systems(
            Update,
            setup_placement_preview.run_if(in_state(AppState::InGame)),
        );
        app.add_systems(
            Update,
            cleanup_placement_preview.run_if(in_state(AppState::InGame)),
        );
        app.add_systems(
            Update,
            update_placement_preview
                .run_if(in_state(AppState::InGame).and(in_state(SimulationState::Running))),
        );
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
            update_time_display.run_if(in_state(AppState::InGame)),
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
                update_cutting_board_menu_rotation,
                animate_card_hover,
                update_card_shadows,
                check_menu_distances,
                update_cooking_timers,
                display_cooking_progress,
                update_cooking_displays,
                update_cooking_display_rotation,
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

        app.add_systems(OnEnter(SimulationState::Paused), setup_pause_menu);
        app.add_systems(OnExit(SimulationState::Paused), cleanup_pause_menu);
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
                update_slider_label::<MouseSensitivitySetting>,
                update_slider_label::<MovementSpeedSetting>,
            )
                .run_if(in_state(SimulationState::Paused).and(in_state(PauseMenuState::Settings))),
        );

        app.init_state::<SimulationState>();
        app.init_state::<PauseMenuState>();
        app.init_resource::<SimulationConfig>();
        app.init_resource::<TimeOfDay>();
        app.init_resource::<PlacementPreviewEntity>();
        app.init_resource::<ActiveCuttingBoardMenu>();
        app.init_resource::<SvgTextureCache>();
    }
}
