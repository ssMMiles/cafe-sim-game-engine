use avian3d::prelude::*;
use bevy::math::VectorSpace;
use bevy::pbr::ExtendedMaterial;
use bevy::window::CursorOptions;
use bevy::{log, prelude::*};

use super::icons::{SvgTextureCache, load_svg_icons};
use super::physics::{CHAR_HEIGHT, CHAR_RADIUS, CharacterState};
use super::{GameEntity, SimulationConfig};
use crate::game::gfx::camera::setup_camera;
use crate::game::gfx::toon_material::ToonMaterial;
use crate::game::world::objects::PlacedObjectRegistry;
use crate::game::world::streaming::StreamingState;
use crate::settings::UserSettings;

#[derive(Component, Debug)]
pub struct ActiveCooking {
    pub recipe_name: String,
    pub total_time: f32,
    pub remaining_time: f32,
}

#[derive(Component)]
pub struct CookingProgressOverlay;

#[derive(Component)]
pub struct CookingDisplay {
    pub table_entity: Entity,
}

#[derive(Component)]
pub struct ProgressArc;

#[derive(Component)]
pub struct Player;

#[derive(Component)]
pub struct PlayerCollider;

pub fn setup(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut toon_materials: ResMut<Assets<ExtendedMaterial<StandardMaterial, ToonMaterial>>>,
    mut images: ResMut<Assets<Image>>,
    asset_server: Res<AssetServer>,
    svg_cache: ResMut<SvgTextureCache>,
    mut cursor_options: Single<&mut CursorOptions>,
) {
    log::info!("Setting up the simulation environment");

    // Lock cursor to window for FPS-style controls
    cursor_options.grab_mode = bevy::window::CursorGrabMode::Locked;
    cursor_options.visible = false;

    let mr_windmill: Handle<Scene> = asset_server.load("meshes/character/robotv2.gltf#Scene0");

    let material = toon_materials.add(ExtendedMaterial {
        base: StandardMaterial {
            base_color: Color::srgba(0.6, 0.4, 0.2, 1.0),
            reflectance: 0.0,
            ..default()
        },
        extension: ToonMaterial {
            color: Color::srgba(0.6, 0.4, 0.2, 1.0).into(),
        },
    });

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

    setup_camera(&mut commands, &mut images, &mut meshes);
    commands.insert_resource(SimulationConfig::default());

    // Load SVG icons
    load_svg_icons(images, svg_cache);

    // Setup the game world
    crate::game::world::setup(
        &mut commands,
        &mut meshes,
        &mut materials,
        &asset_server,
        &mut toon_materials,
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
