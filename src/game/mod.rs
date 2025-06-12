use bevy::{diagnostic::FrameTimeDiagnosticsPlugin, prelude::*};

use crate::{
    AppState,
    settings::{MouseSensitivitySetting, MovementSpeedSetting, UserSettings},
};

mod camera;
pub mod pause_menu;
mod simulation;

use camera::handle_movement;
use pause_menu::{
    PauseMenuState, cleanup_pause_menu, cleanup_pause_settings, handle_pause_input,
    handle_pause_menu, handle_settings_menu, handle_slider, setup_pause_menu, setup_pause_settings,
    update_slider_label,
};
use simulation::{
    check_interactions, cleanup_placement_preview, exit, handle_edit_mode_click,
    propagate_interactable_to_children, setup, setup_fps_display, setup_interaction_info_display,
    setup_keybinds_display, setup_placement_preview, setup_position_display, show_interaction_text, toggle_crosshair,
    toggle_edit_mode, toggle_placement_object, update, update_crosshair_display, update_fps_display,
    update_interaction_info_display, update_keybinds_display, update_outline,
    update_placement_preview, update_position_display, PlacementPreviewEntity,
    handle_cutting_board_interaction, update_cutting_board_menu_rotation,
    ActiveCuttingBoardMenu,
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
struct GameEntity;

fn edit_mode_active(settings: Res<UserSettings>) -> bool {
    settings.edit_mode
}

impl Plugin for SimulationPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(FrameTimeDiagnosticsPlugin::default());
        app.add_systems(
            OnEnter(AppState::InGame),
            (
                setup,
                setup_keybinds_display,
                setup_fps_display,
                setup_interaction_info_display,
                setup_position_display,
            ),
        );
        app.add_systems(OnExit(AppState::InGame), exit);

        app.add_systems(
            FixedUpdate,
            update.run_if(in_state(SimulationState::Running).and(in_state(AppState::InGame))),
        );

        app.add_systems(
            Update,
            handle_movement
                .run_if(in_state(AppState::InGame).and(in_state(SimulationState::Running))),
        );
        app.add_systems(
            Update,
            check_interactions
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
        app.add_systems(Update, toggle_edit_mode.run_if(in_state(AppState::InGame)));
        app.add_systems(Update, toggle_placement_object.run_if(in_state(AppState::InGame)));
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
            update_interaction_info_display.run_if(in_state(AppState::InGame)),
        );
        app.add_systems(
            Update,
            update_position_display.run_if(in_state(AppState::InGame)),
        );
        // Propagate interactable to mesh children
        app.add_systems(
            Update,
            propagate_interactable_to_children.run_if(in_state(AppState::InGame)),
        );
        // Cutting board menu systems
        app.add_systems(
            Update,
            (
                handle_cutting_board_interaction,
                update_cutting_board_menu_rotation,
            )
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
        app.init_resource::<PlacementPreviewEntity>();
        app.init_resource::<ActiveCuttingBoardMenu>();
    }
}
