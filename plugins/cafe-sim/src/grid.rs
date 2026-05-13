use bevy::prelude::*;

use crate::placement::PlacementSelection;

pub const CAFE_WIDTH: i32 = 20;
pub const CAFE_DEPTH: i32 = 12;
pub const HALF_W: i32 = CAFE_WIDTH / 2;
pub const HALF_D: i32 = CAFE_DEPTH / 2;
pub const FLOOR_Y: f32 = 0.1;
pub const PIECE_SCALE: f32 = 0.5;

/// Where the player gets placed each time gameplay starts. Just outside the
/// placeable grid on the entrance side (+Z), so saved furniture can never
/// end up beneath the player. See `customer.rs::pick_queue_cell` for the
/// matching "front = +Z" convention.
pub const PLAYER_SPAWN: Vec3 = Vec3::new(0.0, FLOOR_Y, HALF_D as f32 + 1.5);

pub fn camera_floor_hit(cam: &Transform) -> Option<Vec3> {
    let f = cam.forward().as_vec3();
    if f.y >= -1e-4 {
        return None;
    }
    let t = (FLOOR_Y - cam.translation.y) / f.y;
    if t <= 0.0 {
        return None;
    }
    Some(cam.translation + f * t)
}

pub fn world_to_cell(p: Vec3) -> Option<(i32, i32)> {
    let i = (p.x + HALF_W as f32).floor() as i32;
    let j = (p.z + HALF_D as f32).floor() as i32;
    if (0..CAFE_WIDTH).contains(&i) && (0..CAFE_DEPTH).contains(&j) {
        Some((i, j))
    } else {
        None
    }
}

pub fn cell_center(i: i32, j: i32) -> Vec3 {
    Vec3::new(
        (i - HALF_W) as f32 + 0.5,
        FLOOR_Y,
        (j - HALF_D) as f32 + 0.5,
    )
}

pub fn draw_floor_grid(mut gizmos: Gizmos, selection: Res<PlacementSelection>) {
    if selection.piece.is_none() {
        return;
    }

    let half_w = HALF_W as f32;
    let half_d = HALF_D as f32;
    let y = FLOOR_Y + 0.01;
    let color = Color::srgba(0.35, 0.6, 1.0, 0.7);

    for i in -HALF_W..=HALF_W {
        let x = i as f32;
        gizmos.line(Vec3::new(x, y, -half_d), Vec3::new(x, y, half_d), color);
    }
    for i in -HALF_D..=HALF_D {
        let z = i as f32;
        gizmos.line(Vec3::new(-half_w, y, z), Vec3::new(half_w, y, z), color);
    }
}
