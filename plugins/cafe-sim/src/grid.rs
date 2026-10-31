//! The placement grid, and the frame it lives in.
//!
//! The cafe shell is a placed level entity (see [`crate::cafe_scene`]) that
//! can be dropped anywhere, turned to any angle and scaled. So the grid is
//! **not** world-axis-aligned maths with an origin bolted on: it is defined in
//! the shell's own local space, and [`CafeAnchor`] carries the shell→world
//! transform that every coordinate passes through.
//!
//! Its size is measured from the shell, not hardcoded: `Cafe.gltf` has a node
//! named [`FLOOR_NODE`] whose footprint is the floor the player walks on, and
//! [`FloorPlan::fit`] lays out as many whole cells as fit inside it. Swap the
//! shell for a bigger room and the grid grows to match.
//!
//! # Two kinds of height
//!
//! A cell is one *shell-local* unit across, so cells scale with the shell and
//! a cell index keeps meaning the same spot on the floor whatever the shell's
//! scale — which is what lets a save (which stores cell indices) survive the
//! shell being resized. Heights split into two cases, and picking the wrong
//! one is the easy mistake here:
//!
//! - [`CafeAnchor::to_world`] takes a shell-local point, so its height scales
//!   with the shell. Use it for anything that rides on the furniture — a drink
//!   on a counter has to rise with the counter when the room is scaled up.
//! - [`CafeAnchor::above_floor`] lifts by world metres along the floor's
//!   normal. Use it for characters: customers and staff are the same size as
//!   the engine-spawned player, who does not scale with the shell, so their
//!   clearances are absolute.

use bevy::prelude::*;

use crate::placement::PlacementSelection;

/// Side of one placement cell, in the shell's local units.
pub const CELL_SIZE: f32 = 1.0;

/// Scale applied to a furniture mesh, relative to its cell. Composed with the
/// shell's own scale by [`CafeAnchor::place`].
pub const PIECE_SCALE: f32 = 0.5;

/// Name of the glTF node whose footprint is the placeable floor.
///
/// Bevy's glTF loader names the node entity after the node and its mesh
/// child after the mesh and material (`"Floor.Cafe Floor"` here), so the
/// measurement matches this name as a whole subtree — see
/// [`crate::cafe_scene::measure_floor`].
pub const FLOOR_NODE: &str = "Floor";

/// Footprint used until a shell has been measured: `Cafe.gltf`'s floor is a
/// unit quad scaled 10 × 6, i.e. 20 × 12 cells.
pub const DEFAULT_WIDTH: i32 = 20;
pub const DEFAULT_DEPTH: i32 = 12;

/// How far beyond the grid's front (+Z) edge the entrance line sits, in cells.
const DOOR_MARGIN: f32 = 1.0;

/// The grid laid out on a shell's floor, in shell-local units.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FloorPlan {
    /// Grid centre, with `y` on the floor surface.
    pub center: Vec3,
    /// Cells along local X.
    pub width: i32,
    /// Cells along local Z.
    pub depth: i32,
}

impl Default for FloorPlan {
    fn default() -> Self {
        Self {
            center: Vec3::ZERO,
            width: DEFAULT_WIDTH,
            depth: DEFAULT_DEPTH,
        }
    }
}

impl FloorPlan {
    /// Fit whole cells inside a measured floor surface, centred on it.
    /// `min`/`max` are the surface's extent on the local X and Z axes;
    /// `surface_y` is the local height the furniture stands on.
    pub fn fit(min: Vec2, max: Vec2, surface_y: f32) -> Self {
        Self {
            center: Vec3::new((min.x + max.x) * 0.5, surface_y, (min.y + max.y) * 0.5),
            width: cells_across(max.x - min.x),
            depth: cells_across(max.y - min.y),
        }
    }
}

/// Whole cells spanning `extent`, rounded **down** so the grid never spills
/// over the edge of the floor it was measured from. The epsilon is for the
/// rounding only: a 20-unit floor measured through two matrices comes back as
/// 19.999998, which would otherwise lose a whole column.
fn cells_across(extent: f32) -> i32 {
    (((extent / CELL_SIZE) + 1e-3).floor() as i32).max(1)
}

