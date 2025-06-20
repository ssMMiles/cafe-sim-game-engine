use super::GameEntity;
use super::interaction::{Interactable, InteractableType, spawn_interactable_scene};
use super::materials::{PendingMaterial, update_mesh_materials_color};
use super::simulation::{CookerMarker, CuttingBoardMarker, FridgeMarker, TableMarker};
use crate::settings::{PlacementObjectType, UserSettings};
use bevy::picking::mesh_picking::ray_cast::MeshRayCast;
use bevy::{log, prelude::*};

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
    camera_query: Query<&Transform, With<super::camera::PlayerCamera>>,
    mut raycast: MeshRayCast,
    asset_server: Res<AssetServer>,
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

    // Spawn object at determined location
    if let Some(pos) = position {
        let table_scene: Handle<Scene> = asset_server.load("meshes/table/result.gltf#Scene0");

        // Determine object properties based on current placement type
        let (name, interactable_type) = match settings.current_placement_object {
            PlacementObjectType::Table => ("Wooden Table", InteractableType::Table),
            PlacementObjectType::Cooker => ("Cooker", InteractableType::Cooker),
            PlacementObjectType::Fridge => ("Fridge", InteractableType::Fridge),
            PlacementObjectType::CuttingBoard => ("Cutting Board", InteractableType::CuttingBoard),
        };

        // Build common components
        let base_bundle = (
            GameEntity,
            Interactable {
                name: name.to_string(),
                distance: 10.0,
                interactable_type,
            },
            SceneRoot(table_scene),
            Transform::from_translation(Vec3::new(pos.x, 0.0, pos.z)).with_scale(Vec3::splat(0.02)),
            GlobalTransform::default(),
        );

        // Spawn with type-specific components
        match settings.current_placement_object {
            PlacementObjectType::Table => {
                spawn_interactable_scene(
                    &mut commands,
                    (
                        base_bundle,
                        TableMarker,
                        PendingMaterial {
                            base_color: Color::srgb(0.6, 0.4, 0.2),
                            metallic: 0.0,
                            perceptual_roughness: 0.9,
                            alpha_mode: AlphaMode::Opaque,
                        },
                    ),
                );
            }
            PlacementObjectType::Cooker => {
                spawn_interactable_scene(
                    &mut commands,
                    (
                        base_bundle,
                        CookerMarker,
                        PendingMaterial {
                            base_color: Color::srgb(0.9, 0.2, 0.2),
                            metallic: 0.3,
                            perceptual_roughness: 0.5,
                            alpha_mode: AlphaMode::Opaque,
                        },
                    ),
                );
            }
            PlacementObjectType::Fridge => {
                spawn_interactable_scene(
                    &mut commands,
                    (
                        base_bundle,
                        FridgeMarker,
                        PendingMaterial {
                            base_color: Color::srgb(0.2, 0.4, 0.8),
                            metallic: 0.5,
                            perceptual_roughness: 0.3,
                            alpha_mode: AlphaMode::Opaque,
                        },
                    ),
                );
            }
            PlacementObjectType::CuttingBoard => {
                spawn_interactable_scene(
                    &mut commands,
                    (
                        base_bundle,
                        CuttingBoardMarker,
                        PendingMaterial {
                            base_color: Color::srgb(0.2, 0.8, 0.3),
                            metallic: 0.0,
                            perceptual_roughness: 0.8,
                            alpha_mode: AlphaMode::Opaque,
                        },
                    ),
                );
            }
        };

        log::info!(
            "Spawned {} at position ({:.1}, 0.0, {:.1})",
            name,
            pos.x,
            pos.z
        );
    }
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
) {
    // Only setup preview if edit mode just turned on and we don't already have one
    if !settings.is_changed() || !settings.edit_mode || preview_entity.0.is_some() {
        return;
    }

    // Load the table mesh (same as actual placement)
    let table_scene: Handle<Scene> = asset_server.load("meshes/table/result.gltf#Scene0");

    // Get the color for the current placement type
    let base_color = match settings.current_placement_object {
        PlacementObjectType::Table => Color::srgba(0.6, 0.4, 0.2, 0.5), // Brown
        PlacementObjectType::Cooker => Color::srgba(0.9, 0.2, 0.2, 0.5), // Red
        PlacementObjectType::Fridge => Color::srgba(0.2, 0.4, 0.8, 0.5), // Blue
        PlacementObjectType::CuttingBoard => Color::srgba(0.2, 0.8, 0.3, 0.5), // Green
    };

    // Spawn preview entity
    let entity = spawn_interactable_scene(
        &mut commands,
        (
            GameEntity,
            PlacementPreview,
            SceneRoot(table_scene),
            Transform::from_translation(Vec3::new(0.0, 0.05, 0.0)) // Slightly elevated to prevent z-fighting
                .with_scale(Vec3::splat(0.02)),
            GlobalTransform::default(),
            Visibility::Hidden, // Start hidden until we have a valid position
            PendingMaterial {
                base_color,
                metallic: 0.0,
                perceptual_roughness: 0.9,
                alpha_mode: AlphaMode::Blend,
            },
        ),
    );

    // Store the entity for later reference
    preview_entity.0 = Some(entity);

    log::info!("Placement preview entity created");
}

pub fn update_placement_preview(
    mut preview_query: Query<(&mut Transform, &mut Visibility), With<PlacementPreview>>,
    camera_query: Query<&Transform, (With<super::camera::PlayerCamera>, Without<PlacementPreview>)>,
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

pub fn toggle_placement_object(
    mut settings: ResMut<UserSettings>,
    input: Res<ButtonInput<KeyCode>>,
    mut preview_query: Query<
        (Entity, Option<&mut PendingMaterial>, Option<&Children>),
        With<PlacementPreview>,
    >,
    children_query: Query<&Children>,
    mut mesh_material_query: Query<&mut MeshMaterial3d<StandardMaterial>, With<Mesh3d>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    if input.just_pressed(KeyCode::KeyR)
        && (input.pressed(KeyCode::ShiftLeft) || input.pressed(KeyCode::ShiftRight))
    {
        settings.cycle_placement_object();

        // Update preview material color
        let new_color = match settings.current_placement_object {
            PlacementObjectType::Table => Color::srgba(0.6, 0.4, 0.2, 0.5), // Brown
            PlacementObjectType::Cooker => Color::srgba(0.9, 0.2, 0.2, 0.5), // Red
            PlacementObjectType::Fridge => Color::srgba(0.2, 0.4, 0.8, 0.5), // Blue
            PlacementObjectType::CuttingBoard => Color::srgba(0.2, 0.8, 0.3, 0.5), // Green
        };

        // Update any preview entities
        for (_entity, pending_material, children) in preview_query.iter_mut() {
            if let Some(mut pending) = pending_material {
                // Update the pending material component
                pending.base_color = new_color;
            } else if let Some(children) = children {
                // Update existing materials on mesh children
                update_mesh_materials_color(
                    children,
                    new_color,
                    &children_query,
                    &mut mesh_material_query,
                    &mut materials,
                );
            }
        }
    }
}
