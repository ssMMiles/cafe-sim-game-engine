use bevy::light::{NotShadowCaster, VolumetricLight};
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

    commands.spawn((GameEntity, TerrainSetupMarker));

    spawn_lighting(commands, meshes, materials);
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
    // Add ambient light for diffusion
    commands.insert_resource(GlobalAmbientLight {
        color: Color::srgb(0.9, 0.9, 1.0), // Very dim, slightly blue for nighttime
        brightness: 100.0,                     // Very low ambient for dark nighttime
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
        Sun,
        DirectionalLight {
            color: Color::srgb(1.0, 1.0, 0.9), // Warm sunlight hue
            illuminance: 2000.0, // Bright sunlight
            shadows_enabled: true,
            ..default()
        },
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