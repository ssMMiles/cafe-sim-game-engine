use bevy::prelude::*;

use crate::placement::PlacementSelection;

pub const CAFE_WIDTH: i32 = 20;
pub const CAFE_DEPTH: i32 = 12;
pub const HALF_W: i32 = CAFE_WIDTH / 2;
pub const HALF_D: i32 = CAFE_DEPTH / 2;
/// Y the shell used to be spawned at, back when this plugin spawned it. Only
/// the [`CafeAnchor`] fallback uses it now — `Cafe.gltf`'s floor mesh sits at
/// its own scene origin, so a placed shell's floor is just its transform's Y.
pub const FLOOR_Y: f32 = 0.1;
pub const PIECE_SCALE: f32 = 0.5;

/// Where the cafe shell sits in the world.
///
/// The shell used to be spawned by this plugin at the world origin, so all
/// the grid maths could assume it. It is a placed level entity now (see
/// [`crate::cafe_scene`]), which can be dropped anywhere in the editor — so
/// every world-space coordinate the cafe computes has to be relative to
/// wherever the shell actually ended up, or the furniture grid would sit
/// somewhere other than the floor it belongs to.
///
/// Translation only: a rotated grid would mean rotating piece placement,
/// queue cells, and staff pathing too. [`sync_cafe_anchor`] warns if the
/// placed shell carries a rotation.
#[derive(Resource, Debug, Clone, Copy)]
pub struct CafeAnchor {
    pub origin: Vec3,
}

impl Default for CafeAnchor {
    /// World origin — what every world looked like before the shell became a
    /// placed entity, and the fallback when no shell has been placed.
    fn default() -> Self {
        Self {
            origin: Vec3::new(0.0, FLOOR_Y, 0.0),
        }
    }
}

impl CafeAnchor {
    /// Y of the cafe floor in world space.
    pub fn floor_y(&self) -> f32 {
        self.origin.y
    }

    /// Z of the entrance, just outside the front (+Z) edge of the grid.
    pub fn door_z(&self) -> f32 {
        self.origin.z + HALF_D as f32 + 1.0
    }

    /// Where the player is placed each time gameplay starts. Just outside the
    /// placeable grid on the entrance side, so saved furniture can never end
    /// up beneath the player. See `customer.rs::pick_queue_cell` for the
    /// matching "front = +Z" convention.
    ///
    /// Half a metre *beyond* the door, not short of it: `Cafe.gltf`'s front
    /// wall reaches `origin.z + 6.25`, which is only 0.75 inside `door_z()`,
    /// and the player capsule's 0.3 radius would spawn clipped into it.
    pub fn player_spawn(&self) -> Vec3 {
        Vec3::new(self.origin.x, self.floor_y(), self.door_z() + 0.5)
    }
}

pub fn camera_floor_hit(anchor: &CafeAnchor, cam: &Transform) -> Option<Vec3> {
    let f = cam.forward().as_vec3();
    if f.y >= -1e-4 {
        return None;
    }
    let t = (anchor.floor_y() - cam.translation.y) / f.y;
    if t <= 0.0 {
        return None;
    }
    Some(cam.translation + f * t)
}

pub fn world_to_cell(anchor: &CafeAnchor, p: Vec3) -> Option<(i32, i32)> {
    let i = (p.x - anchor.origin.x + HALF_W as f32).floor() as i32;
    let j = (p.z - anchor.origin.z + HALF_D as f32).floor() as i32;
    if (0..CAFE_WIDTH).contains(&i) && (0..CAFE_DEPTH).contains(&j) {
        Some((i, j))
    } else {
        None
    }
}

pub fn cell_center(anchor: &CafeAnchor, i: i32, j: i32) -> Vec3 {
    Vec3::new(
        anchor.origin.x + (i - HALF_W) as f32 + 0.5,
        anchor.floor_y(),
        anchor.origin.z + (j - HALF_D) as f32 + 0.5,
    )
}

pub fn draw_floor_grid(
    mut gizmos: Gizmos,
    selection: Res<PlacementSelection>,
    anchor: Res<CafeAnchor>,
) {
    if selection.piece.is_none() {
        return;
    }

    let half_w = HALF_W as f32;
    let half_d = HALF_D as f32;
    let o = anchor.origin;
    let y = anchor.floor_y() + 0.01;
    let color = Color::srgba(0.35, 0.6, 1.0, 0.7);

    for i in -HALF_W..=HALF_W {
        let x = o.x + i as f32;
        gizmos.line(
            Vec3::new(x, y, o.z - half_d),
            Vec3::new(x, y, o.z + half_d),
            color,
        );
    }
    for i in -HALF_D..=HALF_D {
        let z = o.z + i as f32;
        gizmos.line(
            Vec3::new(o.x - half_w, y, z),
            Vec3::new(o.x + half_w, y, z),
            color,
        );
    }
}
