//! Transform gizmo for the Select tool: drag an axis arrow to translate the
//! selection, hold **Ctrl** for rotation arcs (Ctrl is free in Select — the
//! Ctrl+LMB terrain conflict is Terrain-tool-only).
//!
//! Hand-rolled on bevy's immediate-mode `Gizmos` plus ray math against
//! [`EditorRay`]: the pinned bevy_egui/egui_dock versions make third-party
//! gizmo crates a dependency hazard, and the editor only needs single-axis
//! world handles. Dragging mutates `Transform` directly and reports it with
//! `PlacedEdited` every frame (the commit path is cheap and keeps group
//! member deltas correct); Escape restores the transform the drag started
//! from. Drag state lives in [`GizmoInteraction`], never in `ViewportInput`,
//! which is rebuilt from scratch every egui pass.

use bevy::prelude::*;

use super::edits::{EditKind, PlacedEdited};
use super::picking::{EditorRay, ViewportInput};
use super::selection::Selection;
use crate::game::gfx::camera::PlayerCamera;
use crate::game::world::objects::PlacedObject;

/// Arrow length as a fraction of the camera distance, so the handles keep a
/// constant screen size.
const ARROW_SCALE: f32 = 0.18;
/// Arrows start a little off the origin so the three don't pile up on it.
const ARROW_INSET: f32 = 0.2;
/// Rotation arc radius as a fraction of the camera distance.
const ARC_SCALE: f32 = 0.16;
/// Pick threshold around a handle, as a fraction of the camera distance.
const PICK_SCALE: f32 = 0.02;
const HIGHLIGHT: Color = Color::srgb(1.0, 0.95, 0.4);

/// Config group so the handles draw over scene geometry with fatter lines
/// than the default (selection boxes, light gizmos) group.
#[derive(Default, Reflect, GizmoConfigGroup)]
pub struct EditorGizmoGroup;

