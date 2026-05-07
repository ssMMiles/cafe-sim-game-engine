use bevy::{pbr::ExtendedMaterial, prelude::*};

pub mod camera;
pub mod crosshair;
pub mod outline;
pub mod post_process;
pub mod toon_material;

use camera::{
    adjust_camera_distance, apply_freecam_movement, camera_follow, handle_camera_input,
    resize_render_texture_sprite,
};
use crosshair::update_crosshair_display;
use outline::update_outline;
use post_process::{EdgeDetectionPlugin, sync_edge_detection_settings};

use crate::game::gfx::toon_material::{
    DefaultToonLightmap, ToonMaterial, create_default_toon_lightmap,
};

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
        // Insert at build time so the resource is available before any Startup
        // system runs — including the script host's plugin loader, which lets
        // dynamic plugins build toon materials in their own setup().
        let handle = {
            let mut images = app.world_mut().resource_mut::<Assets<Image>>();
            create_default_toon_lightmap(&mut images)
        };
        app.insert_resource(DefaultToonLightmap(handle));

        app.add_systems(Update, update_crosshair_display.in_set(GfxSet::InGame));
        app.add_systems(Update, resize_render_texture_sprite.in_set(GfxSet::InGame));
        app.add_systems(Update, adjust_camera_distance.in_set(GfxSet::InGame));

        app.add_systems(
            Update,
            (handle_camera_input, apply_freecam_movement, camera_follow)
                .chain()
                .in_set(GfxSet::Running),
        );

        app.add_plugins(MaterialPlugin::<
            ExtendedMaterial<StandardMaterial, ToonMaterial>,
        >::default());
        app.add_plugins(EdgeDetectionPlugin);

        app.add_systems(Update, update_outline.in_set(GfxSet::Running));
        app.add_systems(Update, sync_edge_detection_settings.in_set(GfxSet::InGame));
    }
}
