use bevy::prelude::*;

use crate::{
    AppState,
    editor::EditorMode,
    game::{
        gfx::{GfxPlugin, GfxSet},
        gltf_loading::{handle_interactable_clicks, show_interaction_text},
        world::{
            WorldPlugin, WorldSet, setup_sky,
            terrain::{TerrainSet, storage::WorldConfig},
        },
    },
    projects::ActiveProject,
};

mod animation;
pub mod gfx;
pub mod gltf_loading;
pub mod pause_menu;
pub mod physics;
mod simulation;
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
    PauseMenuState, cleanup_pause_add_plugin, cleanup_pause_add_world, cleanup_pause_menu,
    cleanup_pause_settings, handle_add_plugin, handle_add_world, handle_pause_input,
    handle_pause_menu, handle_settings_menu, handle_slider, setup_editor_pause_menu,
    setup_pause_add_plugin, setup_pause_add_world, setup_pause_settings, update_slider_label,
};
use physics::character_physics;
use simulation::{exit, setup};

pub struct SimulationPlugin;

pub use engine_api::{GameEntity, SimulationConfig, SimulationState};

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

        app.add_plugins(bevy::diagnostic::FrameTimeDiagnosticsPlugin::default());

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

        // ── OnEnter / OnExit ──────────────────────────────────────────────
        app.add_systems(
            OnEnter(AppState::InGame),
            (
                apply_active_project_world_config.before(setup),
                setup,
                setup_sky,
                setup_character_animations.after(setup),
            ),
        );
        app.add_systems(OnExit(AppState::InGame), exit);

        // The engine's pause menu is the *editor* pause menu — it only spawns
        // in `EditorMode::Editor`. In `EditorMode::Gameplay` the active plugin
        // owns the pause UX entirely.
        app.add_systems(
            OnEnter(SimulationState::Paused),
            (
                setup_editor_pause_menu.run_if(in_state(EditorMode::Editor)),
                pause_animations,
            ),
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
            OnEnter(PauseMenuState::AddWorld),
            (cleanup_pause_menu, setup_pause_add_world),
        );
        app.add_systems(
            OnExit(PauseMenuState::AddWorld),
            (cleanup_pause_add_world, crate::ui::clear_focus),
        );

        app.add_systems(
            OnEnter(PauseMenuState::AddPlugin),
            (cleanup_pause_menu, setup_pause_add_plugin),
        );
        app.add_systems(OnExit(PauseMenuState::AddPlugin), cleanup_pause_add_plugin);

        app.add_systems(
            OnEnter(PauseMenuState::Main),
            (
                cleanup_pause_settings,
                cleanup_pause_add_world,
                cleanup_pause_add_plugin,
                setup_editor_pause_menu,
            )
                .run_if(in_state(SimulationState::Paused).and(in_state(EditorMode::Editor))),
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
        app.add_systems(
            Update,
            handle_pause_input
                .in_set(SimSet::InGame)
                .run_if(in_state(EditorMode::Editor)),
        );

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
            handle_add_world
                .in_set(SimSet::Paused)
                .run_if(in_state(PauseMenuState::AddWorld)),
        );
        app.add_systems(
            Update,
            handle_add_plugin
                .in_set(SimSet::Paused)
                .run_if(in_state(PauseMenuState::AddPlugin)),
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

/// Copy the active project's `start_world` into the terrain `WorldConfig` so
/// the right save dir under `worlds/` is loaded. Runs `.before(setup)` on
/// `OnEnter(AppState::InGame)`.
fn apply_active_project_world_config(
    active: Option<Res<ActiveProject>>,
    mut world_config: ResMut<WorldConfig>,
) {
    let Some(active) = active else {
        warn!("Entered InGame without an ActiveProject — falling back to default world");
        return;
    };
    world_config.world_id = active.0.start_world.clone();
}
