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
use objects::{
    PlacedObjectRegistry, PlacementConfig, apply_auto_colliders, draw_light_gizmos,
    save_placed_objects, seed_placed_ids,
};
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

        app.add_systems(
            Update,
            (
                save_placed_objects,
                apply_auto_colliders,
                apply_environment,
                seed_placed_ids,
            )
                .in_set(WorldSet::InGame),
        );

        // Light markers are an authoring aid, so they only draw in the editor.
        app.add_systems(
            Update,
            draw_light_gizmos
                .in_set(WorldSet::InGame)
                .run_if(in_state(crate::editor::EditorMode::Editor)),
        );

        app.init_resource::<PlacedObjectRegistry>();
        app.init_resource::<PlacementConfig>();
    }
}

/// Marks the yellow cone that visualises the sun's position, so
/// [`apply_environment`] can keep it in sync with the saved sun.
#[derive(Component)]
pub struct SunDebugMarker;

/// Conversion from lightmap units to the physical units Bevy's PBR path wants.
///
/// All toon surfaces (terrain, placed meshes) read baked lightmaps and ignore
/// real-time lights entirely. The one `DirectionalLight` and the ambient
/// resource exist only for meshes that still use `StandardMaterial` — the
/// player character and editor previews. 1.0 sun ≙ 10 000 lux and 0.25
/// ambient ≙ brightness 80, the values the engine historically hard-coded.
const PBR_LUX_PER_UNIT: f32 = 10_000.0;
const PBR_AMBIENT_BRIGHTNESS_PER_UNIT: f32 = 320.0;

fn sun_light(env: &terrain::storage::EnvironmentData) -> DirectionalLight {
    DirectionalLight {
        color: env.sun_color(),
        illuminance: env.sun_intensity * PBR_LUX_PER_UNIT,
        // Shadows are baked; this light never casts any.
        shadow_maps_enabled: false,
        ..default()
    }
}

fn ambient_brightness(env: &terrain::storage::EnvironmentData) -> f32 {
    env.ambient_intensity * PBR_AMBIENT_BRIGHTNESS_PER_UNIT
}

pub fn setup(
    commands: &mut Commands,
    meshes: &mut ResMut<Assets<Mesh>>,
    materials: &mut ResMut<Assets<StandardMaterial>>,
) {
    log::info!("Setting up the game world");

    terrain::setup(commands);

    // Seed lighting from the environment defaults. Once the world's metadata
    // finishes loading, `apply_environment` overwrites these with whatever the
    // level saved; the defaults are the same values the engine used to
    // hard-code here, so an unsaved world looks identical either way.
    let env = terrain::storage::EnvironmentData::default();

    commands.insert_resource(GlobalAmbientLight {
        color: env.ambient_color(),
        brightness: ambient_brightness(&env),
        ..default()
    });

    let sun_transform = env.sun_transform();

    commands.spawn((GameEntity, baking::BakedSun, sun_light(&env), sun_transform));

    // Debug sun position with yellow cone
    commands.spawn((
        GameEntity,
        SunDebugMarker,
        Mesh3d(meshes.add(Cone::new(1.0, 3.0))),
        MeshMaterial3d(materials.add(StandardMaterial {
            base_color: Color::srgb(1.0, 1.0, 0.0),      // Bright yellow
            emissive: Color::srgb(1.0, 1.0, 0.0).into(), // Fully emissive so it glows
            cull_mode: None,                             // Show both sides
            ..Default::default()
        })),
        NotShadowCaster, // Prevent casting shadows
        sun_debug_transform(&sun_transform),
    ));
}

/// The sun cone is modelled pointing up `+Y`, so it needs an extra quarter
/// turn to point along the sun's forward axis.
fn sun_debug_transform(sun: &Transform) -> Transform {
    Transform {
        translation: sun.translation,
        rotation: sun.rotation * Quat::from_rotation_x(-std::f32::consts::PI / 2.0),
        scale: Vec3::splat(3.0),
    }
}

/// Apply the level's saved lighting once the world metadata is available.
///
/// Runs on `TerrainWorld` insert (world load, and again after `switch_world`
/// re-runs the terrain bootstrap) and whenever the environment is edited.
/// Also warns about plugins the level expects but the active project does not
/// load — by design the engine only warns and never edits the project
/// manifest on a level's behalf — and about baked lightmaps from an older
/// format that the loaders will skip.
fn apply_environment(
    terrain_world: Option<Res<terrain::storage::TerrainWorld>>,
    active: Option<Res<crate::projects::ActiveProject>>,
    mut ambient: ResMut<GlobalAmbientLight>,
    mut sun_query: Query<(&mut Transform, &mut DirectionalLight), With<baking::BakedSun>>,
    mut cone_query: Query<&mut Transform, (With<SunDebugMarker>, Without<baking::BakedSun>)>,
) {
    let Some(terrain_world) = terrain_world else {
        return;
    };
    if !terrain_world.is_changed() {
        return;
    }

    let env = &terrain_world.metadata.environment;

    ambient.color = env.ambient_color();
    ambient.brightness = ambient_brightness(env);

    let sun_transform = env.sun_transform();
    for (mut t, mut light) in sun_query.iter_mut() {
        *t = sun_transform;
        *light = sun_light(env);
    }
    for mut t in cone_query.iter_mut() {
        *t = sun_debug_transform(&sun_transform);
    }

    if terrain_world.is_added() {
        warn_missing_plugins(&terrain_world, active.as_deref());
        baking::lightmap::warn_stale_lightmaps(&terrain_world);
    }
}

/// Log the plugins a level references that the active project does not load.
/// Script bindings for them stay on their entities, so adding the plugin later
/// is all that's needed — nothing here mutates the project.
fn warn_missing_plugins(
    terrain_world: &terrain::storage::TerrainWorld,
    active: Option<&crate::projects::ActiveProject>,
) {
    let required = &terrain_world.metadata.required_plugins;
    if required.is_empty() {
        return;
    }
    let loaded: &[String] = active.map(|a| a.0.plugins.as_slice()).unwrap_or(&[]);
    for plugin in required {
        if !loaded.iter().any(|p| p == plugin) {
            log::warn!(
                "World '{}' references plugin '{}', which the active project does not load. \
                 Script bindings for it will stay dormant until you add the plugin to the project.",
                terrain_world.metadata.world_id,
                plugin
            );
        }
    }
}

/// Tear down world-scoped resources so the next session can load a fresh
/// world. Entity despawn (terrain chunks, sun, sky, placed objects, etc.) is
/// driven by the `GameEntity` cleanup elsewhere; this only clears the
/// resource-shaped state that would otherwise carry over between projects.
pub fn destroy(
    commands: &mut Commands,
    registry: &mut PlacedObjectRegistry,
    streaming_state: &mut StreamingState,
) {
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
