use std::f32::consts::PI;

use bevy::{prelude::*, input::mouse::MouseMotion};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MovementMode {
    FreeCam,
    FloorBased,
}

impl Default for MovementMode {
    fn default() -> Self {
        MovementMode::FreeCam
    }
}

#[derive(Component, Debug, Default)]
pub struct PlayerCamera {
    pub velocity: Vec3,
    pub movement_mode: MovementMode,
    pub yaw: f32,
    pub pitch: f32,
}

const SPEED: f32 = 0.1;
const ACCELERATION: f32 = 0.2;
const MAX_VELOCITY: f32 = 1.0;
const MOUSE_SENSITIVITY: f32 = 0.003;
const FLOOR_HEIGHT: f32 = 2.0;

#[derive(Default)]
struct MovementInput {
    forward: bool,
    backward: bool,
    left: bool,
    right: bool,
    up: bool,
    down: bool,
    toggle_mode: bool,
}

pub fn setup_camera(commands: &mut Commands) {
    use super::GameEntity;
    let initial_transform = Transform::from_translation(Vec3::new(0.0, 25.0, 25.0)).looking_at(Vec3::ZERO, Vec3::Y);
    let (yaw, pitch, _) = initial_transform.rotation.to_euler(EulerRot::YXZ);
    
    commands.spawn((
        GameEntity,
        PlayerCamera {
            velocity: Vec3::ZERO,
            movement_mode: MovementMode::default(),
            yaw,
            pitch,
        },
        Camera3d::default(),
        Projection::Perspective(PerspectiveProjection {
            fov: (PI / 5.),
            ..default()
        }),
        initial_transform,
    ));
}

pub fn handle_movement(
    keyboard_input: Res<ButtonInput<KeyCode>>,
    mut mouse_motion: EventReader<MouseMotion>,
    mut query: Query<(&mut Transform, &mut PlayerCamera)>,
) {
    for (mut transform, mut camera) in query.iter_mut() {
        let input = collect_movement_input(&keyboard_input);
        
        handle_mode_toggle(&input, &mut camera, &mut transform);
        handle_mouse_look(&mut mouse_motion, &mut camera, &mut transform);
        apply_movement(&input, &mut transform, &camera);

        info!("Camera translation: {:?}", transform.translation);
        info!("Camera mode: {:?}", camera.movement_mode);
    }
}

fn collect_movement_input(keyboard_input: &Res<ButtonInput<KeyCode>>) -> MovementInput {
    MovementInput {
        forward: keyboard_input.pressed(KeyCode::KeyW),
        backward: keyboard_input.pressed(KeyCode::KeyS),
        left: keyboard_input.pressed(KeyCode::KeyA),
        right: keyboard_input.pressed(KeyCode::KeyD),
        up: keyboard_input.pressed(KeyCode::Space),
        down: keyboard_input.pressed(KeyCode::ControlLeft),
        toggle_mode: keyboard_input.just_pressed(KeyCode::KeyM) && keyboard_input.pressed(KeyCode::ShiftLeft),
    }
}

fn handle_mode_toggle(
    input: &MovementInput,
    camera: &mut PlayerCamera,
    transform: &mut Transform,
) {
    if input.toggle_mode {
        camera.movement_mode = match camera.movement_mode {
            MovementMode::FreeCam => {
                // Snap to floor when switching to floor-based
                transform.translation.y = FLOOR_HEIGHT;
                MovementMode::FloorBased
            },
            MovementMode::FloorBased => MovementMode::FreeCam,
        };
        info!("Switched to movement mode: {:?}", camera.movement_mode);
    }
}

fn handle_mouse_look(
    mouse_motion: &mut EventReader<MouseMotion>,
    camera: &mut PlayerCamera,
    transform: &mut Transform,
) {
    let mut mouse_delta = Vec2::ZERO;
    for motion in mouse_motion.read() {
        mouse_delta += motion.delta;
    }
    
    if mouse_delta != Vec2::ZERO {
        camera.yaw -= mouse_delta.x * MOUSE_SENSITIVITY;
        camera.pitch -= mouse_delta.y * MOUSE_SENSITIVITY;
        camera.pitch = camera.pitch.clamp(-PI / 2.0 + 0.01, PI / 2.0 - 0.01);
        
        // Apply rotation to transform
        transform.rotation = Quat::from_euler(EulerRot::YXZ, camera.yaw, camera.pitch, 0.0);
    }
}

fn apply_movement(
    input: &MovementInput,
    transform: &mut Transform,
    camera: &PlayerCamera,
) {
    match camera.movement_mode {
        MovementMode::FreeCam => apply_freecam_movement(input, transform),
        MovementMode::FloorBased => apply_floor_movement(input, transform),
    }
}

fn apply_freecam_movement(input: &MovementInput, transform: &mut Transform) {
    let mut movement = Vec3::ZERO;
    
    if input.forward {
        movement += transform.forward().as_vec3();
    }
    if input.backward {
        movement -= transform.forward().as_vec3();
    }
    if input.left {
        movement -= transform.right().as_vec3();
    }
    if input.right {
        movement += transform.right().as_vec3();
    }
    if input.up {
        movement += Vec3::Y;
    }
    if input.down {
        movement -= Vec3::Y;
    }
    
    // Normalize and apply movement
    if movement.length() > 0.0 {
        movement = movement.normalize() * SPEED;
    }
    
    transform.translation += movement;
}

fn apply_floor_movement(input: &MovementInput, transform: &mut Transform) {
    let mut movement = Vec3::ZERO;
    
    if input.forward {
        movement += transform.forward().as_vec3();
    }
    if input.backward {
        movement -= transform.forward().as_vec3();
    }
    if input.left {
        movement -= transform.right().as_vec3();
    }
    if input.right {
        movement += transform.right().as_vec3();
    }
    
    // Project movement to XZ plane and normalize
    movement.y = 0.0;
    if movement.length() > 0.0 {
        movement = movement.normalize() * SPEED;
    }
    
    // Apply movement
    transform.translation.x += movement.x;
    transform.translation.z += movement.z;
    
    // Keep camera at floor height
    transform.translation.y = FLOOR_HEIGHT;
}