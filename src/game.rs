use std::f32::consts::PI;

use bevy::{log, prelude::*, render::camera::ScalingMode};

use crate::AppState;

pub struct SimulationPlugin;

#[derive(Default, Clone, Copy, PartialEq, Eq, Hash, Debug, States)]
pub enum SimulationState {
    #[default]
    Running,
    Paused,
}

#[derive(Resource, Debug)]
pub struct SimulationConfig {
    pub speed: f32,
}

impl Default for SimulationConfig {
    fn default() -> Self {
        SimulationConfig { speed: 1.0 }
    }
}

impl Plugin for SimulationPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(OnEnter(AppState::InGame), setup);
        app.add_systems(OnExit(AppState::InGame), exit);

        app.add_systems(
            FixedUpdate,
            (update).run_if(in_state(SimulationState::Running).and(in_state(AppState::InGame))),
        );

        app.add_systems(Update, (handle_movement).run_if(in_state(AppState::InGame)));

        app.init_state::<SimulationState>();
        app.init_resource::<SimulationConfig>();
    }
}

#[derive(Component, Debug, Default)]
pub struct PlayerCamera {
    pub velocity: Vec3,
}

fn setup(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    log::info!("Setting up the simulation environment");

    // setup a 2d camera
    commands.spawn((
        PlayerCamera::default(),
        Camera3d::default(),
        Projection::Perspective(PerspectiveProjection {
            // PI = 180 degree fov, so idfk what this does
            fov: (PI / 5.),
            ..default()
        }),
        Transform::from_translation(Vec3::new(0.0, 25.0, 25.0)).looking_at(Vec3::ZERO, Vec3::Y),
    ));
    commands.insert_resource(SimulationConfig::default());

    let sphere = meshes.add(Sphere::default());

    // spawn a sphere at 0, 0, 0
    commands.spawn((
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
        Mesh3d(meshes.add(Plane3d {
            normal: Dir3::Y,
            half_size: Vec2 { x: 100.0, y: 100.0 },
            ..Default::default()
        })),
        MeshMaterial3d(materials.add(StandardMaterial {
            base_color: Color::srgb(0.1, 0.1, 0.1),
            ..Default::default()
        })),
        Transform::from_translation(Vec3::new(0.0, 0.0, 0.0)),
        GlobalTransform::default(),
    ));
}

const SPEED: f32 = 1.0;
const ACCELERATION: f32 = 0.2;
const MAX_VELOCITY: f32 = 1.0;
// Handle keyboard input for wasd/space/ctrl to move the camera
fn handle_movement(
    mut commands: Commands,
    keyboard_input: Res<ButtonInput<KeyCode>>,
    mut query: Query<(&mut Transform, &mut PlayerCamera)>,
) {
    for (mut transform, mut camera) in query.iter_mut() {
        // Update the camera velocity based on keyboard input
        if keyboard_input.pressed(KeyCode::KeyW) {
            camera.velocity.z -= ACCELERATION;
        } else if keyboard_input.pressed(KeyCode::KeyS) {
            camera.velocity.z += ACCELERATION;
        } else {
            camera.velocity.z = 0.0;
        }

        if keyboard_input.pressed(KeyCode::KeyA) {
            camera.velocity.x -= ACCELERATION;
        } else if keyboard_input.pressed(KeyCode::KeyD) {
            camera.velocity.x += ACCELERATION;
        } else {
            camera.velocity.x = 0.0;
        }

        if keyboard_input.pressed(KeyCode::Space) {
            camera.velocity.y += ACCELERATION;
        } else if keyboard_input.pressed(KeyCode::ControlLeft) {
            camera.velocity.y -= ACCELERATION;
        } else {
            camera.velocity.y = 0.0;
        }

        // Clamp the velocity to the maximum value
        camera.velocity.x = camera.velocity.x.clamp(-MAX_VELOCITY, MAX_VELOCITY);
        camera.velocity.y = camera.velocity.y.clamp(-MAX_VELOCITY, MAX_VELOCITY);
        camera.velocity.z = camera.velocity.z.clamp(-MAX_VELOCITY, MAX_VELOCITY);

        // Update the camera translation based on the velocity
        transform.translation.x += camera.velocity.x * SPEED;
        transform.translation.y += camera.velocity.y * SPEED;
        transform.translation.z += camera.velocity.z * SPEED;

        info!("Camera translation: {:?}", transform.translation);
        info!("Camera velocity: {:?}", camera.velocity);
    }
}

fn update(mut commands: Commands, mut simulation_state: ResMut<SimulationConfig>, time: Res<Time>) {
    // Update the simulation state based on the elapsed time
    // simulation_state.update(time.delta_seconds());
}

fn exit(mut commands: Commands) {
    // Cleanup the simulation environment
    commands.remove_resource::<SimulationConfig>();
}
