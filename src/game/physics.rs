use avian3d::prelude::*;
use bevy::prelude::*;

use crate::{game::world::terrain::HeightmapData, settings::{MovementMode, UserSettings}};

use super::simulation::Player;

// Physics constants
const GRAVITY: f32 = 19.6;
const TURN_SPEED: f32 = 12.0; // radians/sec — snappy but not instant
const JUMP_VELOCITY: f32 = 8.0;
const MAX_SLOPE: f32 = 0.78; // ~45 degrees in radians
const GROUND_SNAP: f32 = 0.3;
const GROUND_ACCEL: f32 = 40.0;
const GROUND_DECEL: f32 = 60.0;
const AIR_ACCEL: f32 = 8.0;

/// Total height of the player capsule (including hemispherical caps).
pub const CHAR_HEIGHT: f32 = 1.8;
/// Radius of the player capsule.
pub const CHAR_RADIUS: f32 = 0.4;

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

/// Main character physics system. Only runs when movement mode is FloorBased.
pub fn character_physics(
    keyboard_input: Res<ButtonInput<KeyCode>>,
    settings: Res<UserSettings>,
    time: Res<Time>,
    heightmap: Option<Res<HeightmapData>>,
    mut player_query: Query<(Entity, &mut Transform, &mut CharacterState), With<Player>>,
    camera_query: Query<&Transform, (With<super::camera::PlayerCamera>, Without<Player>)>,
    spatial_query: SpatialQuery,
    mut capsule_cache: Local<Option<Collider>>,
) {
    if settings.movement_mode != MovementMode::FloorBased {
        return;
    }

    let dt = time.delta_secs();
    if dt <= 0.0 {
        return;
    }

    let Ok((player_entity, mut player_tf, mut state)) = player_query.single_mut() else {
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

    // 5. Move-and-slide — sweep before moving to avoid penetration entirely
    let capsule_shape = capsule_cache
        .get_or_insert_with(|| Collider::capsule(CHAR_RADIUS, CHAR_HEIGHT - 2.0 * CHAR_RADIUS));

    // 5a. Horizontal move-and-slide (up to 3 iterations for corner sliding)
    let h_disp = Vec3::new(state.velocity.x, 0.0, state.velocity.z) * dt;
    let mut remaining = h_disp;

    for _ in 0..3 {
        if remaining.length_squared() < 1e-8 {
            break;
        }
        let dist = remaining.length();
        let Ok(dir) = Dir3::new(remaining / dist) else { break };
        let capsule_center = player_tf.translation + Vec3::Y * (CHAR_HEIGHT * 0.5);

        if let Some(hit) = spatial_query.cast_shape(
            &capsule_shape,
            capsule_center,
            Quat::IDENTITY,
            dir,
            &ShapeCastConfig {
                max_distance: dist,
                ignore_origin_penetration: true,
                ..Default::default()
            },
            &SpatialQueryFilter::from_excluded_entities([player_entity]),
        ) {
            let wall_normal_h = Vec3::new(hit.normal2.x, 0.0, hit.normal2.z).normalize_or_zero();
            // A surface is a "wall" only if it's too steep to walk on. Walkable surfaces
            // (terrain slopes, object tops) are handled by ground detection — skip them here.
            let is_wall = wall_normal_h.length_squared() > 0.01
                && hit.normal2.y.abs() < MAX_SLOPE.cos();
            if is_wall {
                // Wall hit — advance to surface and slide along it
                player_tf.translation += dir.as_vec3() * (hit.distance - 0.001_f32).max(0.0);
                remaining -= remaining.dot(wall_normal_h) * wall_normal_h;
                let into_wall = state.velocity.dot(wall_normal_h).min(0.0);
                state.velocity.x -= wall_normal_h.x * into_wall;
                state.velocity.z -= wall_normal_h.z * into_wall;
            } else {
                // Walkable surface or floor/ceiling during horizontal sweep.
                // Apply remaining freely; vertical constraint is handled by ground detection.
                player_tf.translation += remaining;
                break;
            }
        } else {
            player_tf.translation += remaining;
            break;
        }
    }

    // 5b. Vertical movement (separate axis)
    player_tf.translation.y += state.velocity.y * dt;

    // 6. Ground resolve — terrain heightmap sampling (primary, fast path)
    let feet_y = player_tf.translation.y;
    let mut terrain_ground: Option<f32> = None;
    let mut terrain_normal: Option<(Vec3, f32)> = None;
    if let Some(ref hm) = heightmap {
        if let Some(h) = hm.sample_height(player_tf.translation.x, player_tf.translation.z) {
            terrain_ground = Some(h);
            terrain_normal = hm.sample_normal(player_tf.translation.x, player_tf.translation.z);
        } else {
            // Outside terrain bounds — clamp to edge
            let max_x = (hm.grid_w as f32 - 1.0) * hm.tile_size.0;
            let max_z = (hm.grid_h as f32 - 1.0) * hm.tile_size.1;
            player_tf.translation.x = player_tf.translation.x.clamp(0.0, max_x);
            player_tf.translation.z = player_tf.translation.z.clamp(0.0, max_z);
            // Re-sample after clamping
            if let Some(h) = hm.sample_height(player_tf.translation.x, player_tf.translation.z) {
                terrain_ground = Some(h);
                terrain_normal = hm.sample_normal(player_tf.translation.x, player_tf.translation.z);
            }
        }
    }

    // Downward shape-cast using avian's capsule to detect walkable surfaces (objects, etc.)
    // The capsule is cast from the player's mid-body downward to detect ground beneath feet.
    let cast_origin = player_tf.translation + Vec3::Y * (CHAR_HEIGHT * 0.5);
    let shape_ground: Option<f32> = spatial_query
        .cast_shape(
            &capsule_shape,
            cast_origin,
            Quat::IDENTITY,
            Dir3::NEG_Y,
            &ShapeCastConfig {
                max_distance: GROUND_SNAP + 0.5,
                ignore_origin_penetration: true,
                ..Default::default()
            },
            &SpatialQueryFilter::from_excluded_entities([player_entity]),
        )
        .map(|hit| feet_y - hit.distance);

    // Effective ground = highest of terrain and any collider surface
    let effective_ground = match (terrain_ground, shape_ground) {
        (Some(t), Some(s)) => Some(t.max(s)),
        (Some(t), None) => Some(t),
        (None, Some(s)) => Some(s),
        (None, None) => None,
    };

    if let Some(ground_h) = effective_ground {
        let diff = player_tf.translation.y - ground_h;
        if (0.0..GROUND_SNAP).contains(&diff) && state.velocity.y <= 0.0 {
            player_tf.translation.y = ground_h;
            state.velocity.y = 0.0;
            state.grounded = true;
        } else if diff < 0.0 {
            player_tf.translation.y = ground_h;
            state.velocity.y = 0.0;
            state.grounded = true;
        } else {
            state.grounded = false;
        }

        // 7. Slope check — only when terrain is the active ground surface
        if state.grounded {
            let terrain_is_ground = shape_ground.map_or(true, |sh| {
                terrain_ground.map_or(false, |th| th >= sh)
            });
            if terrain_is_ground {
                if let Some((normal, slope)) = terrain_normal {
                    state.ground_normal = normal;
                    if slope > MAX_SLOPE {
                        let slide_dir = Vec3::new(normal.x, 0.0, normal.z).normalize_or_zero();
                        state.velocity += slide_dir * GRAVITY * slope.sin() * dt;
                        state.grounded = false;
                    }
                }
            }
        }
    }

    // 8. Rotate player to face movement direction
    if wish_dir.length_squared() > 0.0 {
        let target_rot = Quat::from_rotation_arc(Vec3::Z, wish_dir);
        player_tf.rotation = player_tf.rotation.slerp(target_rot, (TURN_SPEED * dt).min(1.0));
    }
}
