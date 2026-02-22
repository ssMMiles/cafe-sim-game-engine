use bevy::math::VectorSpace;
use bevy::pbr::ExtendedMaterial;
use bevy::window::CursorOptions;
use bevy::{log, prelude::*};

use super::icons::{SvgTextureCache, load_svg_icons};
use super::materials::create_arc_mesh;
use super::{GameEntity, SimulationConfig};
use super::physics::CharacterState;
use crate::game::world::ToonMaterial;
use crate::settings::UserSettings;

#[derive(Component)]
pub struct TableMarker;

#[derive(Component)]
pub struct CookerMarker;

#[derive(Component)]
pub struct FridgeMarker;

#[derive(Component)]
pub struct CuttingBoardMarker;

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

    let mr_windmill: Handle<Scene> = asset_server.load("meshes/character/mr windmill.gltf#Scene0");

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

    let _player = commands.spawn((
        GameEntity,
        Player,
        CharacterState::default(),
        Transform::default(),
        GlobalTransform::default(),
        SceneRoot(mr_windmill),
    )).id();

    super::camera::setup_camera(&mut commands, &mut images, &mut meshes);
    commands.insert_resource(SimulationConfig::default());

    // Load SVG icons
    load_svg_icons(images, svg_cache);

    // Setup the game world
    super::world::setup(&mut commands, &mut meshes, &mut materials, &asset_server, &mut toon_materials);
}



pub fn exit(
    mut commands: Commands,
    game_entities: Query<Entity, With<GameEntity>>,
    mut cursor_options: Single<&mut CursorOptions>
) {
    // Use world::cleanup to despawn all game entities
    super::world::cleanup(commands.reborrow(), game_entities);

    // Cleanup the simulation environment
    commands.remove_resource::<SimulationConfig>();

    // Unlock cursor when exiting game
    cursor_options.grab_mode = bevy::window::CursorGrabMode::None;
    cursor_options.visible = true;
}
