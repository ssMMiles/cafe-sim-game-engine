use std::f32::consts::PI;

use bevy::prelude::*;

#[derive(Component, Debug, Default)]
pub struct PlayerCamera {
    pub velocity: Vec3,
}

const SPEED: f32 = 1.0;
const ACCELERATION: f32 = 0.2;
const MAX_VELOCITY: f32 = 1.0;

pub fn setup_camera(commands: &mut Commands) {
    commands.spawn((
        PlayerCamera::default(),
        Camera3d::default(),
        Projection::Perspective(PerspectiveProjection {
            fov: (PI / 5.),
            ..default()
        }),
        Transform::from_translation(Vec3::new(0.0, 25.0, 25.0)).looking_at(Vec3::ZERO, Vec3::Y),
    ));
}

pub fn handle_movement(
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