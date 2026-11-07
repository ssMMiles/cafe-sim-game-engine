use bevy::prelude::*;

pub mod atmosphere;
pub mod camera;
pub mod crosshair;
pub mod frame_pacing;
pub mod post_process;
pub mod profiling;
#[cfg(test)]
mod shader_check;
pub mod system_timing;
pub mod toon_material;

use atmosphere::{AtmospherePlugin, sync_atmosphere_settings};
use camera::{
    adjust_camera_distance, apply_freecam_movement, camera_follow, handle_camera_input,
    resize_render_texture_sprite,
};
use crosshair::update_crosshair_display;
use post_process::{EdgeDetectionPlugin, sync_edge_detection_settings};
use profiling::{GfxProfilingPlugin, run_effect_bench};

use crate::editor::EditorMode;
use crate::game::gfx::toon_material::{
    DefaultToonLightmap, ToonLook, ToonMaterial, create_default_toon_lightmap,
    create_probe_volume_images, sync_toon_look, update_toon_look,
};

#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub enum GfxSet {
    /// Runs whenever the game is active (including while paused).
    InGame,
    /// Runs only while the simulation is running (not paused).
    Running,
}

pub struct GfxPlugin;

/// Ordering for our own `Core3d` post-process passes. Both sit inside
/// `Core3dSystems::PostProcess`, after `tonemapping` and before `fxaa`;
/// this fixes their order against each other without either module having
/// to expose its pass or pipeline types.
///
/// Edges are drawn first so the atmosphere fogs the outlines along with the
/// geometry they trace.
#[derive(SystemSet, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PostProcessPass {
    EdgeDetection,
    Atmosphere,
}

impl Plugin for GfxPlugin {
    fn build(&self, app: &mut App) {
        // Insert at build time so the resource is available before any Startup
        // system runs — including the script host's plugin loader, which lets
        // dynamic plugins build toon materials in their own setup().
        let (handle, probes) = {
            let mut images = app.world_mut().resource_mut::<Assets<Image>>();
            (
                create_default_toon_lightmap(&mut images),
                create_probe_volume_images(&mut images),
            )
        };
        app.insert_resource(DefaultToonLightmap(handle));
        app.insert_resource(probes);
        app.init_resource::<ToonLook>();
        app.add_systems(
            Update,
            (update_toon_look, sync_toon_look)
                .chain()
                .in_set(GfxSet::InGame),
        );

        app.add_systems(Update, update_crosshair_display.in_set(GfxSet::InGame));

        // Gameplay camera drivers. In `EditorMode::Editor` the editor's fly
        // camera owns the camera transform and the viewport is an egui tab,
        // so the player-follow, free-cam, zoom and full-window sprite sizing
        // all step aside (see `editor::camera`).
        app.add_systems(
            Update,
            (resize_render_texture_sprite, adjust_camera_distance)
                .in_set(GfxSet::InGame)
                .run_if(in_state(EditorMode::Gameplay)),
        );
        app.add_systems(
            Update,
            (handle_camera_input, apply_freecam_movement, camera_follow)
                .chain()
                .in_set(GfxSet::Running)
                .run_if(in_state(EditorMode::Gameplay)),
        );

        app.add_plugins(MaterialPlugin::<ToonMaterial>::default());
        app.add_plugins(EdgeDetectionPlugin);
        app.add_plugins(AtmospherePlugin);
        app.add_plugins(GfxProfilingPlugin);
        app.add_plugins(frame_pacing::FramePacingPlugin);
        app.add_plugins(system_timing::SystemTimingPlugin);

        // The benchmark's override has to be set before the uniforms that
        // read it are built, or it would take effect a frame late — on the
        // boundary between an A phase and a B phase.
        app.add_systems(
            Update,
            (
                run_effect_bench,
                (sync_edge_detection_settings, sync_atmosphere_settings),
            )
                .chain()
                .in_set(GfxSet::InGame),
        );
    }
}
