use bevy::prelude::*;

use crate::settings::{MovementMode, UserSettings};
use crate::terrain::HeightmapData;

use super::simulation::Player;

// Physics constants
const GRAVITY: f32 = 19.6;
const JUMP_VELOCITY: f32 = 8.0;
const MAX_SLOPE: f32 = 0.78; // ~45 degrees in radians
const CHAR_HEIGHT: f32 = 1.8;
const CHAR_RADIUS: f32 = 0.4;
const GROUND_SNAP: f32 = 0.3;
const GROUND_ACCEL: f32 = 40.0;
const GROUND_DECEL: f32 = 60.0;
const AIR_ACCEL: f32 = 8.0;

#[derive(Component, Debug)]
pub struct CharacterState {
    pub velocity: Vec3,
    pub grounded: bool,
    pub ground_normal: Vec3,
}

impl Default for CharacterState {
    fn default() -> Self {
        Self {
            velocity: Vec3::ZERO,
            grounded: false,
            ground_normal: Vec3::Y,
        }
    }
}

#[derive(Component, Debug, Clone)]
pub enum Collider {
    Box { half_extents: Vec3 },
    Sphere { radius: f32 },
}

/// Main character physics system. Only runs when movement mode is FloorBased.
pub fn character_physics(
    keyboard_input: Res<ButtonInput<KeyCode>>,
    settings: Res<UserSettings>,
    time: Res<Time>,
    heightmap: Option<Res<HeightmapData>>,
    mut player_query: Query<(&mut Transform, &mut CharacterState), With<Player>>,
    camera_query: Query<&Transform, (With<super::camera::PlayerCamera>, Without<Player>)>,
    collider_query: Query<(&Transform, &Collider), Without<Player>>,
) {
    if settings.movement_mode != MovementMode::FloorBased {
        return;
    }

    let dt = time.delta_secs();
    if dt <= 0.0 {
        return;
    }

    let Ok((mut player_tf, mut state)) = player_query.single_mut() else {
        return;
    };
    let Ok(camera_tf) = camera_query.single() else {
        return;
    };

    // 1. Read input — WASD wish direction on XZ, oriented by camera yaw
    let mut wish_dir = Vec3::ZERO;
    if keyboard_input.pressed(KeyCode::KeyW) {
        wish_dir += camera_tf.forward().as_vec3();
    }
    if keyboard_input.pressed(KeyCode::KeyS) {
        wish_dir -= camera_tf.forward().as_vec3();
    }
    if keyboard_input.pressed(KeyCode::KeyA) {
        wish_dir -= camera_tf.right().as_vec3();
    }
    if keyboard_input.pressed(KeyCode::KeyD) {
        wish_dir += camera_tf.right().as_vec3();
    }
    // Project onto XZ plane
    wish_dir.y = 0.0;
    if wish_dir.length_squared() > 0.0 {
        wish_dir = wish_dir.normalize();
    }

    let jump_pressed = keyboard_input.pressed(KeyCode::Space);
    let sprint = keyboard_input.pressed(KeyCode::ShiftLeft);
    let max_speed = if sprint {
        settings.movement_speed * 80.0
    } else {
        settings.movement_speed * 40.0
    };

    // 2. Horizontal acceleration
    let horizontal_vel = Vec3::new(state.velocity.x, 0.0, state.velocity.z);
    let accel = if state.grounded { GROUND_ACCEL } else { AIR_ACCEL };

    if wish_dir.length_squared() > 0.0 {
        let target = wish_dir * max_speed;
        let diff = target - horizontal_vel;
        let step = diff.normalize_or_zero() * accel * dt;
        let new_horizontal = if step.length() > diff.length() {
            target
        } else {
            horizontal_vel + step
        };
        state.velocity.x = new_horizontal.x;
        state.velocity.z = new_horizontal.z;
    } else if state.grounded {
        // Decelerate when no input and grounded
        let speed = horizontal_vel.length();
        if speed > 0.0 {
            let new_speed = (speed - GROUND_DECEL * dt).max(0.0);
            let decel_vel = horizontal_vel.normalize() * new_speed;
            state.velocity.x = decel_vel.x;
            state.velocity.z = decel_vel.z;
        }
    }

    // 3. Gravity (skip if grounded and not jumping)
    if !state.grounded {
        state.velocity.y -= GRAVITY * dt;
    }

    // 4. Jump
    if state.grounded && jump_pressed {
        state.velocity.y = JUMP_VELOCITY;
        state.grounded = false;
    }

    // 5. Integrate position
    player_tf.translation += state.velocity * dt;

    // 6. Ground resolve — sample terrain
    let feet_y = player_tf.translation.y;
    if let Some(ref hm) = heightmap {
        if let Some(terrain_h) = hm.sample_height(player_tf.translation.x, player_tf.translation.z) {
            let diff = feet_y - terrain_h;

            if (0.0..GROUND_SNAP).contains(&diff) && state.velocity.y <= 0.0 {
                // Within snap range, snap to surface
                player_tf.translation.y = terrain_h;
                state.velocity.y = 0.0;
                state.grounded = true;
            } else if diff < 0.0 {
                // Below terrain, push up
                player_tf.translation.y = terrain_h;
                state.velocity.y = 0.0;
                state.grounded = true;
            } else {
                // Above snap range — airborne
                state.grounded = false;
            }

            // 7. Slope check
            if state.grounded
                && let Some((normal, slope)) = hm.sample_normal(player_tf.translation.x, player_tf.translation.z)
            {
                state.ground_normal = normal;
                if slope > MAX_SLOPE {
                    // Apply downhill slide force
                    let slide_dir = Vec3::new(normal.x, 0.0, normal.z).normalize_or_zero();
                    state.velocity += slide_dir * GRAVITY * slope.sin() * dt;
                    state.grounded = false;
                }
            }
        } else {
            // Outside terrain bounds — clamp to edge
            let max_x = (hm.grid_w as f32 - 1.0) * hm.tile_size.0;
            let max_z = (hm.grid_h as f32 - 1.0) * hm.tile_size.1;
            player_tf.translation.x = player_tf.translation.x.clamp(0.0, max_x);
            player_tf.translation.z = player_tf.translation.z.clamp(0.0, max_z);

            // Re-sample after clamping
            if let Some(terrain_h) = hm.sample_height(player_tf.translation.x, player_tf.translation.z)
                && player_tf.translation.y < terrain_h
            {
                player_tf.translation.y = terrain_h;
                state.velocity.y = 0.0;
                state.grounded = true;
            }
        }
    }

    // 8. Collision resolve — capsule vs world colliders (2 iterations)
    for _ in 0..2 {
        let pos = player_tf.translation;
        // Capsule represented as 3 spheres: bottom, center, top
        let bottom_center = pos + Vec3::Y * CHAR_RADIUS;
        let mid_center = pos + Vec3::Y * (CHAR_HEIGHT * 0.5);
        let top_center = pos + Vec3::Y * (CHAR_HEIGHT - CHAR_RADIUS);
        let sphere_centers = [bottom_center, mid_center, top_center];

        let mut deepest_push = Vec3::ZERO;
        let mut deepest_depth = 0.0f32;

        for (collider_tf, collider) in collider_query.iter() {
            match collider {
                Collider::Box { half_extents } => {
                    let box_pos = collider_tf.translation;

                    for &sphere_center in &sphere_centers {
                        if let Some((dir, depth)) = sphere_vs_aabb(sphere_center, CHAR_RADIUS, box_pos, *half_extents)
                            && depth > deepest_depth
                        {
                            deepest_depth = depth;
                            deepest_push = dir * depth;
                        }
                    }
                }
                Collider::Sphere { radius } => {
                    let sphere_pos = collider_tf.translation;
                    // Closest point on capsule axis to sphere center
                    let axis_bottom = pos + Vec3::Y * CHAR_RADIUS;
                    let axis_top = pos + Vec3::Y * (CHAR_HEIGHT - CHAR_RADIUS);
                    let closest = closest_point_on_segment(axis_bottom, axis_top, sphere_pos);
                    let combined_radius = CHAR_RADIUS + radius;
                    let diff = closest - sphere_pos;
                    let dist = diff.length();
                    if dist < combined_radius && dist > 0.0 {
                        let dir = diff / dist;
                        let depth = combined_radius - dist;
                        if depth > deepest_depth {
                            deepest_depth = depth;
                            deepest_push = dir * depth;
                        }
                    }
                }
            }
        }

        if deepest_depth > 0.0 {
            player_tf.translation += deepest_push;
            // Cancel velocity into the collision surface (slide along it)
            let push_dir = deepest_push.normalize();
            let vel_into_wall = state.velocity.dot(push_dir).min(0.0);
            state.velocity -= push_dir * vel_into_wall;
        } else {
            break;
        }
    }
}

