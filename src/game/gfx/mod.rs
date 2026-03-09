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

use crate::game::gfx::toon_material::ToonMaterial;

#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub enum GfxSet {
    /// Runs whenever the game is active (including while paused).
    InGame,
    /// Runs only while the simulation is running (not paused).
    Running,
}

pub struct GfxPlugin;

impl Plugin for GfxPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, toggle_crosshair.in_set(GfxSet::InGame));
        app.add_systems(Update, update_crosshair_display.in_set(GfxSet::InGame));
        app.add_systems(Update, resize_render_texture_sprite.in_set(GfxSet::InGame));
        app.add_systems(Update, adjust_camera_distance.in_set(GfxSet::InGame));

        app.add_systems(
            Update,
            (
                handle_camera_input,
                apply_freecam_movement,
                camera_follow,
            )
                .chain()
                .in_set(GfxSet::Running),
        );

        app.add_plugins(MaterialPlugin::<
            ExtendedMaterial<StandardMaterial, ToonMaterial>,
        >::default());
        app.add_plugins(EdgeDetectionPlugin);

        app.add_systems(Update, update_outline.in_set(GfxSet::Running));
        app.add_systems(Update, toggle_outline.in_set(GfxSet::InGame));
        app.add_systems(Update, toggle_edge_detection.in_set(GfxSet::InGame));
        app.add_systems(Update, sync_edge_detection_settings.in_set(GfxSet::InGame));
        app.add_systems(Update, toggle_physics_debug.in_set(GfxSet::InGame));
        app.add_systems(Update, sync_physics_debug.in_set(GfxSet::InGame));
    }
}
