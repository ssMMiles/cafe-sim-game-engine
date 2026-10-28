//! Cursor → world ray, and "which placed entity is under the cursor".
//!
//! The editor viewport is an egui tab showing the camera's render texture, so
//! the pointer is not simply the window cursor: the egui pass publishes where
//! the pointer is *inside the texture* ([`ViewportInput`]), and
//! [`compute_editor_ray`] turns that into one [`EditorRay`] per frame. Every
//! tool (placement preview, terrain brush, picker) reads that ray instead of
//! building its own, so they can never disagree about what is being pointed at.
//!
//! Timing: the egui pass runs in `PostUpdate`, so `ViewportInput` describes
//! the previous frame's pointer. One frame of lag on a continuously moving
//! cursor is invisible; clicks are latched in `clicked_px` so none are lost.

use bevy::ecs::system::SystemParam;
use bevy::picking::mesh_picking::ray_cast::{MeshRayCast, MeshRayCastSettings};
use bevy::prelude::*;

use crate::game::gfx::camera::PlayerCamera;
use crate::game::gltf_loading::InteractableParent;
use crate::game::world::objects::PlacedObject;

/// Pick radius for mesh-less placed entities (lights, anchors).
const PICK_SPHERE_RADIUS: f32 = 0.5;

/// What the egui viewport tab saw this pass. Written in
/// `EguiPrimaryContextPass`, read by `Update` systems next frame.
#[derive(Resource, Default, Debug, Clone)]
pub struct ViewportInput {
    /// Pointer is over the viewport image and no other egui widget is being
    /// dragged. Gate for every mouse-button and wheel consumer in the editor.
    pub hovered: bool,
    /// Pointer position in render-texture pixels (origin top-left), while
    /// hovered or looking.
    pub cursor_px: Option<Vec2>,
    /// Right-button fly-look in progress. The OS cursor is locked meanwhile.
    pub look_active: bool,
    /// A primary click landed on the viewport this pass, at this texture
    /// position.
    pub clicked_px: Option<Vec2>,
    /// Wheel delta over the viewport this pass, in egui points.
    pub scroll: Vec2,
    /// Physical size of the tab body. The render target is resized to match.
    pub size_px: UVec2,
    /// egui still has an interaction in flight (widget drag, open popup,
    /// focused text field). Continuous edits stay coalesced while it holds.
    pub egui_busy: bool,
}

impl ViewportInput {
    /// Mouse buttons and the wheel belong to the editor only while the pointer
    /// is on the viewport (or a look-drag that started there is in progress).
    pub fn pointer_free(&self) -> bool {
        self.hovered || self.look_active
    }
}

/// Run condition: the viewport owns the pointer this frame.
pub fn viewport_pointer_free(viewport: Res<ViewportInput>) -> bool {
    viewport.pointer_free()
}

/// The ray through the cursor, in world space, if the cursor is on the
/// viewport. Computed once per frame by [`compute_editor_ray`].
#[derive(Resource, Default, Debug, Clone, Copy)]
pub struct EditorRay(pub Option<Ray3d>);

pub fn compute_editor_ray(
    viewport: Res<ViewportInput>,
    camera: Query<(&Camera, &GlobalTransform), With<PlayerCamera>>,
    mut ray: ResMut<EditorRay>,
) {
    let Ok((camera, transform)) = camera.single() else {
        ray.0 = None;
        return;
    };
    // The camera renders to an image, so its logical viewport is the image
    // size (scale factor 1) and texture pixels are already the right space.
    ray.0 = viewport
        .cursor_px
        .and_then(|px| camera.viewport_to_world(transform, px).ok());
}

