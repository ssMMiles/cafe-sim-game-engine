use avian3d::prelude::*;
use bevy::window::CursorOptions;
use bevy::{log, prelude::*};

use super::physics::{CHAR_HEIGHT, CHAR_RADIUS, CharacterState};
use super::{GameEntity, SimulationConfig};
use crate::game::gfx::camera::setup_camera;
use crate::game::world::objects::PlacedObjectRegistry;
use crate::game::world::streaming::StreamingState;

#[derive(Component)]
pub struct Player;

#[derive(Component)]
pub struct PlayerCollider;

pub fn setup(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut images: ResMut<Assets<Image>>,
    asset_server: Res<AssetServer>,
    mut cursor_options: Single<&mut CursorOptions>,
) {
    log::info!("Setting up the simulation environment");

    // Lock cursor to window for FPS-style controls
    cursor_options.grab_mode = bevy::window::CursorGrabMode::Locked;
    cursor_options.visible = false;

    let mr_windmill: Handle<Scene> = asset_server.load("meshes/character/robotv2.gltf#Scene0");

    // Capsule length = cylindrical portion only (total height - 2 * radius)
    let capsule_cylinder_height = CHAR_HEIGHT - 2.0 * CHAR_RADIUS;
    let player = commands
        .spawn((
            GameEntity,
            Player,
            CharacterState::default(),
            Transform::default(),
            GlobalTransform::default(),
            Visibility::Inherited,
            SceneRoot(mr_windmill),
            RigidBody::Kinematic,
        ))
        .id();

    // Offset the collider upward so its bottom aligns with the player's feet (entity origin).
    // avian centers the capsule at the child entity origin, so offset by half the total height.
    commands.entity(player).with_child((
        PlayerCollider,
        Collider::capsule(CHAR_RADIUS, capsule_cylinder_height),
        Transform::from_translation(Vec3::Y * CHAR_HEIGHT * 0.5),
    ));

    setup_camera(&mut commands, &mut images);
    commands.insert_resource(SimulationConfig::default());

    // Setup the game world
    crate::game::world::setup(
        &mut commands,
        &mut meshes,
        &mut materials,
    );
}

pub fn exit(
    mut commands: Commands,
    game_entities: Query<Entity, With<GameEntity>>,
    mut cursor_options: Single<&mut CursorOptions>,
    mut registry: ResMut<PlacedObjectRegistry>,
    mut streaming_state: ResMut<StreamingState>,
) {
    // Despawn all game entities (includes terrain chunks, placed objects, etc.)
    for entity in game_entities.iter() {
        commands.entity(entity).despawn();
    }

    // Cleanup the simulation environment
    commands.remove_resource::<SimulationConfig>();

    // Unlock cursor when exiting game
    cursor_options.grab_mode = bevy::window::CursorGrabMode::None;
    cursor_options.visible = true;

    // Reset placement and streaming state so re-entering the game starts fresh.
    *registry = PlacedObjectRegistry::default();
    *streaming_state = StreamingState::default();
}
