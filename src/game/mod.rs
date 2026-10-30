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
pub mod blob_shadow;
pub mod gfx;
pub mod gltf_loading;
pub mod physics;
mod simulation;
pub mod world;

use animation::{
    init_animation_player, pause_animations, resume_animations, setup_character_animations,
    update_character_animation,
};
use gltf_loading::{
    InteractableClicked, check_interactions, detect_interactable_clicks, sync_dynamic_lit_source,
};
use physics::character_physics;
use simulation::{Player, exit, setup};

use crate::game::gfx::camera::PlayerCamera;

pub struct SimulationPlugin;

pub use engine_api::{GameEntity, SimulationConfig, SimulationState};

/// The position the world streams and bakes around: the editor camera while
/// authoring (it flies far from the player), the player in gameplay.
#[derive(Resource, Default, Debug, Clone, Copy)]
pub struct WorldFocus(pub Vec3);

fn update_world_focus(
    mode: Res<State<EditorMode>>,
    player: Query<&Transform, With<Player>>,
    camera: Query<&Transform, With<PlayerCamera>>,
    mut focus: ResMut<WorldFocus>,
) {
    let source = match mode.get() {
        EditorMode::Editor => camera.single().ok(),
        EditorMode::Gameplay => player.single().ok(),
    };
    if let Some(t) = source
        && focus.0 != t.translation
    {
        focus.0 = t.translation;
    }
}

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
                .run_if(in_state(AppState::InGame).and_then(in_state(SimulationState::Running))),
        );
        app.configure_sets(
            Update,
            SimSet::Paused
                .run_if(in_state(AppState::InGame).and_then(in_state(SimulationState::Paused))),
        );
        app.configure_sets(
            FixedUpdate,
            SimSet::Running
                .run_if(in_state(AppState::InGame).and_then(in_state(SimulationState::Running))),
        );

        // Sub-plugin sets — GfxPlugin
        app.configure_sets(Update, GfxSet::InGame.run_if(in_state(AppState::InGame)));
        app.configure_sets(
            Update,
            GfxSet::Running
                .run_if(in_state(AppState::InGame).and_then(in_state(SimulationState::Running))),
        );

        // Sub-plugin sets — WorldPlugin
        app.configure_sets(Update, WorldSet::InGame.run_if(in_state(AppState::InGame)));
        app.configure_sets(
            Update,
            WorldSet::Running
                .run_if(in_state(AppState::InGame).and_then(in_state(SimulationState::Running))),
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

        // The engine has no pause UI of its own any more: the editor is a
        // docked window (Escape cancels/deselects there) and in gameplay the
        // active plugin owns the pause UX. `SimulationState::Paused` remains
        // for plugins that want to freeze the simulation.
        app.add_systems(OnEnter(SimulationState::Paused), pause_animations);
        app.add_systems(OnExit(SimulationState::Paused), resume_animations);

        app.add_plugins(blob_shadow::BlobShadowPlugin);

        // ── Update ────────────────────────────────────────────────────────
        app.init_resource::<WorldFocus>();
        app.add_systems(
            PreUpdate,
            update_world_focus.run_if(in_state(AppState::InGame)),
        );
        // Walking and the crosshair-style interaction probe are gameplay:
        // the editor flies the camera and picks with the cursor instead.
        app.add_systems(
            Update,
            (
                character_physics.run_if(in_state(EditorMode::Gameplay)),
                update_character_animation,
            )
                .chain()
                .in_set(SimSet::Running),
        );
        app.add_systems(Update, init_animation_player.in_set(SimSet::InGame));
        // `on_scene_ready` picks a scene's lighting source once; while
        // authoring, `DynamicLit` also toggles after the fact (script
        // hydration, inspector attach/detach, hot-reload), so the editor
        // keeps converted materials in step. Gameplay converts with the
        // marker already hydrated and skips the per-frame check.
        app.add_systems(
            Update,
            sync_dynamic_lit_source
                .in_set(SimSet::InGame)
                .run_if(in_state(EditorMode::Editor)),
        );
        app.add_systems(
            Update,
            (
                check_interactions,
                detect_interactable_clicks,
                show_interaction_text,
                handle_interactable_clicks,
            )
                .in_set(SimSet::Running)
                .run_if(in_state(EditorMode::Gameplay)),
        );
        // ── Resources / States ────────────────────────────────────────────
        app.init_state::<SimulationState>();
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
