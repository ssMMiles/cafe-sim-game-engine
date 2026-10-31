//! The cafe shell: the level entity everything in this plugin is anchored to.
//!
//! The shell is **level data**, authored in the editor as the `cafe-shell`
//! prefab (`assets/prefabs/cafe-shell.prefab`): a group holding the shell
//! mesh (`Cafe.gltf`, carrying the `cafe-sim/cafe-shell` script) and a
//! "Player Spawn" empty (carrying `cafe-sim/spawn-point`). Place one instance
//! per level from the Assets tab's prefab entries. The host hydrates the two
//! bindings into [`CafeShell`] and [`CafeSpawnPoint`] markers, in both editor
//! and gameplay mode, and this plugin only ever *reads* them:
//!
//! - [`sync_cafe_anchor`] measures the shell — where it is, how it is turned
//!   and scaled, and how big its floor is — into [`CafeAnchor`], which every
//!   grid coordinate goes through. While no shell is loaded the anchor is
//!   *unresolved* and the main menu refuses to start a game: a save stores
//!   furniture as grid cells, so applying it before the shell is known would
//!   scatter the cafe around the world origin.
//! - [`player_start`] picks where the player stands when play begins: the
//!   spawn point if the level has one, else outside the shell's own door.
//!
//! The markers, not the prefab, are what the plugin looks for. A prefab
//! instance is the convenient way to author them together and keep every
//! level's shell in step, but a hand-placed shell mesh with the binding
//! attached (the catalog preset) works identically, and so does an unpacked
//! instance. The plugin cannot see prefab-ness — the engine keeps that on the
//! host side — and does not need to.
//!
//! History: the shell used to be spawned here, by plugin code, every time
//! gameplay started. That stopped working once plugins became inactive in
//! editor mode — the shell was absent while authoring, including from
//! lightmap bakes, where it is the main occluder in the scene. A later
//! `dress_cafe_shell` system assigned the shell's materials at runtime,
//! which was wrong twice over (white in the editor; plugin state written
//! onto a level entity that nothing unwinds on unload). `Cafe.gltf` carries
//! its own materials now. See `docs/PLUGIN_LEVEL_ENTITY_CONTRACT.md`.

use bevy::{camera::primitives::Aabb, log, math::Affine3A, prelude::*};

use crate::grid::{CafeAnchor, FLOOR_NODE, FloorPlan};

/// Marker hydrated onto the placed shell entity by the `cafe-shell` script.
#[derive(Component, Debug)]
pub struct CafeShell;

/// Marker hydrated onto an empty by the `spawn-point` script: where the
/// player appears when play begins, facing along the empty's forward (−Z)
/// axis. Optional; one per level is expected and the first found wins.
#[derive(Component, Debug)]
pub struct CafeSpawnPoint;

// ----------------------------------------------------------------------------
// Measuring the shell
// ----------------------------------------------------------------------------

/// An accumulating axis-aligned box in the shell's local space.
#[derive(Debug, Clone, Copy)]
struct Bounds {
    min: Vec3,
    max: Vec3,
    any: bool,
}

/// Which face of a measured box the furniture stands on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Surface {
    /// Top of the floor slab — what you walk on.
    Top,
    /// Bottom of the whole shell, used by the fallback: the union of every
    /// mesh reaches the top of the *walls*, which is no use as a floor.
    Bottom,
}

impl Default for Bounds {
    fn default() -> Self {
        Self {
            min: Vec3::splat(f32::INFINITY),
            max: Vec3::splat(f32::NEG_INFINITY),
            any: false,
        }
    }
}

