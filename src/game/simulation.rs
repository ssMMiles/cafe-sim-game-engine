use bevy::{log, prelude::*};

use super::{GameEntity, SimulationConfig};

pub fn setup(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut windows: Query<&mut Window>,
) {
    log::info!("Setting up the simulation environment");
    
    // Lock cursor to window for FPS-style controls
    if let Ok(mut window) = windows.get_single_mut() {
        window.cursor_options.grab_mode = bevy::window::CursorGrabMode::Locked;
        window.cursor_options.visible = false;
    }

    super::camera::setup_camera(&mut commands);
    commands.insert_resource(SimulationConfig::default());

    // Add lighting
    commands.spawn((
        GameEntity,
        DirectionalLight {
            color: Color::srgb(1.0, 1.0, 0.9),
            illuminance: 10000.0,
            shadows_enabled: true,
            ..default()
        },
        Transform::from_rotation(Quat::from_euler(EulerRot::XYZ, -0.5, -0.5, 0.0)),
    ));

    // Add ambient light
    commands.insert_resource(AmbientLight {
        color: Color::srgb(0.4, 0.4, 0.5),
        brightness: 300.0,
    });

    let sphere = meshes.add(Sphere::default());

    // spawn a sphere at 0, 0, 0
    commands.spawn((
        GameEntity,
        Mesh3d(sphere),
        MeshMaterial3d(materials.add(StandardMaterial {
            base_color: Color::srgb(1.0, 0.1, 0.1),
            ..Default::default()
        })),
        Transform::from_translation(Vec3::new(0.0, 1.0, 0.0)),
        GlobalTransform::default(),
    ));

    // spawn a plane at 0, 0, 0
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

    spawn_cafe_building(&mut commands, &mut meshes, &mut materials);
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
    
    // Cafe walls with door opening (front wall split into three parts) - 4x bigger
    // Left front wall piece (8 units wide, 2x shorter)
    let left_wall = meshes.add(Cuboid::new(8.0, 6.0, 0.8));
    commands.spawn((
        GameEntity,
        Mesh3d(left_wall),
        MeshMaterial3d(brown_material.clone()),
        Transform::from_translation(Vec3::new(0.0, 3.0, -8.0)),
        GlobalTransform::default(),
    ));
    
    // Right front wall piece (8 units wide, 2x shorter)
    let right_wall = meshes.add(Cuboid::new(8.0, 6.0, 0.8));
    commands.spawn((
        GameEntity,
        Mesh3d(right_wall),
        MeshMaterial3d(brown_material.clone()),
        Transform::from_translation(Vec3::new(16.0, 3.0, -8.0)),
        GlobalTransform::default(),
    ));
    
    // Top piece above door (8 units wide, 2x shorter)
    let top_wall = meshes.add(Cuboid::new(8.0, 1.6, 0.8));
    commands.spawn((
        GameEntity,
        Mesh3d(top_wall),
        MeshMaterial3d(brown_material.clone()),
        Transform::from_translation(Vec3::new(8.0, 5.2, -8.0)),
        GlobalTransform::default(),
    ));
    
    // Back wall (2x shorter)
    let back_wall = meshes.add(Cuboid::new(24.0, 6.0, 0.8));
    commands.spawn((
        GameEntity,
        Mesh3d(back_wall),
        MeshMaterial3d(brown_material.clone()),
        Transform::from_translation(Vec3::new(8.0, 3.0, 8.0)),
        GlobalTransform::default(),
    ));
    
    // Left side wall (2x shorter)
    let left_side_wall = meshes.add(Cuboid::new(0.8, 6.0, 16.0));
    commands.spawn((
        GameEntity,
        Mesh3d(left_side_wall),
        MeshMaterial3d(brown_material.clone()),
        Transform::from_translation(Vec3::new(-4.0, 3.0, 0.0)),
        GlobalTransform::default(),
    ));
    
    // Right side wall (2x shorter)
    let right_side_wall = meshes.add(Cuboid::new(0.8, 6.0, 16.0));
    commands.spawn((
        GameEntity,
        Mesh3d(right_side_wall),
        MeshMaterial3d(brown_material.clone()),
        Transform::from_translation(Vec3::new(20.0, 3.0, 0.0)),
        GlobalTransform::default(),
    ));
    
    // Cafe roof (2x shorter height)
    commands.spawn((
        GameEntity,
        Mesh3d(cafe_roof),
        MeshMaterial3d(red_material),
        Transform::from_translation(Vec3::new(8.0, 6.5, 0.0)),
        GlobalTransform::default(),
    ));
}

pub fn update(_commands: Commands, _simulation_state: ResMut<SimulationConfig>, _time: Res<Time>) {
    // Update the simulation state based on the elapsed time
    // simulation_state.update(time.delta_seconds());
}

pub fn exit(
    mut commands: Commands, 
    mut windows: Query<&mut Window>,
    game_entities: Query<Entity, With<GameEntity>>,
) {
    // Cleanup all game entities
    for entity in game_entities.iter() {
        commands.entity(entity).despawn_recursive();
    }
    
    // Cleanup the simulation environment
    commands.remove_resource::<SimulationConfig>();
    
    // Unlock cursor when exiting game
    if let Ok(mut window) = windows.get_single_mut() {
        window.cursor_options.grab_mode = bevy::window::CursorGrabMode::None;
        window.cursor_options.visible = true;
    }
}