/// Where the cafe is, and how big its floor is.
///
/// Unresolved until a shell is loaded: saved furniture is stored as grid
/// cells, so expanding a save against an unresolved anchor would scatter it
/// around the world origin. The frame and plan keep their last resolved
/// values (or the defaults) so coordinate readers never see a jump.
#[derive(Resource, Debug, Clone, Copy, PartialEq)]
pub struct CafeAnchor {
    shell: Option<Entity>,
    /// Whether `plan` was measured from that shell, rather than defaulted. A
    /// shell whose glTF is still streaming has no bounds to measure yet.
    measured: bool,
    /// Shell-local → world. Carries the shell's position, rotation and scale.
    frame: GlobalTransform,
    plan: FloorPlan,
}

impl Default for CafeAnchor {
    fn default() -> Self {
        Self {
            shell: None,
            measured: false,
            frame: GlobalTransform::IDENTITY,
            plan: FloorPlan::default(),
        }
    }
}

impl CafeAnchor {
    /// An anchor backed by a live shell whose floor has been measured.
    pub fn placed(shell: Entity, frame: GlobalTransform, plan: FloorPlan) -> Self {
        Self {
            shell: Some(shell),
            measured: true,
            frame,
            plan,
        }
    }

    /// A shell is in the level but its geometry hasn't loaded far enough to
    /// measure. Not resolved: the grid's size isn't known yet.
    pub fn loading(shell: Entity, frame: GlobalTransform) -> Self {
        Self {
            shell: Some(shell),
            measured: false,
            frame,
            plan: FloorPlan::default(),
        }
    }

    /// An anchor positioned like a shell but with no live entity behind it —
    /// unresolved, so gameplay stays blocked.
    #[cfg(test)]
    pub fn detached(frame: GlobalTransform, plan: FloorPlan) -> Self {
        Self {
            shell: None,
            measured: false,
            frame,
            plan,
        }
    }

    /// Whether a placed cafe shell is loaded, measured, and anchoring the
    /// grid. Gameplay waits for this.
    pub fn is_resolved(&self) -> bool {
        self.shell.is_some() && self.measured
    }

    /// A shell is present but hasn't finished loading.
    pub fn is_loading(&self) -> bool {
        self.shell.is_some() && !self.measured
    }

    pub fn shell(&self) -> Option<Entity> {
        self.shell
    }

    pub fn width(&self) -> i32 {
        self.plan.width
    }

    pub fn depth(&self) -> i32 {
        self.plan.depth
    }

    /// Shell-local centre of the grid, on the floor surface.
    pub fn center_local(&self) -> Vec3 {
        self.plan.center
    }

    pub fn contains(&self, cell: (i32, i32)) -> bool {
        (0..self.plan.width).contains(&cell.0) && (0..self.plan.depth).contains(&cell.1)
    }

    /// Shell-local point → world. Heights scale with the shell.
    pub fn to_world(self, local: Vec3) -> Vec3 {
        self.frame.transform_point(local)
    }

    /// World point → shell-local.
    pub fn to_local(self, world: Vec3) -> Vec3 {
        self.frame.affine().inverse().transform_point3(world)
    }

    /// Compose a shell-local transform into world space, so what it places
    /// takes the shell's rotation and scale. Furniture goes through this.
    pub fn place(&self, local: Transform) -> Transform {
        self.frame.mul_transform(local).compute_transform()
    }

    /// Unit world normal of the floor plane (the shell's local up).
    pub fn up(&self) -> Vec3 {
        (self.frame.rotation() * Vec3::Y).normalize_or(Vec3::Y)
    }

    /// Unit world direction from the entrance into the room (local −Z),
    /// flattened to the floor plane.
    pub fn facing_in(&self) -> Vec3 {
        (self.frame.rotation() * Vec3::NEG_Z).normalize_or(Vec3::NEG_Z)
    }

    /// World point `metres` above the floor, over shell-local `local` (whose
    /// own height is ignored). For characters — see the module docs.
    pub fn above_floor(&self, local: Vec3, metres: f32) -> Vec3 {
        self.to_world(local.with_y(self.plan.center.y)) + self.up() * metres
    }

    /// Shell-local centre of a cell, on the floor surface.
    pub fn cell_local(&self, i: i32, j: i32) -> Vec3 {
        Vec3::new(
            self.plan.center.x + (i as f32 + 0.5 - self.plan.width as f32 * 0.5) * CELL_SIZE,
            self.plan.center.y,
            self.plan.center.z + (j as f32 + 0.5 - self.plan.depth as f32 * 0.5) * CELL_SIZE,
        )
    }

