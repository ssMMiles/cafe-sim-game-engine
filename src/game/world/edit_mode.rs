use super::objects::{
    PlacedObject, PlacedObjectRegistry, PlacementConfig, spawn_placed_object, world_pos_to_chunk,
};
use crate::game::GameEntity;
use crate::game::camera::PlayerCamera;
use crate::game::interaction::spawn_interactable_scene;
use crate::game::materials::PendingMaterial;
use crate::settings::UserSettings;
use bevy::input::mouse::MouseScrollUnit;
use bevy::picking::mesh_picking::ray_cast::MeshRayCast;
use bevy::{input::mouse::MouseWheel, log, prelude::*};

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
    mut raycast: MeshRayCast,
    asset_server: Res<AssetServer>,
    config: Res<PlacementConfig>,
    mut registry: ResMut<PlacedObjectRegistry>,
) {
    // Only proceed if edit mode is active and left mouse button was just clicked
    if !settings.edit_mode || !mouse_input.just_pressed(MouseButton::Left) {
        return;
    }

    // Try to get position from preview entity first
    let mut position = None;
    if let Some(entity) = preview_entity.0 {
        if let Ok(preview_transform) = preview_query.get(entity) {
            position = Some(preview_transform.translation);
        }
    }

    // Fall back to raycast if no preview position
    if position.is_none() {
        if let Ok(camera_transform) = camera_query.single() {
            let camera_pos = camera_transform.translation;
            let camera_forward = camera_transform.forward().as_vec3();

            // Create ray from camera
            let ray = Ray3d::new(camera_pos, Dir3::new(camera_forward).unwrap());

            // Cast ray to find ground intersection
            let hits = raycast.cast_ray(ray, &Default::default());

            // Find the ground plane hit (y = 0)
            for (_, hit_data) in hits.iter() {
                if hit_data.point.y < 1.0 {
                    position = Some(hit_data.point);
                    break;
                }
            }

            // If no mesh hit, calculate ground intersection manually
            if position.is_none() && camera_forward.y < 0.0 {
                let t = -camera_pos.y / camera_forward.y;
                if t > 0.0 {
                    let intersection = camera_pos + camera_forward * t;
                    position = Some(intersection);
                }
            }
        }
    }

    let Some(pos) = position else {
        log::warn!("Could not determine placement position for new object");
        return;
    };

    let chunk = world_pos_to_chunk(pos.x, pos.z);
    let id = registry.alloc_id();
    let transform =
        Transform::from_translation(Vec3::new(pos.x, 0.0, pos.z)).with_scale(config.scale);
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

#[derive(Component)]
pub struct PlacementPreview;

#[derive(Resource, Default)]
pub struct PlacementPreviewEntity(pub Option<Entity>);

pub fn setup_placement_preview(
    mut commands: Commands,
    mut preview_entity: ResMut<PlacementPreviewEntity>,
    asset_server: Res<AssetServer>,
    settings: Res<UserSettings>,
    config: Res<PlacementConfig>,
) {
    // Only setup preview if edit mode just turned on and we don't already have one
    if !settings.is_changed() || !settings.edit_mode || preview_entity.0.is_some() {
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
    mut raycast: MeshRayCast,
    preview_entity: Res<PlacementPreviewEntity>,
    settings: Res<UserSettings>,
) {
    // Only update if we're in edit mode and have a preview entity
    if !settings.edit_mode || preview_entity.0.is_none() {
        return;
    }

    let entity = preview_entity.0.unwrap();

    if let Ok((mut preview_transform, mut visibility)) = preview_query.get_mut(entity) {
        if let Ok(camera_transform) = camera_query.single() {
            let camera_pos = camera_transform.translation;
            let camera_forward = camera_transform.forward().as_vec3();

            // Create ray from camera
            let ray = Ray3d::new(camera_pos, Dir3::new(camera_forward).unwrap());

            // Cast ray to find ground intersection
            let hits = raycast.cast_ray(ray, &Default::default());

            // Find the ground plane hit (y = 0)
            let mut ground_hit: Option<Vec3> = None;

            for (_, hit_data) in hits.iter() {
                // Check if this is roughly at ground level
                if hit_data.point.y < 1.0 {
                    ground_hit = Some(hit_data.point);
                    break;
                }
            }

            // If no mesh hit, calculate ground intersection manually
            if ground_hit.is_none() && camera_forward.y < 0.0 {
                // Calculate where ray intersects y=0 plane
                let t = -camera_pos.y / camera_forward.y;
                if t > 0.0 {
                    let intersection = camera_pos + camera_forward * t;
                    ground_hit = Some(intersection);
                }
            }

            // Update preview position
            if let Some(position) = ground_hit {
                preview_transform.translation = Vec3::new(position.x, 0.05, position.z); // Slightly elevated
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
    // Only cleanup if edit mode just turned off and we have a preview
    if !settings.is_changed() || settings.edit_mode || preview_entity.0.is_none() {
        return;
    }

    if let Some(entity) = preview_entity.0.take() {
        commands.entity(entity).despawn();
        log::info!("Placement preview entity removed");
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
            MouseScrollUnit::Pixel => event.y / 53.0, // normalise pixels to ~lines
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
