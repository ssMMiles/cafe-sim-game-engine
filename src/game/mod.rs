use bevy::prelude::*;

use crate::{
    AppState,
    game::{
        gfx::{GfxPlugin, GfxSet},
        gltf_loading::{handle_interactable_clicks, show_interaction_text},
        world::{WorldPlugin, WorldSet, setup_sky, terrain::TerrainSet},
    },
};

mod animation;
mod gfx;
mod gltf_loading;
pub mod pause_menu;
pub mod physics;
mod simulation;
mod ui_displays;
pub mod world;

use crate::settings::{
    EdgeDepthSensitivitySetting, EdgeNormalSensitivitySetting, MouseSensitivitySetting,
    MovementSpeedSetting,
};
use animation::{
    init_animation_player, pause_animations, resume_animations, setup_character_animations,
    update_character_animation,
};
use gltf_loading::{InteractableClicked, check_interactions, detect_interactable_clicks};
use pause_menu::{
    PauseMenuState, cleanup_pause_menu, cleanup_pause_settings, handle_pause_input,
    handle_pause_menu, handle_settings_menu, handle_slider, setup_pause_menu, setup_pause_settings,
    update_slider_label,
};
use physics::character_physics;
use simulation::{exit, setup};
use ui_displays::{
    setup_fps_display, setup_interaction_info_display, setup_keybinds_display,
    setup_position_display, update_fps_display, update_interaction_info_display,
    update_keybinds_display, update_position_display,
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
    #[allow(dead_code)]
    pub speed: f32,
}

impl Default for SimulationConfig {
    fn default() -> Self {
        SimulationConfig { speed: 1.0 }
    }
}

#[derive(Component)]
pub struct GameEntity;

/// System sets for the simulation, configured by [`SimulationPlugin`].
///
/// Sub-plugins use their own sets (e.g. [`GfxSet`], [`WorldSet`]); this set is
/// for systems that live directly in [`SimulationPlugin`].
#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub enum SimSet {
    /// Runs when in [`AppState::InGame`] (including while paused).
    InGame,
    /// Runs when in [`AppState::InGame`] and [`SimulationState::Running`].
    Running,
    /// Runs when in [`AppState::InGame`] and [`SimulationState::Paused`].
    Paused,
}

impl Plugin for SimulationPlugin {
    fn build(&self, app: &mut App) {
        #[cfg(feature = "profiling")]
        app.add_plugins(bevy::diagnostic::SystemInformationDiagnosticsPlugin);

        app.add_plugins(WorldPlugin);
        app.add_plugins(GfxPlugin);
        app.add_message::<InteractableClicked>();

        // ── Set configuration ─────────────────────────────────────────────
        // SimulationPlugin's own sets
        app.configure_sets(Update, SimSet::InGame.run_if(in_state(AppState::InGame)));
        app.configure_sets(
            Update,
            SimSet::Running
                .run_if(in_state(AppState::InGame).and(in_state(SimulationState::Running))),
        );
        app.configure_sets(
            Update,
            SimSet::Paused
                .run_if(in_state(AppState::InGame).and(in_state(SimulationState::Paused))),
        );
        app.configure_sets(
            FixedUpdate,
            SimSet::Running
                .run_if(in_state(AppState::InGame).and(in_state(SimulationState::Running))),
        );

        // Sub-plugin sets — GfxPlugin
        app.configure_sets(Update, GfxSet::InGame.run_if(in_state(AppState::InGame)));
        app.configure_sets(
            Update,
            GfxSet::Running
                .run_if(in_state(AppState::InGame).and(in_state(SimulationState::Running))),
        );

        // Sub-plugin sets — WorldPlugin
        app.configure_sets(Update, WorldSet::InGame.run_if(in_state(AppState::InGame)));
        app.configure_sets(
            Update,
            WorldSet::Running
                .run_if(in_state(AppState::InGame).and(in_state(SimulationState::Running))),
        );

        // Sub-plugin sets — TerrainPlugin
        app.configure_sets(
            Update,
            TerrainSet::Update.run_if(in_state(AppState::InGame)),
        );
        app.configure_sets(
            Update,
            TerrainSet::Edit
                .run_if(in_state(AppState::InGame).and(in_state(SimulationState::Running))),
        );

        // ── OnEnter / OnExit ──────────────────────────────────────────────
        app.add_systems(
            OnEnter(AppState::InGame),
            (
                setup,
                setup_sky,
                setup_keybinds_display,
                setup_fps_display,
                setup_interaction_info_display,
                setup_position_display,
                setup_character_animations.after(setup),
            ),
        );
        app.add_systems(OnExit(AppState::InGame), exit);

        app.add_systems(
            OnEnter(SimulationState::Paused),
            (setup_pause_menu, pause_animations),
        );
        app.add_systems(
            OnExit(SimulationState::Paused),
            (cleanup_pause_menu, resume_animations),
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

        // ── Update ────────────────────────────────────────────────────────
        app.add_systems(
            Update,
            (character_physics, update_character_animation)
                .chain()
                .in_set(SimSet::Running),
        );
        app.add_systems(Update, init_animation_player.in_set(SimSet::InGame));
        app.add_systems(
            Update,
            (
                check_interactions,
                detect_interactable_clicks,
                show_interaction_text,
            )
                .in_set(SimSet::Running),
        );
        app.add_systems(Update, handle_interactable_clicks.in_set(SimSet::Running));
        app.add_systems(Update, update_keybinds_display.in_set(SimSet::InGame));
        app.add_systems(Update, update_fps_display.in_set(SimSet::InGame));
        app.add_systems(
            Update,
            update_interaction_info_display.in_set(SimSet::InGame),
        );
        app.add_systems(Update, update_position_display.in_set(SimSet::InGame));
        app.add_systems(Update, handle_pause_input.in_set(SimSet::InGame));

        // Pause menu systems — also implicitly gated to AppState::InGame via SimSet::Paused
        app.add_systems(
            Update,
            handle_pause_menu
                .in_set(SimSet::Paused)
                .run_if(in_state(PauseMenuState::Main)),
        );
        app.add_systems(
            Update,
            handle_settings_menu
                .in_set(SimSet::Paused)
                .run_if(in_state(PauseMenuState::Settings)),
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
                .in_set(SimSet::Paused)
                .run_if(in_state(PauseMenuState::Settings)),
        );

        // ── Resources / States ────────────────────────────────────────────
        app.init_state::<SimulationState>();
        app.init_state::<PauseMenuState>();
        app.init_resource::<SimulationConfig>();
    }
}