/// Test a sphere against an AABB. Returns push direction and depth if penetrating.
fn sphere_vs_aabb(
    sphere_center: Vec3,
    sphere_radius: f32,
    box_center: Vec3,
    half_extents: Vec3,
) -> Option<(Vec3, f32)> {
    let box_min = box_center - half_extents;
    let box_max = box_center + half_extents;

    // Closest point on AABB to sphere center
    let closest = Vec3::new(
        sphere_center.x.clamp(box_min.x, box_max.x),
        sphere_center.y.clamp(box_min.y, box_max.y),
        sphere_center.z.clamp(box_min.z, box_max.z),
    );

    let diff = sphere_center - closest;
    let dist_sq = diff.length_squared();

    if dist_sq < sphere_radius * sphere_radius && dist_sq > 0.0 {
        let dist = dist_sq.sqrt();
        let dir = diff / dist;
        let depth = sphere_radius - dist;
        Some((dir, depth))
    } else if dist_sq == 0.0 {
        // Sphere center is inside the AABB — push out along shortest axis
        let to_min = sphere_center - box_min;
        let to_max = box_max - sphere_center;

        let mut min_dist = to_min.x;
        let mut push_dir = Vec3::NEG_X;

        if to_max.x < min_dist {
            min_dist = to_max.x;
            push_dir = Vec3::X;
        }
        if to_min.y < min_dist {
            min_dist = to_min.y;
            push_dir = Vec3::NEG_Y;
        }
        if to_max.y < min_dist {
            min_dist = to_max.y;
            push_dir = Vec3::Y;
        }
        if to_min.z < min_dist {
            min_dist = to_min.z;
            push_dir = Vec3::NEG_Z;
        }
        if to_max.z < min_dist {
            min_dist = to_max.z;
            push_dir = Vec3::Z;
        }

        Some((push_dir, min_dist + sphere_radius))
    } else {
        None
    }
}

/// Closest point on line segment AB to point P.
fn closest_point_on_segment(a: Vec3, b: Vec3, p: Vec3) -> Vec3 {
    let ab = b - a;
    let len_sq = ab.length_squared();
    if len_sq < f32::EPSILON {
        return a;
    }
    let t = ((p - a).dot(ab) / len_sq).clamp(0.0, 1.0);
    a + ab * t
}