impl Bounds {
    /// Fold in a mesh's bounding box, mapped into the shell's local space.
    /// All eight corners are mapped, because a rotated node's box is not a
    /// box any more once it has been transformed.
    fn add_aabb(&mut self, aabb: &Aabb, to_local: Affine3A) {
        let c = Vec3::from(aabb.center);
        let h = Vec3::from(aabb.half_extents);
        for sx in [-1.0, 1.0] {
            for sy in [-1.0, 1.0] {
                for sz in [-1.0, 1.0] {
                    let corner = to_local.transform_point3(c + h * Vec3::new(sx, sy, sz));
                    self.min = self.min.min(corner);
                    self.max = self.max.max(corner);
                    self.any = true;
                }
            }
        }
    }

    fn plan(&self, surface: Surface) -> FloorPlan {
        let y = match surface {
            Surface::Top => self.max.y,
            Surface::Bottom => self.min.y,
        };
        FloorPlan::fit(
            Vec2::new(self.min.x, self.min.z),
            Vec2::new(self.max.x, self.max.z),
            y,
        )
    }
}

/// Does this entity's name mark it as the floor? Bevy's glTF loader names a
/// node entity after the node (`"Floor"`) and its mesh child after the mesh
/// and material (`"Floor.Cafe Floor"`), so both forms count and the whole
/// subtree is taken.
fn is_floor_name(name: &str) -> bool {
    name == FLOOR_NODE
        || name
            .strip_prefix(FLOOR_NODE)
            .is_some_and(|rest| rest.starts_with('.'))
}

/// Lay the grid out on the shell's floor.
///
/// Walks the shell's spawned glTF hierarchy for the [`FLOOR_NODE`] subtree
/// and fits whole cells inside its footprint, in the shell's **local** units
/// — so the plan is the same whatever the shell's placement, and the scale
/// and rotation ride on the anchor's frame instead.
///
/// Falls back to the union of every mesh in the shell when there is no floor
/// node, which is wrong by up to the wall thickness but beats refusing to
/// start on a shell that isn't ours. Returns `None` while the glTF is still
/// streaming and there is nothing to measure.
fn measure_floor(
    shell: Entity,
    frame: &GlobalTransform,
    children: &Query<&Children>,
    meshes: &Query<(&Aabb, &GlobalTransform), With<Mesh3d>>,
    names: &Query<&Name>,
    warned_fallback: &mut bool,
) -> Option<FloorPlan> {
    let to_shell = frame.affine().inverse();
    let mut floor = Bounds::default();
    let mut whole = Bounds::default();

    let mut stack = vec![(shell, false)];
    while let Some((entity, inherited)) = stack.pop() {
        let is_floor = inherited
            || names
                .get(entity)
                .is_ok_and(|name| is_floor_name(name.as_str()));

        if let Ok((aabb, mesh_frame)) = meshes.get(entity) {
            let to_local = to_shell * mesh_frame.affine();
            whole.add_aabb(aabb, to_local);
            if is_floor {
                floor.add_aabb(aabb, to_local);
            }
        }

        if let Ok(kids) = children.get(entity) {
            stack.extend(kids.iter().map(|child| (child, is_floor)));
        }
    }

    if floor.any {
        *warned_fallback = false;
        return Some(floor.plan(Surface::Top));
    }
    if whole.any {
        if !*warned_fallback {
            *warned_fallback = true;
            log::warn!(
                "[cafe-sim] the cafe shell has no '{FLOOR_NODE}' node; sizing the placement \
                 grid from the whole shell's footprint, which includes its walls. Name the \
                 floor mesh '{FLOOR_NODE}' in the glTF to place furniture accurately."
            );
        }
        return Some(whole.plan(Surface::Bottom));
    }
    None
}