pub fn configure_editor_gizmos(mut store: ResMut<GizmoConfigStore>) {
    let (config, _) = store.config_mut::<EditorGizmoGroup>();
    config.line.width = 3.0;
    // Fully in front: a handle must be grabbable even inside geometry.
    config.depth_bias = -1.0;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GizmoMode {
    Translate,
    Rotate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GizmoAxis {
    X,
    Y,
    Z,
}

impl GizmoAxis {
    const ALL: [GizmoAxis; 3] = [GizmoAxis::X, GizmoAxis::Y, GizmoAxis::Z];

    fn dir(self) -> Vec3 {
        match self {
            GizmoAxis::X => Vec3::X,
            GizmoAxis::Y => Vec3::Y,
            GizmoAxis::Z => Vec3::Z,
        }
    }

    fn color(self) -> Color {
        match self {
            GizmoAxis::X => Color::srgb(0.92, 0.28, 0.28),
            GizmoAxis::Y => Color::srgb(0.35, 0.85, 0.35),
            GizmoAxis::Z => Color::srgb(0.3, 0.5, 0.95),
        }
    }

    /// Rotation taking the gizmo circle's local plane (XY, normal +Z) onto
    /// the plane perpendicular to this axis.
    fn arc_rotation(self) -> Quat {
        match self {
            GizmoAxis::X => Quat::from_rotation_y(std::f32::consts::FRAC_PI_2),
            GizmoAxis::Y => Quat::from_rotation_x(-std::f32::consts::FRAC_PI_2),
            GizmoAxis::Z => Quat::IDENTITY,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct GizmoDrag {
    entity: Entity,
    axis: GizmoAxis,
    mode: GizmoMode,
    /// Transform when the drag began; restored on Escape.
    start: Transform,
    /// Translate: axis parameter of the grab point.
    start_t: f32,
    /// Rotate: reference direction in the rotation plane.
    start_dir: Vec3,
}

#[derive(Resource, Default, Debug)]
pub struct GizmoInteraction {
    pub hovered: Option<(GizmoAxis, GizmoMode)>,
    pub drag: Option<GizmoDrag>,
    /// Set by `escape_cancels`; consumed here by restoring the start
    /// transform.
    pub cancel_requested: bool,
}

/// Run condition: the pointer isn't on (or dragging) a handle, so a viewport
/// click still means "select".
pub fn gizmo_pointer_free(gizmo: Res<GizmoInteraction>) -> bool {
    gizmo.hovered.is_none() && gizmo.drag.is_none()
}

/// `OnExit(EditorTool::Select)`: no stale hover/drag may linger into another
/// tool (it would keep suppressing `select_on_click` on re-entry).
pub fn reset_gizmo(mut gizmo: ResMut<GizmoInteraction>) {
    *gizmo = GizmoInteraction::default();
}

fn ctrl_held(keys: &ButtonInput<KeyCode>) -> bool {
    keys.pressed(KeyCode::ControlLeft) || keys.pressed(KeyCode::ControlRight)
}

/// Parameter along the line through `origin` with unit direction `axis` of
/// the point closest to `ray`. `None` when nearly parallel.
fn axis_ray_param(origin: Vec3, axis: Vec3, ray: Ray3d) -> Option<f32> {
    let d = ray.direction.as_vec3();
    let w = origin - ray.origin;
    let b = axis.dot(d);
    let denom = 1.0 - b * b;
    if denom < 1e-4 {
        return None;
    }
    Some((b * d.dot(w) - axis.dot(w)) / denom)
}

/// Distance from `ray` to `point`, treating the ray as starting at its origin.
fn ray_point_distance(ray: Ray3d, point: Vec3) -> f32 {
    let d = ray.direction.as_vec3();
    let s = (point - ray.origin).dot(d).max(0.0);
    point.distance(ray.origin + d * s)
}

/// Distance from `ray` to the segment `a..b`.
fn segment_ray_distance(a: Vec3, b: Vec3, ray: Ray3d) -> f32 {
    let len = a.distance(b);
    let axis = (b - a) / len;
    let t = axis_ray_param(a, axis, ray).unwrap_or(0.0).clamp(0.0, len);
    ray_point_distance(ray, a + axis * t)
}

/// Where `ray` crosses the plane with `normal` through `center`.
fn plane_hit(center: Vec3, normal: Vec3, ray: Ray3d) -> Option<Vec3> {
    let d = ray.direction.as_vec3();
    let denom = d.dot(normal);
    if denom.abs() < 1e-4 {
        return None;
    }
    let s = (center - ray.origin).dot(normal) / denom;
    (s > 0.0).then(|| ray.origin + d * s)
}

/// Unit direction from `center` to the plane crossing.
fn plane_hit_dir(center: Vec3, normal: Vec3, ray: Ray3d) -> Option<Vec3> {
    let v = plane_hit(center, normal, ray)? - center;
    (v.length_squared() > 1e-8).then(|| v.normalize())
}

/// Signed angle from `from` to `to` around `axis` (all unit, `from`/`to` in
/// the plane perpendicular to `axis`).
fn signed_angle(from: Vec3, to: Vec3, axis: Vec3) -> f32 {
    from.cross(to).dot(axis).atan2(from.dot(to))
}

/// Handle sizes at the selection's distance from the camera.
fn handle_sizes(cam_pos: Vec3, origin: Vec3) -> (f32, f32, f32) {
    let dist = cam_pos.distance(origin).max(0.5);
    (dist * ARROW_SCALE, dist * ARC_SCALE, dist * PICK_SCALE)
}

/// Hover detection, drag start/update/end, and Escape cancel.
pub fn update_transform_gizmo(
    keys: Res<ButtonInput<KeyCode>>,
    mouse: Res<ButtonInput<MouseButton>>,
    viewport: Res<ViewportInput>,
    ray: Res<EditorRay>,
    selection: Res<Selection>,
    camera: Query<&GlobalTransform, With<PlayerCamera>>,
    mut transforms: Query<&mut Transform, With<PlacedObject>>,
    mut gizmo: ResMut<GizmoInteraction>,
    mut edits: MessageWriter<PlacedEdited>,
) {
    if std::mem::take(&mut gizmo.cancel_requested)
        && let Some(drag) = gizmo.drag.take()
        && let Ok(mut transform) = transforms.get_mut(drag.entity)
    {
        *transform = drag.start;
        edits.write(PlacedEdited {
            entity: drag.entity,
            what: EditKind::Transform,
        });
    }

    let Some(entity) = selection.primary else {
        gizmo.hovered = None;
        gizmo.drag = None;
        return;
    };
    if gizmo.drag.is_some_and(|d| d.entity != entity) {
        gizmo.drag = None;
    }
    let Ok(cam) = camera.single() else {
        return;
    };

    // ── Active drag ──────────────────────────────────────────────────────────
    if let Some(drag) = gizmo.drag {
        if mouse.pressed(MouseButton::Left) {
            // No ray (cursor off the viewport): hold the last transform and
            // keep the drag alive; the button is the authority.
            if let Some(ray) = ray.0
                && let Ok(mut transform) = transforms.get_mut(drag.entity)
            {
                let axis = drag.axis.dir();
                let changed = match drag.mode {
                    GizmoMode::Translate => {
                        axis_ray_param(drag.start.translation, axis, ray).map(|t| {
                            let new = drag.start.translation + axis * (t - drag.start_t);
                            let changed = new != transform.translation;
                            transform.translation = new;
                            changed
                        })
                    }
                    GizmoMode::Rotate => {
                        plane_hit_dir(drag.start.translation, axis, ray).map(|dir| {
                            let angle = signed_angle(drag.start_dir, dir, axis);
                            let new = Quat::from_axis_angle(axis, angle) * drag.start.rotation;
                            let changed = new != transform.rotation;
                            transform.rotation = new;
                            changed
                        })
                    }
                };
                if changed == Some(true) {
                    edits.write(PlacedEdited {
                        entity: drag.entity,
                        what: EditKind::Transform,
                    });
                }
            }
            return;
        }
        gizmo.drag = None; // released: the transform stays where it is
    }

    // ── Hover, and drag start on press ───────────────────────────────────────
    gizmo.hovered = None;
    let (Some(ray), Ok(transform)) = (ray.0, transforms.get(entity)) else {
        return;
    };
    let origin = transform.translation;
    let (arrow_len, arc_radius, pick) = handle_sizes(cam.translation(), origin);
    let mode = if ctrl_held(&keys) {
        GizmoMode::Rotate
    } else {
        GizmoMode::Translate
    };

    let mut best: Option<(f32, GizmoAxis)> = None;
    for axis in GizmoAxis::ALL {
        let dir = axis.dir();
        let proximity = match mode {
            GizmoMode::Translate => segment_ray_distance(
                origin + dir * arrow_len * ARROW_INSET,
                origin + dir * arrow_len,
                ray,
            ),
            // Radial distance from the plane crossing to the arc.
            GizmoMode::Rotate => plane_hit(origin, dir, ray)
                .map(|crossing| (crossing.distance(origin) - arc_radius).abs())
                .unwrap_or(f32::INFINITY),
        };
        if proximity < pick && best.is_none_or(|(d, _)| proximity < d) {
            best = Some((proximity, axis));
        }
    }
    let Some((_, axis)) = best else {
        return;
    };
    gizmo.hovered = Some((axis, mode));

    if viewport.hovered && mouse.just_pressed(MouseButton::Left) {
        let dir = axis.dir();
        let (start_t, start_dir) = match mode {
            GizmoMode::Translate => (axis_ray_param(origin, dir, ray).unwrap_or(0.0), Vec3::X),
            GizmoMode::Rotate => (0.0, plane_hit_dir(origin, dir, ray).unwrap_or(Vec3::X)),
        };
        gizmo.drag = Some(GizmoDrag {
            entity,
            axis,
            mode,
            start: *transform,
            start_t,
            start_dir,
        });
    }
}

/// Draw the handles: arrows for translate, full circles for rotate. The
/// hovered/dragged axis is highlighted.
pub fn draw_transform_gizmo(
    keys: Res<ButtonInput<KeyCode>>,
    selection: Res<Selection>,
    gizmo: Res<GizmoInteraction>,
    camera: Query<&GlobalTransform, With<PlayerCamera>>,
    transforms: Query<&Transform, With<PlacedObject>>,
    mut painter: Gizmos<EditorGizmoGroup>,
) {
    let Some(entity) = selection.primary else {
        return;
    };
    let (Ok(cam), Ok(transform)) = (camera.single(), transforms.get(entity)) else {
        return;
    };
    let origin = transform.translation;
    let (arrow_len, arc_radius, _) = handle_sizes(cam.translation(), origin);

    // A drag freezes its mode; otherwise Ctrl previews rotation live.
    let mode = gizmo.drag.map(|d| d.mode).unwrap_or(if ctrl_held(&keys) {
        GizmoMode::Rotate
    } else {
        GizmoMode::Translate
    });
    let active = gizmo
        .drag
        .map(|d| d.axis)
        .or(gizmo.hovered.and_then(|(a, m)| (m == mode).then_some(a)));

    for axis in GizmoAxis::ALL {
        let color = if active == Some(axis) {
            HIGHLIGHT
        } else {
            axis.color()
        };
        let dir = axis.dir();
        match mode {
            GizmoMode::Translate => {
                painter.arrow(
                    origin + dir * arrow_len * ARROW_INSET,
                    origin + dir * arrow_len,
                    color,
                );
            }
            GizmoMode::Rotate => {
                painter.circle(
                    Isometry3d::new(origin, axis.arc_rotation()),
                    arc_radius,
                    color,
                );
            }
        }
    }
}
