use bevy::prelude::*;

use crate::AppState;

mod camera;
mod simulation;
mod pause_menu;

use camera::handle_movement;
use simulation::{setup, update, exit};
use pause_menu::{handle_pause_input, setup_pause_menu, cleanup_pause_menu, handle_pause_menu};

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

impl Plugin for SimulationPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(OnEnter(AppState::InGame), setup);
        app.add_systems(OnExit(AppState::InGame), exit);

        app.add_systems(
            FixedUpdate,
            update.run_if(in_state(SimulationState::Running).and(in_state(AppState::InGame))),
        );

        app.add_systems(Update, handle_movement.run_if(in_state(AppState::InGame).and(in_state(SimulationState::Running))));
        app.add_systems(Update, handle_pause_input.run_if(in_state(AppState::InGame)));
        
        app.add_systems(OnEnter(SimulationState::Paused), setup_pause_menu);
        app.add_systems(OnExit(SimulationState::Paused), cleanup_pause_menu);
        app.add_systems(Update, handle_pause_menu.run_if(in_state(SimulationState::Paused)));

        app.init_state::<SimulationState>();
        app.init_resource::<SimulationConfig>();
    }
}