/// Convert an egui hover position into render-texture pixels.
///
/// `rect_min` is the image's top-left and `ppp` the context's
/// pixels-per-point; both are in egui's logical points. Returns `None` when
/// the position is outside the image, which happens for one pass when a drag
/// leaves it.
pub fn cursor_to_texture_px(pos: Vec2, rect_min: Vec2, rect_size: Vec2, ppp: f32) -> Option<Vec2> {
    let local = pos - rect_min;
    if local.x < 0.0 || local.y < 0.0 || local.x > rect_size.x || local.y > rect_size.y {
        return None;
    }
    Some(local * ppp)
}

/// Everything needed to find the placed entity under the cursor.
#[derive(SystemParam)]
pub struct PlacedPicker<'w, 's> {
    raycast: MeshRayCast<'w, 's>,
    ray: Res<'w, EditorRay>,
    parents: Query<'w, 's, &'static InteractableParent>,
    placed: Query<'w, 's, (Entity, &'static PlacedObject, &'static GlobalTransform)>,
}

impl PlacedPicker<'_, '_> {
    /// The nearest placed entity along the cursor ray, if any.
    pub fn pick(&mut self) -> Option<Entity> {
        let ray = self.ray.0?;

        // Meshes: first hit that resolves (via the scene's mesh children) to a
        // PlacedObject root.
        let mut best: Option<(f32, Entity)> = None;
        for (entity, hit) in self.raycast.cast_ray(ray, &MeshRayCastSettings::default()) {
            let root = self.parents.get(*entity).map(|p| p.0).unwrap_or(*entity);
            if self.placed.get(root).is_ok() {
                best = Some((hit.distance, root));
                break;
            }
        }

        // Mesh-less kinds: ray/sphere against their origin. Keep whichever is
        // nearer so a light in front of a rock wins over the rock.
        for (entity, obj, gt) in self.placed.iter() {
            // Groups are selected from the outliner (or Alt+click on a
            // member); an invisible pick sphere would only steal clicks.
            if obj.kind.is_mesh() || obj.kind.is_group() {
                continue;
            }
            let Some(t) = ray_sphere(ray, gt.translation(), PICK_SPHERE_RADIUS) else {
                continue;
            };
            if best.is_none_or(|(d, _)| t < d) {
                best = Some((t, entity));
            }
        }

        best.map(|(_, e)| e)
    }
}

/// Distance along `ray` to the first intersection with a sphere, or `None`.
fn ray_sphere(ray: Ray3d, center: Vec3, radius: f32) -> Option<f32> {
    let oc = ray.origin - center;
    let b = oc.dot(ray.direction.as_vec3());
    let c = oc.length_squared() - radius * radius;
    let disc = b * b - c;
    if disc < 0.0 {
        return None;
    }
    let t = -b - disc.sqrt();
    (t > 0.0).then_some(t)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_maps_into_texture_pixels_with_scale_factor() {
        // A 400×300 pt image at (100, 50) on a 2× display.
        let px = cursor_to_texture_px(
            Vec2::new(300.0, 200.0),
            Vec2::new(100.0, 50.0),
            Vec2::new(400.0, 300.0),
            2.0,
        );
        assert_eq!(px, Some(Vec2::new(400.0, 300.0)));
    }

    #[test]
    fn cursor_outside_the_image_is_none() {
        let rect_min = Vec2::new(100.0, 50.0);
        let size = Vec2::new(400.0, 300.0);
        assert_eq!(
            cursor_to_texture_px(Vec2::new(99.0, 60.0), rect_min, size, 1.0),
            None
        );
        assert_eq!(
            cursor_to_texture_px(Vec2::new(501.0, 60.0), rect_min, size, 1.0),
            None
        );
    }

    #[test]
    fn ray_sphere_hits_in_front_only() {
        let ray = Ray3d::new(Vec3::ZERO, Dir3::Z);
        assert!(ray_sphere(ray, Vec3::new(0.0, 0.0, 5.0), 0.5).is_some());
        assert!(ray_sphere(ray, Vec3::new(0.0, 0.0, -5.0), 0.5).is_none());
        assert!(ray_sphere(ray, Vec3::new(2.0, 0.0, 5.0), 0.5).is_none());
    }
}