/// Mirror the placed shell's frame and floor size into [`CafeAnchor`].
///
/// Runs every frame rather than once: the shell can be grabbed, turned or
/// scaled in the editor, and it may stream in after the plugin has started.
/// Losing the shell (chunk unloaded, entity deleted) unresolves the anchor
/// but keeps the last frame and plan, so coordinate readers never see a jump.
pub fn sync_cafe_anchor(
    mut anchor: ResMut<CafeAnchor>,
    shell_q: Query<(Entity, &GlobalTransform), With<CafeShell>>,
    children_q: Query<&Children>,
    meshes_q: Query<(&Aabb, &GlobalTransform), With<Mesh3d>>,
    names_q: Query<&Name>,
    mut warned_missing: Local<bool>,
    mut warned_fallback: Local<bool>,
) {
    let Some((entity, frame)) = shell_q.iter().next() else {
        if anchor.shell().is_some() {
            *anchor = CafeAnchor::default();
        }
        if !*warned_missing {
            *warned_missing = true;
            log::warn!(
                "[cafe-sim] no cafe shell in this level — place the `cafe-shell` prefab from \
                 the editor's Assets tab and save with Shift+S. Gameplay waits until one is \
                 found."
            );
        }
        return;
    };
    *warned_missing = false;

    let next = match measure_floor(
        entity,
        frame,
        &children_q,
        &meshes_q,
        &names_q,
        &mut warned_fallback,
    ) {
        Some(plan) => CafeAnchor::placed(entity, *frame, plan),
        None => CafeAnchor::loading(entity, *frame),
    };

    // Write only on change, so `Res<CafeAnchor>::is_changed` means something
    // to the menu's status line.
    if *anchor != next {
        *anchor = next;
    }
}

// ----------------------------------------------------------------------------
// Player start
// ----------------------------------------------------------------------------

/// Where and which way the player faces when play begins.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlayerStart {
    pub position: Vec3,
    /// Yaw about +Y in the engine camera's convention (0 looks down −Z).
    pub yaw: f32,
}

/// The level's spawn point if it has one, else outside the shell's door
/// looking in.
pub fn player_start(anchor: &CafeAnchor, spawn_points: &[&GlobalTransform]) -> PlayerStart {
    match spawn_points.first() {
        Some(gt) => PlayerStart {
            position: gt.translation(),
            yaw: yaw_of(gt.forward().as_vec3()),
        },
        None => PlayerStart {
            position: anchor.player_spawn(),
            yaw: yaw_of(anchor.facing_in()),
        },
    }
}

