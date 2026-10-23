//! Edit-mode placement of level entities (Shift+E to toggle).
//!
//! What gets placed is whatever `PlacementConfig` currently describes — a mesh,
//! a light, or a plugin preset — and the translucent preview follows the
//! camera ray's hit on the terrain. Lights sit a little above the hit so they
//! light the ground they were dropped on rather than starting inside it.
//!
//! [`PlacedPicker`] is the one way the editor answers "which placed entity is
//! the camera pointing at". It raycasts meshes first and falls back to a
//! ray/sphere test for mesh-less kinds (lights, anchors), so Delete and the
//! scripts panel work on lights even though nothing can be raycast there.

use bevy::ecs::system::SystemParam;
use bevy::input::mouse::{MouseScrollUnit, MouseWheel};
use bevy::picking::mesh_picking::ray_cast::{MeshRayCast, MeshRayCastSettings};
use bevy::{log, prelude::*};
use engine_api::{ScriptBinding, ScriptRegistry};

use crate::game::GameEntity;
use crate::game::gfx::camera::PlayerCamera;
use crate::game::gltf_loading::{InteractableParent, PendingMaterial, spawn_interactable_scene};
use crate::game::world::objects::{
    PlacedEntityRecord, PlacedKind, PlacedObject, PlacedObjectRegistry, PlacementConfig,
    spawn_entity_record, world_pos_to_chunk,
};
use crate::game::world::terrain::TerrainChunk;
use crate::settings::UserSettings;

/// How far above the terrain hit a light preview (and the placed light) sits.
const LIGHT_PLACEMENT_HEIGHT: f32 = 2.5;
/// Pick radius for mesh-less placed entities.
const PICK_SPHERE_RADIUS: f32 = 0.5;

#[derive(Component)]
pub struct PlacementPreview;

#[derive(Resource, Default)]
pub struct PlacementPreviewEntity(pub Option<Entity>);

// ── Picking ───────────────────────────────────────────────────────────────────

/// Everything needed to find the placed entity under the crosshair.
#[derive(SystemParam)]
pub struct PlacedPicker<'w, 's> {
    raycast: MeshRayCast<'w, 's>,
    camera: Query<'w, 's, &'static Transform, With<PlayerCamera>>,
    parents: Query<'w, 's, &'static InteractableParent>,
    placed: Query<'w, 's, (Entity, &'static PlacedObject, &'static GlobalTransform)>,
}

