use bevy::{pbr::ExtendedMaterial, prelude::*};

pub mod outline;
pub mod physics_debug;
pub mod post_process;
pub mod toon_material;
pub mod camera;
mod crosshair;

use crosshair::{toggle_crosshair, update_crosshair_display};
use outline::{toggle_outline, update_outline};
use physics_debug::{sync_physics_debug, toggle_physics_debug};
use post_process::{EdgeDetectionPlugin, sync_edge_detection_settings, toggle_edge_detection};
use camera::{adjust_camera_distance, apply_freecam_movement, camera_follow, handle_camera_input, resize_render_texture_sprite};

use crate::{
    AppState,
    game::{SimulationState, gfx::toon_material::ToonMaterial},
};

pub struct GfxPlugin;

impl Plugin for GfxPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, toggle_crosshair.run_if(in_state(AppState::InGame)));
        app.add_systems(
            Update,
            update_crosshair_display.run_if(in_state(AppState::InGame)),
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
            (
                handle_camera_input,
                apply_freecam_movement,
                camera_follow,
            )
                .chain()
                .run_if(in_state(AppState::InGame).and(in_state(SimulationState::Running))),
        );

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
        app.add_systems(Update, toggle_physics_debug.run_if(in_state(AppState::InGame)));
        app.add_systems(Update, sync_physics_debug.run_if(in_state(AppState::InGame)));
    }
}
