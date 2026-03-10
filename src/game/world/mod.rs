use bevy::{
    light::{CascadeShadowConfigBuilder, NotShadowCaster, VolumetricLight},
    pbr::ExtendedMaterial,
    prelude::*,
};

pub mod chunk_assets;
pub mod edit_mode;
pub mod mesh_selector;
pub mod objects;
pub mod streaming;
mod sky;
pub mod terrain;

use crate::{
    game::{
        GameEntity,
        gfx::toon_material::ToonMaterial,
        world::{
            sky::{SkyDomeMaterial, sync_sky_dome},
            terrain::bootstrap::TerrainSetupMarker,
        },
    },
    settings::UserSettings,
};
use edit_mode::{
    PlacementPreviewEntity, adjust_placement_scale, cleanup_placement_preview,
    handle_edit_mode_click, rebuild_preview_on_config_change, setup_placement_preview,
    toggle_edit_mode, update_placement_preview,
};
use mesh_selector::{
    MeshCatalog, MeshSelectorIndex, cleanup_mesh_selector_panel, discover_meshes,
    mesh_selector_open, scroll_mesh_selector, setup_mesh_selector_panel, toggle_mesh_selector,
};

pub use mesh_selector::MeshSelectorOpen;
use objects::{PlacedObjectRegistry, PlacementConfig, delete_hovered_object, save_placed_objects};
use chunk_assets::{ChunkDataAsset, ChunkDataLoader, ChunkObjectsAsset, ChunkObjectsLoader, WorldMetaAsset, WorldMetaLoader};
use streaming::{ChunkStreamingConfig, StreamingState, process_pending_chunks, update_streaming_ring};

pub use sky::setup_sky;

#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub enum WorldSet {
    /// Runs whenever the game is active (including while paused).
    InGame,
    /// Runs only while the simulation is running (not paused).
    Running,
}

fn edit_mode_active(settings: Res<UserSettings>) -> bool {
    settings.edit_mode
}

pub struct WorldPlugin;

impl Plugin for WorldPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(MaterialPlugin::<SkyDomeMaterial>::default());
        app.add_systems(Update, sync_sky_dome.in_set(WorldSet::InGame));

        app.add_plugins(terrain::TerrainPlugin);

        // Register chunk asset types and their loaders.
        app.init_asset::<ChunkDataAsset>();
        app.init_asset_loader::<ChunkDataLoader>();
        app.init_asset::<ChunkObjectsAsset>();
        app.init_asset_loader::<ChunkObjectsLoader>();
        app.init_asset::<WorldMetaAsset>();
        app.init_asset_loader::<WorldMetaLoader>();

        // Streaming resources and systems.
        app.init_resource::<ChunkStreamingConfig>();
        app.init_resource::<StreamingState>();
        app.add_systems(
            Update,
            (update_streaming_ring, process_pending_chunks)
                .chain()
                .in_set(terrain::TerrainSet::Update),
        );

        app.add_systems(Update, toggle_edit_mode.in_set(WorldSet::InGame));
        app.add_systems(
            Update,
            handle_edit_mode_click
                .in_set(WorldSet::Running)
                .run_if(edit_mode_active),
        );

        app.add_systems(Update, save_placed_objects.in_set(WorldSet::InGame));
        app.add_systems(
            Update,
            delete_hovered_object
                .in_set(WorldSet::Running)
                .run_if(edit_mode_active),
        );

        app.add_systems(Update, setup_placement_preview.in_set(WorldSet::InGame));
        app.add_systems(Update, cleanup_placement_preview.in_set(WorldSet::InGame));
        app.add_systems(Update, update_placement_preview.in_set(WorldSet::Running));
        app.add_systems(Update, adjust_placement_scale.in_set(WorldSet::Running));

        app.init_resource::<PlacementPreviewEntity>();
        app.init_resource::<PlacedObjectRegistry>();
        app.init_resource::<PlacementConfig>();

        app.init_resource::<MeshCatalog>();
        app.init_resource::<MeshSelectorOpen>();
        app.init_resource::<MeshSelectorIndex>();
        app.add_systems(Startup, discover_meshes);
        app.add_systems(Update, toggle_mesh_selector.in_set(WorldSet::InGame));
        app.add_systems(Update, setup_mesh_selector_panel.in_set(WorldSet::InGame));
        app.add_systems(Update, cleanup_mesh_selector_panel.in_set(WorldSet::InGame));
        app.add_systems(
            Update,
            scroll_mesh_selector
                .in_set(WorldSet::Running)
                .run_if(mesh_selector_open),
        );
        app.add_systems(
            Update,
            rebuild_preview_on_config_change
                .in_set(WorldSet::InGame)
                .run_if(edit_mode_active)
                .run_if(resource_changed::<PlacementConfig>),
        );
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
