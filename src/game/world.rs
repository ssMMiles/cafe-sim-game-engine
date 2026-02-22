use bevy::pbr::{ExtendedMaterial, MaterialExtension};
use bevy::render::alpha::AlphaMode;
use bevy::render::render_resource::AsBindGroup;
use bevy::shader::ShaderRef;
use bevy::{log, prelude::*};

use crate::terrain::TerrainSetupMarker;

use super::day_night::Sun;
use super::interaction::{Interactable, spawn_interactable_scene};
use super::materials::PendingMaterial;
use super::physics::Collider;
use super::{GameEntity, simulation::TableMarker};

// Define the Material Extension
#[derive(Asset, TypePath, AsBindGroup, Debug, Clone)]
pub struct ToonMaterial {
    #[uniform(100)]
    pub color: LinearRgba,
}

impl MaterialExtension for ToonMaterial {
    fn fragment_shader() -> ShaderRef {
        "shaders/toon_lighting.wgsl".into()
    }

    fn deferred_fragment_shader() -> ShaderRef {
        "shaders/toon_lighting.wgsl".into()
    }

    fn alpha_mode() -> Option<AlphaMode> {
        Some(AlphaMode::Opaque)
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

    spawn_environment(commands, meshes, toon_materials, asset_server);
    spawn_lighting(commands, meshes, materials);
}

fn spawn_environment(
    commands: &mut Commands,
    meshes: &mut ResMut<Assets<Mesh>>,
    toon_materials: &mut ResMut<Assets<ExtendedMaterial<StandardMaterial, ToonMaterial>>>,
    asset_server: &Res<AssetServer>,
) {
    commands.spawn((GameEntity, TerrainSetupMarker));
}

pub fn cleanup(mut commands: Commands, game_entities: Query<Entity, With<GameEntity>>) {
    log::info!("Cleaning up the game world");

    // Despawn all game entities
    for entity in game_entities.iter() {
        commands.entity(entity).despawn();
    }
}

fn spawn_lighting(
    commands: &mut Commands,
    meshes: &mut ResMut<Assets<Mesh>>,
    materials: &mut ResMut<Assets<StandardMaterial>>,
) {
    let moon_position = Vec3::new(25.0, 50.0, -150.0);

    // Calculate rotation to point at origin
    let target = Vec3::new(0.0, 0.0, 0.0);
    let direction = (target - moon_position).normalize();
    let moon_rotation = Quat::from_rotation_arc(Vec3::NEG_Z, direction);

    // Add directional light (moonlight)
    let moon_transform = Transform {
        translation: moon_position,
        rotation: moon_rotation,
        ..default()
    };

    commands.spawn((
        GameEntity,
        Sun,
        DirectionalLight {
            color: Color::srgb(0.3, 0.3, 0.4), // Saturated blue for moonlight hue
            illuminance: 20.0, // Very low illuminance for dark nighttime
            shadows_enabled: true,
            ..default()
        },
        moon_transform,
    ));

    // Debug sun position with yellow cone
    // commands.spawn((
    //     GameEntity,
    //     Mesh3d(meshes.add(Cone::new(1.0, 3.0))),
    //     MeshMaterial3d(materials.add(StandardMaterial {
    //         base_color: Color::srgb(1.0, 1.0, 0.0),      // Bright yellow
    //         emissive: Color::srgb(1.0, 1.0, 0.0).into(), // Fully emissive so it glows
    //         cull_mode: None,                             // Show both sides
    //         ..Default::default()
    //     })),
    //     NotShadowCaster, // Prevent casting shadows
    //     Transform {
    //         translation: sun_transform.translation,
    //         rotation: sun_transform.rotation * Quat::from_rotation_x(-std::f32::consts::PI / 2.0), // Point the cone downwards
    //         scale: Vec3::splat(3.0), // Make it bigger
    //     },
    // ));

    // Add ambient light for diffusion
    commands.insert_resource(GlobalAmbientLight {
        color: Color::srgb(0.05, 0.05, 0.1), // Very dim, slightly blue for nighttime
        brightness: 0.02,                     // Very low ambient for dark nighttime
        ..default()
    });

    // Add warm point light inside the building (lamp)
    commands.spawn((
        GameEntity,
        PointLight {
            color: Color::srgb(1.0, 0.7, 0.4), // Warm orange/yellow lamp
            intensity: 10000.0, // Bright lamp
            range: 2500.0,
            radius: 0.5,
            shadows_enabled: true,
            ..default()
        },
        Transform::from_translation(Vec3::new(8.0, 5.0, 0.0)), // Center of building, mid-height
    ));
}