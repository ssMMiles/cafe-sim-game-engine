use bevy::{pbr::ExtendedMaterial, prelude::*};

pub mod outline;
pub mod post_process;
pub mod toon_material;

use outline::{toggle_outline, update_outline};
use post_process::{EdgeDetectionPlugin, sync_edge_detection_settings, toggle_edge_detection};

use crate::{
    AppState,
    game::{SimulationState, gfx::toon_material::ToonMaterial},
};

pub struct GfxPlugin;

impl Plugin for GfxPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(MaterialPlugin::<
            ExtendedMaterial<StandardMaterial, ToonMaterial>,
        >::default());
        app.add_plugins(EdgeDetectionPlugin);
        app.add_systems(
            Update,
            update_outline
                .run_if(in_state(AppState::InGame).and(in_state(SimulationState::Running))),
        );
        app.add_systems(Update, toggle_outline.run_if(in_state(AppState::InGame)));
        app.add_systems(
            Update,
            toggle_edge_detection.run_if(in_state(AppState::InGame)),
        );
        app.add_systems(
            Update,
            sync_edge_detection_settings.run_if(in_state(AppState::InGame)),
        );
    }
}