impl PlacedPicker<'_, '_> {
    /// The nearest placed entity along the camera's forward ray, if any.
    pub fn pick(&mut self) -> Option<Entity> {
        let cam = self.camera.single().ok()?;
        let dir = Dir3::new(cam.forward().as_vec3()).ok()?;
        let ray = Ray3d::new(cam.translation, dir);

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
            if obj.kind.is_mesh() {
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

// ── Systems ───────────────────────────────────────────────────────────────────

pub fn toggle_edit_mode(mut settings: ResMut<UserSettings>, input: Res<ButtonInput<KeyCode>>) {
    if input.just_pressed(KeyCode::KeyE)
        && (input.pressed(KeyCode::ShiftLeft) || input.pressed(KeyCode::ShiftRight))
    {
        settings.toggle_edit_mode();
    }
}

pub fn handle_edit_mode_click(
    mut commands: Commands,
    settings: Res<UserSettings>,
    mouse_input: Res<ButtonInput<MouseButton>>,
    preview_entity: Res<PlacementPreviewEntity>,
    preview_query: Query<(&Transform, &Visibility), With<PlacementPreview>>,
    asset_server: Res<AssetServer>,
    config: Res<PlacementConfig>,
    script_registry: Res<ScriptRegistry>,
    mut registry: ResMut<PlacedObjectRegistry>,
) {
    if !settings.edit_mode || !mouse_input.just_pressed(MouseButton::Left) {
        return;
    }

    // The preview is the single source of truth for where a click lands; if
    // it isn't showing there is nowhere valid to place.
    let Some((preview_transform, vis)) = preview_entity.0.and_then(|e| preview_query.get(e).ok())
    else {
        return;
    };
    if *vis == Visibility::Hidden {
        return;
    }
    let pos = preview_transform.translation;

    let chunk = world_pos_to_chunk(pos.x, pos.z);
    let id = registry.alloc_id();
    let transform = Transform::from_translation(pos).with_scale(config.effective_scale());

    let mut record = PlacedEntityRecord::new(id, config.kind.clone(), &transform);

    // Stamp mode: seed the binding with the script's schema defaults. If the
    // plugin isn't loaded right now the binding still gets attached with no
    // props — it hydrates when the plugin shows up.
    if let Some((plugin, script)) = &config.stamp_script {
        let props = script_registry
            .get(plugin, script)
            .map(|r| r.schema.default_props())
            .unwrap_or_default();
        record = record.with_scripts(vec![
            ScriptBinding::new(plugin.clone(), script.clone()).with_props(props),
        ]);
    }

    spawn_entity_record(&mut commands, &asset_server, &mut registry, chunk, &record);
    // Streaming must not dirty a chunk just by loading it, so marking dirty
    // is the placing caller's job.
    registry.dirty_chunks.insert(chunk);
}

pub fn setup_placement_preview(
    mut commands: Commands,
    mut preview_entity: ResMut<PlacementPreviewEntity>,
    asset_server: Res<AssetServer>,
    settings: Res<UserSettings>,
    config: Res<PlacementConfig>,
) {
    if !settings.edit_mode || preview_entity.0.is_some() {
        return;
    }

    let transform =
        Transform::from_translation(Vec3::new(0.0, 0.05, 0.0)).with_scale(config.effective_scale());

    let entity = match &config.kind {
        PlacedKind::Mesh { asset_path, .. } => {
            let scene: Handle<Scene> = asset_server.load(asset_path);
            spawn_interactable_scene(
                &mut commands,
                (
                    GameEntity,
                    PlacementPreview,
                    SceneRoot(scene),
                    transform,
                    GlobalTransform::default(),
                    Visibility::Hidden,
                    PendingMaterial {
                        base_color: Color::srgba(0.8, 0.8, 0.8, 0.4),
                        metallic: 0.0,
                        perceptual_roughness: 0.9,
                        alpha_mode: AlphaMode::Blend,
                    },
                ),
            )
        }
        // No mesh to show; `draw_preview_gizmo` renders these.
        _ => commands
            .spawn((
                GameEntity,
                PlacementPreview,
                transform,
                GlobalTransform::default(),
                Visibility::Hidden,
            ))
            .id(),
    };

    preview_entity.0 = Some(entity);
    log::info!("Placement preview entity created ({})", config.kind.label());
}

pub fn update_placement_preview(
    mut preview_query: Query<(&mut Transform, &mut Visibility), With<PlacementPreview>>,
    camera_query: Query<&Transform, (With<PlayerCamera>, Without<PlacementPreview>)>,
    terrain_filter: Query<(), With<TerrainChunk>>,
    mut raycast: MeshRayCast,
    preview_entity: Res<PlacementPreviewEntity>,
    settings: Res<UserSettings>,
    config: Res<PlacementConfig>,
) {
    if !settings.edit_mode {
        return;
    }
    let Some(entity) = preview_entity.0 else {
        return;
    };
    let Ok((mut preview_transform, mut visibility)) = preview_query.get_mut(entity) else {
        return;
    };
    let Ok(camera_transform) = camera_query.single() else {
        return;
    };
    let Ok(dir) = Dir3::new(camera_transform.forward().as_vec3()) else {
        return;
    };

    let ray = Ray3d::new(camera_transform.translation, dir);
    let terrain_only = |e: Entity| terrain_filter.contains(e);
    let settings = MeshRayCastSettings::default().with_filter(&terrain_only);

    match raycast.cast_ray(ray, &settings).first() {
        Some((_, hit)) => {
            let lift = if config.kind.is_light() {
                LIGHT_PLACEMENT_HEIGHT
            } else {
                0.05
            };
            preview_transform.translation = hit.point + Vec3::Y * lift;
            *visibility = Visibility::Visible;
        }
        None => *visibility = Visibility::Hidden,
    }
}

/// Immediate-mode marker for mesh-less previews, matching the look of the
/// world's placed-light gizmos so what you see is what you get.
pub fn draw_preview_gizmo(
    mut gizmos: Gizmos,
    preview_entity: Res<PlacementPreviewEntity>,
    preview_query: Query<(&Transform, &Visibility), With<PlacementPreview>>,
    config: Res<PlacementConfig>,
) {
    if config.kind.is_mesh() {
        return;
    }
    let Some((transform, vis)) = preview_entity.0.and_then(|e| preview_query.get(e).ok()) else {
        return;
    };
    if *vis == Visibility::Hidden {
        return;
    }
    let pos = transform.translation;
    let color = match &config.kind {
        PlacedKind::PointLight { color, .. } | PlacedKind::SpotLight { color, .. } => {
            Color::linear_rgba(color[0], color[1], color[2], 0.7)
        }
        _ => Color::srgba(0.8, 0.8, 0.8, 0.7),
    };
    gizmos.sphere(pos, 0.2, color);
    // Drop line to the ground so the lift reads as height, not as floating
    // somewhere behind the crosshair.
    gizmos.line(
        pos,
        pos - Vec3::Y * LIGHT_PLACEMENT_HEIGHT,
        color.with_alpha(0.35),
    );
}

pub fn cleanup_placement_preview(
    mut commands: Commands,
    mut preview_entity: ResMut<PlacementPreviewEntity>,
    settings: Res<UserSettings>,
) {
    if !settings.is_changed() || settings.edit_mode || preview_entity.0.is_none() {
        return;
    }

    if let Some(entity) = preview_entity.0.take() {
        commands.entity(entity).despawn();
        log::info!("Placement preview entity removed");
    }
}

/// Despawn the preview entity unconditionally — used by `OnExit(EditorMode::Editor)`
/// so the gameplay screen has no leftover ghost mesh.
pub fn teardown_placement_preview(
    mut commands: Commands,
    mut preview_entity: ResMut<PlacementPreviewEntity>,
) {
    if let Some(entity) = preview_entity.0.take() {
        commands.entity(entity).despawn();
    }
}

/// When `PlacementConfig` changes (e.g. a new entry is selected), despawn the
/// existing preview so `setup_placement_preview` re-creates it next frame.
pub fn rebuild_preview_on_config_change(
    mut commands: Commands,
    mut preview_entity: ResMut<PlacementPreviewEntity>,
) {
    if let Some(entity) = preview_entity.0.take() {
        commands.entity(entity).despawn();
    }
}

/// Shift + scroll wheel adjusts PlacementConfig.scale uniformly.
pub fn adjust_placement_scale(
    settings: Res<UserSettings>,
    input: Res<ButtonInput<KeyCode>>,
    mut scroll_events: MessageReader<MouseWheel>,
    mut config: ResMut<PlacementConfig>,
    preview_entity: Res<PlacementPreviewEntity>,
    mut preview_query: Query<&mut Transform, With<PlacementPreview>>,
) {
    if !settings.edit_mode {
        scroll_events.clear();
        return;
    }

    let shift = input.pressed(KeyCode::ShiftLeft) || input.pressed(KeyCode::ShiftRight);

    let mut delta = 0.0_f32;
    for event in scroll_events.read() {
        let y = match event.unit {
            MouseScrollUnit::Line => event.y,
            MouseScrollUnit::Pixel => event.y / 53.0,
        };
        delta += y;
    }

    if !shift || delta == 0.0 || !config.kind.is_mesh() {
        return;
    }

    let factor = 1.15_f32.powf(delta);
    // Mutate through `bypass_change_detection` so a scale tweak doesn't
    // trigger `rebuild_preview_on_config_change` and reload the scene.
    let config = config.bypass_change_detection();
    config.scale = (config.scale * factor).clamp(Vec3::splat(0.001), Vec3::splat(100.0));

    if let Some(entity) = preview_entity.0
        && let Ok(mut transform) = preview_query.get_mut(entity)
    {
        transform.scale = config.scale;
    }
}

/// Delete key while looking at a placed entity in edit mode. Works on lights
/// too, via the picker's sphere fallback.
pub fn delete_hovered_object(
    mut commands: Commands,
    settings: Res<UserSettings>,
    input: Res<ButtonInput<KeyCode>>,
    mut registry: ResMut<PlacedObjectRegistry>,
    mut picker: PlacedPicker,
    placed_query: Query<&PlacedObject>,
) {
    if !settings.edit_mode || !input.just_pressed(KeyCode::Delete) {
        return;
    }

    let Some(root) = picker.pick() else {
        return;
    };
    let Ok(obj) = placed_query.get(root) else {
        return;
    };

    registry.by_id.remove(&obj.id);
    registry.dirty_chunks.insert(obj.chunk);
    commands.entity(root).despawn();
    info!(
        "Deleted placed {} id={} from chunk {:?}",
        obj.kind.label(),
        obj.id,
        obj.chunk
    );
}
