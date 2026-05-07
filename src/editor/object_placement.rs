use bevy::input::mouse::{MouseScrollUnit, MouseWheel};
use bevy::picking::mesh_picking::ray_cast::{MeshRayCast, MeshRayCastSettings};
use bevy::{log, prelude::*};

use crate::game::GameEntity;
use crate::game::gfx::camera::PlayerCamera;
use crate::game::gltf_loading::{InteractableParent, PendingMaterial, spawn_interactable_scene};
use crate::game::world::objects::{
    PlacedObject, PlacedObjectRegistry, PlacementConfig, spawn_placed_object, world_pos_to_chunk,
};
use crate::game::world::terrain::TerrainChunk;
use crate::settings::UserSettings;

#[derive(Component)]
pub struct PlacementPreview;

#[derive(Resource, Default)]
pub struct PlacementPreviewEntity(pub Option<Entity>);

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
    preview_query: Query<&Transform, With<PlacementPreview>>,
    camera_query: Query<&Transform, With<PlayerCamera>>,
    terrain_filter: Query<(), With<TerrainChunk>>,
    mut raycast: MeshRayCast,
    asset_server: Res<AssetServer>,
    config: Res<PlacementConfig>,
    mut registry: ResMut<PlacedObjectRegistry>,
) {
    if !settings.edit_mode || !mouse_input.just_pressed(MouseButton::Left) {
        return;
    }

    let mut position = None;
    if let Some(entity) = preview_entity.0 {
        if let Ok(preview_transform) = preview_query.get(entity) {
            position = Some(preview_transform.translation);
        }
    }

    if position.is_none() {
        if let Ok(camera_transform) = camera_query.single() {
            let camera_pos = camera_transform.translation;
            let camera_forward = camera_transform.forward().as_vec3();

            let ray = Ray3d::new(camera_pos, Dir3::new(camera_forward).unwrap());
            let terrain_only = |e: Entity| terrain_filter.contains(e);
            let settings = MeshRayCastSettings::default().with_filter(&terrain_only);
            let hits = raycast.cast_ray(ray, &settings);
            position = hits.first().map(|(_, hit)| hit.point);
        }
    }

    let Some(pos) = position else {
        log::warn!("Could not determine placement position for new object");
        return;
    };

    let chunk = world_pos_to_chunk(pos.x, pos.z);
    let id = registry.alloc_id();
    let transform = Transform::from_translation(pos).with_scale(config.scale);
    spawn_placed_object(
        &mut commands,
        &asset_server,
        &mut registry,
        config.asset_path.clone(),
        id,
        chunk,
        transform,
    );
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

    let scene: Handle<Scene> = asset_server.load(&config.asset_path);
    let base_color = Color::srgba(0.8, 0.8, 0.8, 0.4);

    let entity = spawn_interactable_scene(
        &mut commands,
        (
            GameEntity,
            PlacementPreview,
            SceneRoot(scene),
            Transform::from_translation(Vec3::new(0.0, 0.05, 0.0)).with_scale(config.scale),
            GlobalTransform::default(),
            Visibility::Hidden,
            PendingMaterial {
                base_color,
                metallic: 0.0,
                perceptual_roughness: 0.9,
                alpha_mode: AlphaMode::Blend,
            },
        ),
    );

    preview_entity.0 = Some(entity);
    log::info!("Placement preview entity created");
}

pub fn update_placement_preview(
    mut preview_query: Query<(&mut Transform, &mut Visibility), With<PlacementPreview>>,
    camera_query: Query<&Transform, (With<PlayerCamera>, Without<PlacementPreview>)>,
    terrain_filter: Query<(), With<TerrainChunk>>,
    mut raycast: MeshRayCast,
    preview_entity: Res<PlacementPreviewEntity>,
    settings: Res<UserSettings>,
) {
    if !settings.edit_mode || preview_entity.0.is_none() {
        return;
    }

    let entity = preview_entity.0.unwrap();

    if let Ok((mut preview_transform, mut visibility)) = preview_query.get_mut(entity) {
        if let Ok(camera_transform) = camera_query.single() {
            let camera_pos = camera_transform.translation;
            let camera_forward = camera_transform.forward().as_vec3();

            let ray = Ray3d::new(camera_pos, Dir3::new(camera_forward).unwrap());
            let terrain_only = |e: Entity| terrain_filter.contains(e);
            let settings = MeshRayCastSettings::default().with_filter(&terrain_only);
            let hits = raycast.cast_ray(ray, &settings);

            if let Some((_, hit)) = hits.first() {
                let position = hit.point;
                preview_transform.translation =
                    Vec3::new(position.x, position.y + 0.05, position.z);
                *visibility = Visibility::Visible;
            } else {
                *visibility = Visibility::Hidden;
            }
        }
    }
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

/// When `PlacementConfig` changes (e.g. a new mesh is selected), despawn the
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

    if !shift || delta == 0.0 {
        return;
    }

    let factor = 1.15_f32.powf(delta);
    config.scale = (config.scale * factor).clamp(Vec3::splat(0.001), Vec3::splat(100.0));

    if let Some(entity) = preview_entity.0 {
        if let Ok(mut transform) = preview_query.get_mut(entity) {
            transform.scale = config.scale;
        }
    }
}

/// Delete key while looking at a `PlacedObject` in edit mode.
pub fn delete_hovered_object(
    mut commands: Commands,
    settings: Res<UserSettings>,
    input: Res<ButtonInput<KeyCode>>,
    mut registry: ResMut<PlacedObjectRegistry>,
    mut raycast: MeshRayCast,
    camera_query: Query<&Transform, With<PlayerCamera>>,
    interactable_parent_query: Query<&InteractableParent>,
    placed_query: Query<&PlacedObject>,
) {
    if !settings.edit_mode || !input.just_pressed(KeyCode::Delete) {
        return;
    }

    let Ok(cam) = camera_query.single() else {
        return;
    };
    let Ok(forward) = Dir3::new(cam.forward().as_vec3()) else {
        return;
    };
    let ray = Ray3d::new(cam.translation, forward);
    let hits = raycast.cast_ray(ray, &Default::default());

    for (entity, _) in hits.iter() {
        let root = interactable_parent_query
            .get(*entity)
            .map(|p| p.0)
            .unwrap_or(*entity);

        if let Ok(obj) = placed_query.get(root) {
            registry.by_id.remove(&obj.id);
            registry.dirty_chunks.insert(obj.chunk);
            commands.entity(root).despawn();
            info!(
                "Deleted placed object id={} from chunk {:?}",
                obj.id, obj.chunk
            );
            return;
        }
    }
}