    /// Shell-local Z of the entrance line, just outside the front (+Z) edge of
    /// the grid. `customer.rs::pick_queue_cell` shares the "front = +Z"
    /// convention.
    pub fn door_local_z(&self) -> f32 {
        self.plan.center.z + (self.plan.depth as f32 * 0.5 + DOOR_MARGIN) * CELL_SIZE
    }

    /// Shell-local point on the entrance line, at local X `x`.
    pub fn door_local(&self, x: f32) -> Vec3 {
        Vec3::new(x, self.plan.center.y, self.door_local_z())
    }

    /// Which cell a world point is over, if any.
    pub fn world_to_cell(&self, world: Vec3) -> Option<(i32, i32)> {
        self.local_to_cell(self.to_local(world))
    }

    /// Which cell a shell-local point is over, if any.
    pub fn local_to_cell(&self, local: Vec3) -> Option<(i32, i32)> {
        let i = ((local.x - self.plan.center.x) / CELL_SIZE + self.plan.width as f32 * 0.5).floor()
            as i32;
        let j = ((local.z - self.plan.center.z) / CELL_SIZE + self.plan.depth as f32 * 0.5).floor()
            as i32;
        self.contains((i, j)).then_some((i, j))
    }

    /// Where the camera's forward ray meets the floor plane, looking at its
    /// front face. Returns `None` when the ray is parallel to the floor, aimed
    /// away from it, or hitting it from underneath.
    pub fn camera_floor_hit(&self, cam: &Transform) -> Option<Vec3> {
        let normal = self.up();
        let forward = cam.forward().as_vec3();
        let denom = forward.dot(normal);
        if denom >= -1e-4 {
            return None;
        }
        let t = (self.to_world(self.plan.center) - cam.translation).dot(normal) / denom;
        (t > 0.0).then(|| cam.translation + forward * t)
    }

    /// Where the player is placed when gameplay starts and the level has no
    /// `spawn-point` (see [`crate::cafe_scene::player_start`]). Outside the
    /// grid on the entrance side, so saved furniture can never end up beneath
    /// the player.
    ///
    /// Half a cell *beyond* the door line, not short of it: `Cafe.gltf`'s
    /// front wall stands 0.25 outside the floor edge, which is inside the
    /// door line, and the player capsule's 0.3 radius would spawn clipped
    /// into it.
    pub fn player_spawn(&self) -> Vec3 {
        self.to_world(self.door_local(self.plan.center.x) + Vec3::Z * 0.5 * CELL_SIZE)
    }
}

/// Move `position` toward `target` at `speed` m/s, staying in the floor
/// plane. Returns true once it is within `arrive` of the target.
///
/// The step is projected onto the plane rather than taken in world XZ, so a
/// rotated shell doesn't walk its customers through the floor. Speed is in
/// world metres: characters match the player, who doesn't scale with the
/// shell.
pub fn step_along_floor(
    anchor: &CafeAnchor,
    position: &mut Vec3,
    target: Vec3,
    speed: f32,
    dt: f32,
    arrive: f32,
) -> bool {
    let up = anchor.up();
    let to_target = target - *position;
    let flat = to_target - up * to_target.dot(up);
    let dist = flat.length();
    if dist <= arrive {
        return true;
    }
    *position += (flat / dist) * speed * dt;
    false
}

pub fn draw_floor_grid(
    mut gizmos: Gizmos,
    selection: Res<PlacementSelection>,
    anchor: Res<CafeAnchor>,
) {
    if selection.piece.is_none() {
        return;
    }

    let color = Color::srgba(0.35, 0.6, 1.0, 0.7);
    // A hair above the floor in local units, so it clears the surface by the
    // same visual margin however the shell is scaled.
    let lift = Vec3::Y * 0.01;
    let corner =
        |i: i32, j: i32| anchor.to_world(anchor.cell_local(i, j) + lift - cell_centre_bias());

    for i in 0..=anchor.width() {
        gizmos.line(corner(i, 0), corner(i, anchor.depth()), color);
    }
    for j in 0..=anchor.depth() {
        gizmos.line(corner(0, j), corner(anchor.width(), j), color);
    }
}