/// Yaw that makes the camera's −Z forward point along `forward`'s ground
/// projection. Matches `Quat::from_euler(EulerRot::YXZ, yaw, pitch, 0.0)` in
/// the engine's look system. A direction straight up or down has no heading;
/// keep 0.
fn yaw_of(forward: Vec3) -> f32 {
    let flat = Vec2::new(forward.x, forward.z);
    if flat.length_squared() < 1e-8 {
        return 0.0;
    }
    (-flat.x).atan2(-flat.y)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::FRAC_PI_2;

    #[test]
    fn spawn_point_yaw_matches_the_camera_euler_convention() {
        for yaw in [0.0, FRAC_PI_2, -FRAC_PI_2, 2.5, -3.0] {
            let rotation = Quat::from_euler(EulerRot::YXZ, yaw, 0.0, 0.0);
            let gt = GlobalTransform::from(Transform::from_rotation(rotation));
            let start = player_start(&CafeAnchor::default(), &[&gt]);
            let diff = (start.yaw - yaw).rem_euclid(std::f32::consts::TAU);
            let diff = diff.min(std::f32::consts::TAU - diff);
            assert!(diff < 1e-4, "yaw {yaw} round-tripped as {}", start.yaw);
        }
    }

    #[test]
    fn a_vertical_spawn_point_keeps_a_zero_heading() {
        let gt = GlobalTransform::from(Transform::from_rotation(Quat::from_rotation_x(-FRAC_PI_2)));
        let start = player_start(&CafeAnchor::default(), &[&gt]);
        assert_eq!(start.yaw, 0.0);
    }

    /// Without a spawn point the player stands outside the shell's own door
    /// and looks in — which means following the shell's rotation, not +Z.
    #[test]
    fn the_door_fallback_follows_a_rotated_shell() {
        let anchor = CafeAnchor::detached(
            GlobalTransform::from(
                Transform::from_xyz(5.0, 0.0, 9.0).with_rotation(Quat::from_rotation_y(FRAC_PI_2)),
            ),
            FloorPlan::default(),
        );
        let start = player_start(&anchor, &[]);
        assert_eq!(start.position, anchor.player_spawn());

        // Looking in means walking toward the floor: step forward from the
        // spawn and you end up over the grid.
        let forward = Quat::from_euler(EulerRot::YXZ, start.yaw, 0.0, 0.0) * Vec3::NEG_Z;
        assert!(
            anchor
                .world_to_cell(start.position + forward * 2.0)
                .is_some(),
            "spawn faces away from the cafe"
        );
    }

    /// The one that matters: `Cafe.gltf`'s floor is a unit quad on a node
    /// scaled 10 × 6, and that has to come out as the 20 × 12 grid the game
    /// was written against.
    #[test]
    fn the_shipped_floor_node_measures_as_a_20_by_12_grid() {
        let mut bounds = Bounds::default();
        bounds.add_aabb(
            &Aabb::from_min_max(Vec3::new(-1.0, 0.0, -1.0), Vec3::new(1.0, 0.0, 1.0)),
            Affine3A::from_scale(Vec3::new(10.0, 1.0, 6.0)),
        );
        let plan = bounds.plan(Surface::Top);
        assert_eq!((plan.width, plan.depth), (20, 12));
        assert_eq!(plan.center, Vec3::ZERO);
    }

    /// A rotated floor node is measured by its corners, not by reusing its
    /// unrotated box: a quarter turn swaps the footprint's axes.
    #[test]
    fn a_rotated_floor_node_is_measured_through_its_corners() {
        let mut bounds = Bounds::default();
        bounds.add_aabb(
            &Aabb::from_min_max(Vec3::new(-10.0, 0.0, -6.0), Vec3::new(10.0, 0.0, 6.0)),
            Affine3A::from_rotation_y(FRAC_PI_2),
        );
        let plan = bounds.plan(Surface::Top);
        assert_eq!((plan.width, plan.depth), (12, 20));
    }

    /// Furniture stands on top of a thick floor slab, not inside it.
    #[test]
    fn a_thick_floor_puts_the_grid_on_its_top_face() {
        let mut bounds = Bounds::default();
        bounds.add_aabb(
            &Aabb::from_min_max(Vec3::new(-10.0, -0.5, -6.0), Vec3::new(10.0, 0.25, 6.0)),
            Affine3A::IDENTITY,
        );
        assert_eq!(bounds.plan(Surface::Top).center.y, 0.25);
    }

    /// The nameless-shell fallback takes the bottom of the union: the top
    /// would be the roofline.
    #[test]
    fn the_fallback_sizes_from_the_bottom_of_the_whole_shell() {
        let mut bounds = Bounds::default();
        bounds.add_aabb(
            &Aabb::from_min_max(Vec3::new(-10.0, 0.0, -6.0), Vec3::new(10.0, 0.0, 6.0)),
            Affine3A::IDENTITY,
        );
        // A wall standing on that floor.
        bounds.add_aabb(
            &Aabb::from_min_max(Vec3::new(-10.25, 0.0, 6.0), Vec3::new(10.25, 3.5, 6.25)),
            Affine3A::IDENTITY,
        );
        let plan = bounds.plan(Surface::Bottom);
        assert_eq!(plan.center.y, 0.0);
        // Rounded down off the wall overhang rather than over it.
        assert_eq!((plan.width, plan.depth), (20, 12));
    }

    #[test]
    fn floor_names_cover_the_node_and_its_mesh_child() {
        assert!(is_floor_name("Floor"));
        assert!(is_floor_name("Floor.Cafe Floor"));
        assert!(!is_floor_name("Floorboards"));
        assert!(!is_floor_name("Front Wall"));
    }
}
