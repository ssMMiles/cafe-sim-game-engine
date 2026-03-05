use bevy::{
    light::{CascadeShadowConfigBuilder, NotShadowCaster, VolumetricLight},
    pbr::ExtendedMaterial,
    prelude::*,
};

pub mod edit_mode;
pub mod objects;
mod sky;
pub mod terrain;

use crate::{
    AppState,
    game::{
        GameEntity, SimulationState, edit_mode_active,
        gfx::toon_material::ToonMaterial,
        world::{
            sky::{SkyDomeMaterial, setup_sky, sync_sky_dome},
            terrain::bootstrap::TerrainSetupMarker,
        },
    },
};
use edit_mode::{
    PlacementPreviewEntity, adjust_placement_scale, cleanup_placement_preview,
    handle_edit_mode_click, setup_placement_preview, toggle_edit_mode, update_placement_preview,
};
use objects::{
    ObjectsLoaded, PlacedObjectRegistry, PlacementConfig, delete_hovered_object,
    load_placed_objects, save_placed_objects,
};

pub struct WorldPlugin;

impl Plugin for WorldPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(MaterialPlugin::<SkyDomeMaterial>::default());
        app.add_systems(OnEnter(AppState::InGame), setup_sky);
        app.add_systems(Update, sync_sky_dome.run_if(in_state(AppState::InGame)));

        app.add_plugins(terrain::TerrainPlugin);

        app.add_systems(Update, toggle_edit_mode.run_if(in_state(AppState::InGame)));
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
            load_placed_objects.run_if(in_state(AppState::InGame)),
        );
        app.add_systems(
            Update,
            save_placed_objects.run_if(in_state(AppState::InGame)),
        );
        app.add_systems(
            Update,
            delete_hovered_object.run_if(
                edit_mode_active
                    .and(in_state(AppState::InGame))
                    .and(in_state(SimulationState::Running)),
            ),
        );

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
            adjust_placement_scale
                .run_if(in_state(AppState::InGame).and(in_state(SimulationState::Running))),
        );

        app.init_resource::<PlacementPreviewEntity>();
        app.init_resource::<PlacedObjectRegistry>();
        app.init_resource::<ObjectsLoaded>();
        app.init_resource::<PlacementConfig>();
    }
}

pub fn setup(
    commands: &mut Commands,
    meshes: &mut ResMut<Assets<Mesh>>,
    materials: &mut ResMut<Assets<StandardMaterial>>,
    asset_server: &Res<AssetServer>,
    toon_materials: &mut ResMut<Assets<ExtendedMaterial<StandardMaterial, ToonMaterial>>>,
) {
    log::info!("Setting up the game world");

    commands.spawn((GameEntity, TerrainSetupMarker));

    // Add ambient light for diffusion
    commands.insert_resource(GlobalAmbientLight {
        color: Color::srgb(0.9, 0.9, 1.0), // Very dim, slightly blue for nighttime
        brightness: 100.0,                 // Very low ambient for dark nighttime
        ..default()
    });

    let sun_position = Vec3::new(25.0, 50.0, -150.0);

    // Calculate rotation to point at origin
    let target = Vec3::new(0.0, 0.0, 0.0);
    let direction = (target - sun_position).normalize();
    let sun_rotation = Quat::from_rotation_arc(Vec3::NEG_Z, direction);

    // Add directional light (sunlight)
    let sun_transform = Transform {
        translation: sun_position,
        rotation: sun_rotation,
        ..default()
    };

    commands.spawn((
        GameEntity,
        DirectionalLight {
            color: Color::srgb(1.0, 1.0, 0.9), // Warm sunlight hue
            illuminance: 2000.0,               // Bright sunlight
            shadows_enabled: true,
            ..default()
        },
        CascadeShadowConfigBuilder {
            maximum_distance: 600.0,
            first_cascade_far_bound: 50.0,
            ..default()
        }
        .build(),
        VolumetricLight, // Enables light-shaft scattering through cloud FogVolumes
        sun_transform,
    ));

    // Debug sun position with yellow cone
    commands.spawn((
        GameEntity,
        Mesh3d(meshes.add(Cone::new(1.0, 3.0))),
        MeshMaterial3d(materials.add(StandardMaterial {
            base_color: Color::srgb(1.0, 1.0, 0.0),      // Bright yellow
            emissive: Color::srgb(1.0, 1.0, 0.0).into(), // Fully emissive so it glows
            cull_mode: None,                             // Show both sides
            ..Default::default()
        })),
        NotShadowCaster, // Prevent casting shadows
        Transform {
            translation: sun_transform.translation,
            rotation: sun_transform.rotation * Quat::from_rotation_x(-std::f32::consts::PI / 2.0), // Point the cone downwards
            scale: Vec3::splat(3.0), // Make it bigger
        },
    ));
}