/// `cell_local` returns a cell's centre; the grid lines run along its corners.
fn cell_centre_bias() -> Vec3 {
    Vec3::new(0.5 * CELL_SIZE, 0.0, 0.5 * CELL_SIZE)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::FRAC_PI_2;

    /// World centre of a cell, on the floor surface.
    fn cell_world(anchor: &CafeAnchor, i: i32, j: i32) -> Vec3 {
        anchor.to_world(anchor.cell_local(i, j))
    }

    /// `Cafe.gltf`'s floor: a unit quad scaled 10 × 6, measured in shell-local
    /// units.
    fn cafe_floor() -> FloorPlan {
        FloorPlan::fit(Vec2::new(-10.0, -6.0), Vec2::new(10.0, 6.0), 0.0)
    }

    #[test]
    fn the_shipped_floor_measures_as_the_grid_we_hardcoded() {
        let plan = cafe_floor();
        assert_eq!((plan.width, plan.depth), (DEFAULT_WIDTH, DEFAULT_DEPTH));
        assert_eq!(plan.center, Vec3::ZERO);
    }

    /// Measuring through two matrices loses a few ulps; a floor that is
    /// 20 units wide must not come back as 19 cells.
    #[test]
    fn a_whole_floor_measured_with_float_error_keeps_all_its_cells() {
        let plan = FloorPlan::fit(Vec2::new(-9.999_999, -6.0), Vec2::new(9.999_999, 6.0), 0.0);
        assert_eq!(plan.width, 20);
    }

    /// A floor that isn't a whole number of cells gives up the remainder
    /// rather than overhanging its own edge.
    #[test]
    fn a_partial_cell_is_dropped_and_the_grid_stays_centred() {
        let plan = FloorPlan::fit(Vec2::new(0.0, 0.0), Vec2::new(20.5, 12.5), 0.0);
        assert_eq!((plan.width, plan.depth), (20, 12));
        assert_eq!(plan.center, Vec3::new(10.25, 0.0, 6.25));

        // Every cell centre is inside the measured surface.
        let anchor = CafeAnchor::detached(GlobalTransform::IDENTITY, plan);
        for (i, j) in [(0, 0), (19, 11), (0, 11), (19, 0)] {
            let c = anchor.cell_local(i, j);
            assert!(
                (0.0..=20.5).contains(&c.x) && (0.0..=12.5).contains(&c.z),
                "cell ({i},{j}) at {c:?} escaped the floor"
            );
        }
    }

    fn rotated_and_scaled() -> CafeAnchor {
        CafeAnchor::detached(
            GlobalTransform::from(
                Transform::from_xyz(30.0, 4.0, -12.0)
                    .with_rotation(Quat::from_rotation_y(FRAC_PI_2))
                    .with_scale(Vec3::splat(2.0)),
            ),
            cafe_floor(),
        )
    }

    #[test]
    fn every_cell_round_trips_through_a_rotated_scaled_shell() {
        let anchor = rotated_and_scaled();
        for i in 0..anchor.width() {
            for j in 0..anchor.depth() {
                let world = cell_world(&anchor, i, j);
                assert_eq!(
                    anchor.world_to_cell(world),
                    Some((i, j)),
                    "cell ({i},{j}) at {world:?} did not come back"
                );
            }
        }
    }

    #[test]
    fn cells_scale_with_the_shell_so_they_still_cover_its_floor() {
        let anchor = rotated_and_scaled();
        // Adjacent cells are one *scaled* unit apart, i.e. the grid still
        // spans the whole 40 × 24 m floor of a shell scaled 2x.
        let step = cell_world(&anchor, 1, 0) - cell_world(&anchor, 0, 0);
        assert!((step.length() - 2.0).abs() < 1e-4, "cell pitch {step:?}");

        let across = cell_world(&anchor, 19, 0) - cell_world(&anchor, 0, 0);
        assert!(
            (across.length() - 38.0).abs() < 1e-3,
            "grid width {across:?}"
        );
    }

    #[test]
    fn a_point_outside_the_floor_is_not_a_cell() {
        let anchor = rotated_and_scaled();
        let outside = anchor.to_world(Vec3::new(10.5, 0.0, 0.0));
        assert_eq!(anchor.world_to_cell(outside), None);
    }

    /// Characters are player-sized whatever the shell's scale, so their
    /// clearance is world metres — while the point underneath them still
    /// follows the scaled, rotated grid.
    #[test]
    fn character_clearance_is_world_metres_not_shell_units() {
        let anchor = rotated_and_scaled();
        let cell = anchor.cell_local(3, 4);
        let floor = cell_world(&anchor, 3, 4);
        let floating = anchor.above_floor(cell, 0.6);
        assert!(
            ((floating - floor).length() - 0.6).abs() < 1e-4,
            "lifted {:?} above the floor",
            (floating - floor).length()
        );
        assert!((floating - floor).normalize().dot(anchor.up()) > 0.999);
    }

    /// Furniture-relative heights do scale: a drink sits on a counter that
    /// got taller with the room.
    #[test]
    fn furniture_heights_scale_with_the_shell() {
        let anchor = rotated_and_scaled();
        let on_counter = anchor.to_world(anchor.cell_local(3, 4) + Vec3::Y * 1.0);
        let floor = cell_world(&anchor, 3, 4);
        assert!(((on_counter - floor).length() - 2.0).abs() < 1e-4);
    }

    #[test]
    fn placed_furniture_inherits_the_shells_rotation_and_scale() {
        let anchor = rotated_and_scaled();
        let piece = anchor.place(
            Transform::from_translation(anchor.cell_local(0, 0))
                .with_scale(Vec3::splat(PIECE_SCALE)),
        );
        assert_eq!(piece.translation, cell_world(&anchor, 0, 0));
        assert!((piece.scale - Vec3::splat(PIECE_SCALE * 2.0)).length() < 1e-5);
        // Axis-by-axis rather than `Quat::angle_between`, whose `acos_approx`
        // is only accurate to ~1e-3 near zero and can't show a match this
        // close.
        let turned = Quat::from_rotation_y(FRAC_PI_2);
        for axis in [Vec3::X, Vec3::Y, Vec3::Z] {
            assert!((piece.rotation * axis - turned * axis).length() < 1e-5);
        }
    }

    #[test]
    fn the_camera_ray_meets_a_tilted_floor() {
        // A shell tipped about X: a world-axis-aligned plane test would miss.
        let anchor = CafeAnchor::detached(
            GlobalTransform::from(Transform::from_rotation(Quat::from_rotation_x(0.3))),
            cafe_floor(),
        );
        let target = cell_world(&anchor, 5, 5);
        let eye = target + anchor.up() * 6.0 + Vec3::X * 2.0;
        let cam = Transform::from_translation(eye).looking_at(target, Vec3::Y);

        let hit = anchor.camera_floor_hit(&cam).expect("ray meets the floor");
        assert!((hit - target).length() < 1e-3, "hit {hit:?} vs {target:?}");
        assert_eq!(anchor.world_to_cell(hit), Some((5, 5)));
    }

    #[test]
    fn a_camera_looking_away_from_the_floor_misses_it() {
        let anchor = rotated_and_scaled();
        let eye = cell_world(&anchor, 5, 5) + anchor.up() * 3.0;
        let cam = Transform::from_translation(eye).looking_at(eye + anchor.up(), Vec3::Z);
        assert_eq!(anchor.camera_floor_hit(&cam), None);
    }

    #[test]
    fn the_player_spawns_outside_the_grid_on_the_entrance_side() {
        let anchor = rotated_and_scaled();
        let spawn = anchor.player_spawn();
        assert_eq!(anchor.world_to_cell(spawn), None);

        // Beyond the front edge, measured along the shell's own +Z.
        let local = anchor.to_local(spawn);
        assert!(local.z > anchor.door_local_z());
    }

    #[test]
    fn walking_stays_in_the_floor_plane() {
        let anchor = CafeAnchor::detached(
            GlobalTransform::from(Transform::from_rotation(Quat::from_rotation_x(0.3))),
            cafe_floor(),
        );
        let start = anchor.above_floor(anchor.cell_local(0, 0), 0.6);
        let target = anchor.above_floor(anchor.cell_local(19, 0), 0.6);

        let mut pos = start;
        for _ in 0..600 {
            if step_along_floor(&anchor, &mut pos, target, 2.0, 1.0 / 60.0, 0.1) {
                break;
            }
            let height = (pos - anchor.to_world(anchor.to_local(pos).with_y(0.0))).dot(anchor.up());
            assert!(
                (height - 0.6).abs() < 1e-3,
                "drifted to {height} above the floor"
            );
        }
        assert!((pos - target).length() <= 0.1, "never arrived: {pos:?}");
    }
}
