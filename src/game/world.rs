use bevy::{log, pbr::NotShadowCaster, prelude::*};

use super::interaction::{Interactable, InteractableType, spawn_interactable_scene};
use super::materials::PendingMaterial;
use super::{GameEntity, simulation::TableMarker};

pub fn setup(
    commands: &mut Commands,
    meshes: &mut ResMut<Assets<Mesh>>,
    materials: &mut ResMut<Assets<StandardMaterial>>,
    asset_server: &Res<AssetServer>,
) {
    log::info!("Setting up the game world");

    spawn_lighting(commands, meshes, materials);
    spawn_environment(commands, meshes, materials);
    spawn_cafe_building(commands, meshes, materials);
    spawn_table(commands, asset_server);
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
    let sun_position = Vec3::new(25.0, 50.0, -150.0);

    // Calculate rotation to point at origin
    let target = Vec3::new(0.0, 0.0, 0.0);
    let direction = (target - sun_position).normalize();
    let sun_rotation = Quat::from_rotation_arc(Vec3::NEG_Z, direction);

    // Add directional light (sun)
    let sun_transform = Transform {
        translation: sun_position,
        rotation: sun_rotation,
        ..default()
    };

    commands.spawn((
        GameEntity,
        DirectionalLight {
            color: Color::srgb(1.0, 1.0, 0.9),
            illuminance: 10000.0,
            shadows_enabled: true,
            ..default()
        },
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

    // Add ambient light for diffusion
    commands.insert_resource(AmbientLight {
        color: Color::srgb(0.6, 0.6, 0.7), // Slightly brighter and warmer
        brightness: 800.0,                 // Much higher for more diffusion
        affects_lightmapped_meshes: true,
    });
}

fn spawn_environment(
    commands: &mut Commands,
    meshes: &mut ResMut<Assets<Mesh>>,
    materials: &mut ResMut<Assets<StandardMaterial>>,
) {
    // Test sphere
    let sphere = meshes.add(Sphere::default());
    commands.spawn((
        GameEntity,
        Interactable {
            name: "Red Sphere".to_string(),
            distance: 10.0,
            interactable_type: InteractableType::Table, // Default type for test sphere
        },
        Mesh3d(sphere),
        MeshMaterial3d(materials.add(StandardMaterial {
            base_color: Color::srgb(1.0, 0.1, 0.1),
            ..Default::default()
        })),
        Transform::from_translation(Vec3::new(0.0, 1.0, 0.0)),
        GlobalTransform::default(),
    ));

    // Ground plane
    commands.spawn((
        GameEntity,
        Mesh3d(meshes.add(Plane3d {
            normal: Dir3::Y,
            half_size: Vec2 { x: 100.0, y: 100.0 },
            ..Default::default()
        })),
        MeshMaterial3d(materials.add(StandardMaterial {
            base_color: Color::srgb(0.2, 0.7, 0.2),
            ..Default::default()
        })),
        Transform::from_translation(Vec3::new(0.0, 0.0, 0.0)),
        GlobalTransform::default(),
    ));
}

fn spawn_cafe_building(
    commands: &mut Commands,
    meshes: &mut ResMut<Assets<Mesh>>,
    materials: &mut ResMut<Assets<StandardMaterial>>,
) {
    // Create a 4x bigger cafe building with door opening (2x shorter height)
    let cafe_roof = meshes.add(Cuboid::new(28.0, 1.0, 20.0));

    let brown_material = materials.add(StandardMaterial {
        base_color: Color::srgb(0.6, 0.4, 0.2),
        cull_mode: None,
        ..Default::default()
    });

    let red_material = materials.add(StandardMaterial {
        base_color: Color::srgb(0.8, 0.2, 0.2),
        cull_mode: None,
        ..Default::default()
    });

    // Cafe walls with door opening (front wall split into three parts) - 4x bigger, 1.5x taller
    // Left front wall piece (8 units wide, 1.5x taller)
    let left_wall = meshes.add(Cuboid::new(8.0, 9.0, 0.8));
    commands.spawn((
        GameEntity,
        Mesh3d(left_wall),
        MeshMaterial3d(brown_material.clone()),
        Transform::from_translation(Vec3::new(0.0, 4.5, -8.0)),
        GlobalTransform::default(),
    ));

    // Right front wall piece (8 units wide, 1.5x taller)
    let right_wall = meshes.add(Cuboid::new(8.0, 9.0, 0.8));
    commands.spawn((
        GameEntity,
        Mesh3d(right_wall),
        MeshMaterial3d(brown_material.clone()),
        Transform::from_translation(Vec3::new(16.0, 4.5, -8.0)),
        GlobalTransform::default(),
    ));

    // Top piece above door (8 units wide, 1.5x taller)
    let top_wall = meshes.add(Cuboid::new(8.0, 2.4, 0.8));
    commands.spawn((
        GameEntity,
        Mesh3d(top_wall),
        MeshMaterial3d(brown_material.clone()),
        Transform::from_translation(Vec3::new(8.0, 7.8, -8.0)),
        GlobalTransform::default(),
    ));

    // Back wall (1.5x taller)
    let back_wall = meshes.add(Cuboid::new(24.0, 9.0, 0.8));
    commands.spawn((
        GameEntity,
        Mesh3d(back_wall),
        MeshMaterial3d(brown_material.clone()),
        Transform::from_translation(Vec3::new(8.0, 4.5, 8.0)),
        GlobalTransform::default(),
    ));

    // Left side wall (1.5x taller)
    let left_side_wall = meshes.add(Cuboid::new(0.8, 9.0, 16.0));
    commands.spawn((
        GameEntity,
        Mesh3d(left_side_wall),
        MeshMaterial3d(brown_material.clone()),
        Transform::from_translation(Vec3::new(-4.0, 4.5, 0.0)),
        GlobalTransform::default(),
    ));

    // Right side wall (1.5x taller)
    let right_side_wall = meshes.add(Cuboid::new(0.8, 9.0, 16.0));
    commands.spawn((
        GameEntity,
        Mesh3d(right_side_wall),
        MeshMaterial3d(brown_material.clone()),
        Transform::from_translation(Vec3::new(20.0, 4.5, 0.0)),
        GlobalTransform::default(),
    ));

    // Cafe roof (1.5x taller)
    commands.spawn((
        GameEntity,
        Mesh3d(cafe_roof),
        MeshMaterial3d(red_material),
        Transform::from_translation(Vec3::new(8.0, 9.75, 0.0)),
        GlobalTransform::default(),
    ));
}

fn spawn_table(commands: &mut Commands, asset_server: &Res<AssetServer>) {
    // Load the scene directly
    let table_scene: Handle<Scene> = asset_server.load("meshes/table/result.gltf#Scene0");
    log::info!("Loading scene from: meshes/table/result.gltf#Scene0");

    spawn_interactable_scene(
        commands,
        (
            GameEntity,
            Interactable {
                name: "Wooden Table".to_string(),
                distance: 10.0,
                interactable_type: InteractableType::Table,
            },
            SceneRoot(table_scene),
            Transform::from_translation(Vec3::new(3.0, 0.0, 0.0)).with_scale(Vec3::splat(0.02)), // Scale down to 1/50th size
            GlobalTransform::default(),
            TableMarker, // Mark this as a table for material override
            PendingMaterial {
                base_color: Color::srgb(0.6, 0.4, 0.2), // Brown wood color
                metallic: 0.0,
                perceptual_roughness: 0.9,
                alpha_mode: bevy::render::alpha::AlphaMode::Opaque,
            },
        ),
    );

    log::info!("Table entity spawned at position (3, 0, 0)");
}
