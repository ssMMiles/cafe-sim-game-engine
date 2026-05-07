use bevy::{light::NotShadowCaster, prelude::*};

pub mod baking;
pub mod chunk_assets;
pub mod objects;
mod sky;
pub mod streaming;
pub mod terrain;

use crate::game::{
    GameEntity,
    world::sky::{SkyDomeMaterial, sync_sky_dome},
};

use chunk_assets::{
    ChunkDataAsset, ChunkDataLoader, ChunkObjectsAsset, ChunkObjectsLoader, WorldMetaAsset,
    WorldMetaLoader,
};
use objects::{PlacedObjectRegistry, PlacementConfig, save_placed_objects};
use streaming::{
    ChunkStreamingConfig, StreamingState, finalize_computed_chunks, launch_chunk_tasks,
    update_streaming_ring,
};

pub use sky::setup_sky;

#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub enum WorldSet {
    /// Runs whenever the game is active (including while paused).
    InGame,
    /// Runs only while the simulation is running (not paused).
    Running,
}

pub struct WorldPlugin;

impl Plugin for WorldPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(MaterialPlugin::<SkyDomeMaterial>::default());
        app.add_systems(Update, sync_sky_dome.in_set(WorldSet::InGame));

        app.add_plugins(terrain::TerrainPlugin);
        app.add_plugins(baking::BakingPlugin);

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
            (
                update_streaming_ring,
                launch_chunk_tasks,
                finalize_computed_chunks,
            )
                .chain()
                .in_set(terrain::TerrainSet::Update),
        );

        app.add_systems(Update, save_placed_objects.in_set(WorldSet::InGame));

        app.init_resource::<PlacedObjectRegistry>();
        app.init_resource::<PlacementConfig>();
    }
}

pub fn setup(
    commands: &mut Commands,
    meshes: &mut ResMut<Assets<Mesh>>,
    materials: &mut ResMut<Assets<StandardMaterial>>,
) {
    log::info!("Setting up the game world");

    terrain::setup(commands);

    // Add ambient light for diffusion
    commands.insert_resource(GlobalAmbientLight {
        color: Color::srgb(0.9, 0.9, 1.0),
        brightness: 80.0, // Low fill light; main illumination comes from the directional sun
        ..default()
    });

    let sun_position = Vec3::new(20.0, 120.0, -80.0);

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

    commands.spawn((GameEntity, baking::BakedSun, sun_transform));

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

/// Tear down world-scoped resources so the next session can load a fresh
/// world. Entity despawn (terrain chunks, sun, sky, placed objects, etc.) is
/// driven by the `GameEntity` cleanup elsewhere; this only clears the
/// resource-shaped state that would otherwise carry over between projects.
pub fn destroy(commands: &mut Commands, registry: &mut PlacedObjectRegistry, streaming_state: &mut StreamingState) {
    log::info!("Tearing down the game world");
    terrain::destroy(commands);
    *registry = PlacedObjectRegistry::default();
    *streaming_state = StreamingState::default();
}

/// Swap to `new_world_id` without leaving `AppState::InGame`. Despawns
/// world-owned entities (terrain chunks, terrain setup marker, placed
/// objects), clears world-scoped resources, points `WorldConfig` at the new
/// world id, and re-arms the terrain bootstrap. Player is teleported to
/// origin so they spawn somewhere sane in the new world.
pub fn switch_world(
    new_world_id: &str,
    commands: &mut Commands,
    world_config: &mut terrain::storage::WorldConfig,
    chunk_query: &Query<Entity, With<terrain::TerrainChunk>>,
    marker_query: &Query<Entity, With<terrain::bootstrap::TerrainSetupMarker>>,
    object_query: &Query<Entity, With<objects::PlacedObject>>,
    placed_registry: &mut PlacedObjectRegistry,
    streaming_state: &mut StreamingState,
    player_transform: &mut Transform,
) {
    log::info!("Switching world to '{new_world_id}'");
    for e in chunk_query.iter() {
        commands.entity(e).despawn();
    }
    for e in marker_query.iter() {
        commands.entity(e).despawn();
    }
    for e in object_query.iter() {
        commands.entity(e).despawn();
    }

    *placed_registry = PlacedObjectRegistry::default();
    *streaming_state = StreamingState::default();
    terrain::destroy(commands);

    world_config.world_id = new_world_id.to_owned();
    terrain::setup(commands);

    *player_transform = Transform::default();
}
