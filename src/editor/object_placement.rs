//! Place-tool placement of level entities.
//!
//! What gets placed is whatever `PlacementConfig` currently describes — a mesh,
//! a light, or a plugin preset — and the translucent preview follows the
//! cursor ray's hit on the terrain. Lights sit a little above the hit so they
//! light the ground they were dropped on rather than starting inside it.
//!
//! The preview exists only while [`EditorTool::Place`] is active; the tool's
//! `OnExit` tears it down (see `editor::mod`).

use bevy::picking::mesh_picking::ray_cast::{MeshRayCast, MeshRayCastSettings};
use bevy::{log, prelude::*};
use engine_api::{ScriptBinding, ScriptRegistry};

use crate::editor::input::{ScrollMod, scroll_modifier, scroll_notches};
use crate::editor::picking::{EditorRay, ViewportInput};
use crate::editor::selection::Selection;
use crate::game::GameEntity;
use crate::game::gltf_loading::{PendingMaterial, spawn_interactable_scene};
use crate::game::world::objects::{
    PlacedEntityRecord, PlacedKind, PlacedObjectRegistry, PlacementConfig, spawn_entity_record,
    world_pos_to_chunk,
};
use crate::game::world::terrain::TerrainChunk;

/// How far above the terrain hit a light preview (and the placed light) sits.
const LIGHT_PLACEMENT_HEIGHT: f32 = 2.5;
/// Yaw applied per notch of Alt+Scroll.
const ROTATION_STEP: f32 = std::f32::consts::FRAC_PI_2 / 6.0; // 15°

#[derive(Component)]
pub struct PlacementPreview;

#[derive(Resource, Default)]
pub struct PlacementPreviewEntity(pub Option<Entity>);

/// LMB on the viewport places the configured entity at the preview.
pub fn handle_place_click(
    mut commands: Commands,
    viewport: Res<ViewportInput>,
    preview_entity: Res<PlacementPreviewEntity>,
    preview_query: Query<(&Transform, &Visibility), With<PlacementPreview>>,
    asset_server: Res<AssetServer>,
    config: Res<PlacementConfig>,
    script_registry: Res<ScriptRegistry>,
    mut registry: ResMut<PlacedObjectRegistry>,
    mut selection: ResMut<Selection>,
) {
    if viewport.clicked_px.is_none() {
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

    // Until the world's saved id counter has been applied, alloc_id could
    // reuse an id from a not-yet-streamed chunk. Seeding happens the frame
    // the world's metadata lands, so hitting this is a same-frame race.
    if !registry.ids_seeded {
        warn!("Placement unavailable: world entity-id counter not seeded yet");
        return;
    }

    let pos = preview_transform.translation;

    let chunk = world_pos_to_chunk(pos.x, pos.z);
    let id = registry.alloc_id();
    let transform = Transform::from_translation(pos)
        .with_rotation(config.rotation)
        .with_scale(config.effective_scale());

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

    let entity = spawn_entity_record(&mut commands, &asset_server, &mut registry, chunk, &record);
    // Streaming must not dirty a chunk just by loading it, so marking dirty
    // is the placing caller's job.
    registry.dirty_chunks.insert(chunk);
    selection.primary = Some(entity);
}

pub fn setup_placement_preview(
    mut commands: Commands,
    mut preview_entity: ResMut<PlacementPreviewEntity>,
    asset_server: Res<AssetServer>,
    config: Res<PlacementConfig>,
) {
    if preview_entity.0.is_some() {
        return;
    }

    let transform = Transform::from_translation(Vec3::new(0.0, 0.05, 0.0))
        .with_rotation(config.rotation)
        .with_scale(config.effective_scale());

    let entity = match &config.kind {
        PlacedKind::Mesh { asset_path, .. } => {
            let scene: Handle<WorldAsset> = asset_server.load(asset_path);
            spawn_interactable_scene(
                &mut commands,
                (
                    GameEntity,
                    PlacementPreview,
                    WorldAssetRoot(scene),
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

/// Follow the cursor ray's terrain hit; hidden when the cursor is off the
/// viewport or over nothing.
pub fn update_placement_preview(
    mut preview_query: Query<(&mut Transform, &mut Visibility), With<PlacementPreview>>,
    terrain_filter: Query<(), With<TerrainChunk>>,
    mut raycast: MeshRayCast,
    preview_entity: Res<PlacementPreviewEntity>,
    ray: Res<EditorRay>,
    config: Res<PlacementConfig>,
) {
    let Some(entity) = preview_entity.0 else {
        return;
    };
    let Ok((mut preview_transform, mut visibility)) = preview_query.get_mut(entity) else {
        return;
    };
    let Some(ray) = ray.0 else {
        *visibility = Visibility::Hidden;
        return;
    };

    let terrain_only = |e: Entity| terrain_filter.contains(e);
    let settings = MeshRayCastSettings::default().with_filter(&terrain_only);

    // Scale/rotation live on the config (edited by scroll and the Place
    // panel, through `bypass_change_detection`); mirror them here so no
    // editor needs to reach into the preview.
    preview_transform.rotation = config.rotation;
    preview_transform.scale = config.effective_scale();

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
    // somewhere behind the cursor.
    gizmos.line(
        pos,
        pos - Vec3::Y * LIGHT_PLACEMENT_HEIGHT,
        color.with_alpha(0.35),
    );
}

/// Despawn the preview entity — `OnExit(EditorTool::Place)` and
/// `OnExit(EditorMode::Editor)`, so neither the next tool nor gameplay has a
/// leftover ghost mesh.
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
    input: Res<ButtonInput<KeyCode>>,
    viewport: Res<ViewportInput>,
    mut config: ResMut<PlacementConfig>,
) {
    if scroll_modifier(&input) != ScrollMod::Shift {
        return;
    }

    let delta = scroll_notches(&viewport);
    if delta == 0.0 || !config.kind.is_mesh() {
        return;
    }

    let factor = 1.15_f32.powf(delta);
    // Mutate through `bypass_change_detection` so a scale tweak doesn't
    // trigger `rebuild_preview_on_config_change` and reload the scene.
    let config = config.bypass_change_detection();
    config.scale = (config.scale * factor).clamp(Vec3::splat(0.001), Vec3::splat(100.0));
}

/// Alt/Option + scroll wheel yaws the placement in fixed steps.
///
/// Snapped rather than continuous so repeated placements line up with each
/// other — freehand rotation makes a row of fence posts impossible to match.
pub fn adjust_placement_rotation(
    input: Res<ButtonInput<KeyCode>>,
    viewport: Res<ViewportInput>,
    mut config: ResMut<PlacementConfig>,
    mut accumulated: Local<f32>,
) {
    if scroll_modifier(&input) != ScrollMod::Alt {
        *accumulated = 0.0;
        return;
    }

    // Trackpads deliver fractions of a notch; accumulate so a slow swipe
    // still rotates rather than being discarded below the threshold.
    *accumulated += scroll_notches(&viewport);
    if accumulated.abs() < 0.5 {
        return;
    }

    let steps = accumulated.round();
    *accumulated -= steps;
    let yaw = Quat::from_rotation_y(steps * ROTATION_STEP);
    // Same reasoning as `adjust_placement_scale`: bypass change detection so
    // a rotation tweak doesn't respawn the preview scene every notch.
    let config = config.bypass_change_detection();
    config.rotation = (yaw * config.rotation).normalize();
}
