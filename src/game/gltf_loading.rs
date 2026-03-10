use crate::game::GameEntity;

use super::gfx::camera::PlayerCamera;
use bevy::{
    log,
    picking::mesh_picking::ray_cast::{MeshRayCast, MeshRayCastSettings},
    prelude::*,
    scene::SceneInstanceReady,
};

#[derive(Component, Clone)]
pub struct PendingMaterial {
    pub base_color: Color,
    pub metallic: f32,
    pub perceptual_roughness: f32,
    pub alpha_mode: bevy::render::alpha::AlphaMode,
}

pub fn apply_material_to_meshes(
    commands: &mut Commands,
    children: &Children,
    material: &Handle<StandardMaterial>,
    mesh_query: &Query<Entity, With<Mesh3d>>,
    children_query: &Query<&Children>,
) {
    for child in children.iter() {
        if mesh_query.contains(child) {
            commands
                .entity(child)
                .insert(MeshMaterial3d(material.clone()));
        }

        // Recursively apply to grandchildren
        if let Ok(grandchildren) = children_query.get(child) {
            apply_material_to_meshes(
                commands,
                grandchildren,
                material,
                mesh_query,
                children_query,
            );
        }
    }
}

#[derive(Component, Clone)]
pub struct Interactable {
    pub name: String,
    pub distance: f32,
}

#[derive(Component)]
pub struct InteractableParent(pub Entity);

#[derive(Component)]
pub struct HoveredOver;

// Single observer function that handles both Interactable and PendingMaterial when scene is ready
fn on_scene_ready(
    trigger: On<SceneInstanceReady>,
    mut commands: Commands,
    entity_query: Query<(
        Option<&Interactable>,
        Option<&PendingMaterial>,
        Option<&Children>,
    )>,
    mesh_query: Query<Entity, With<Mesh3d>>,
    children_query: Query<&Children>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    let entity = trigger.entity;

    if let Ok((maybe_interactable, maybe_pending_material, maybe_children)) =
        entity_query.get(entity)
    {
        // Only process if we have children to work with
        if let Some(children) = maybe_children {
            // Handle interactable propagation if present
            if let Some(interactable) = maybe_interactable {
                propagate_to_mesh_children(
                    &mut commands,
                    children,
                    interactable,
                    entity,
                    &mesh_query,
                    &children_query,
                );
                log::info!(
                    "Propagated Interactable to mesh children of entity {:?}",
                    entity
                );
            }

            // Handle material application if present
            if let Some(pending_material) = maybe_pending_material {
                let material = materials.add(StandardMaterial {
                    base_color: pending_material.base_color,
                    metallic: pending_material.metallic,
                    perceptual_roughness: pending_material.perceptual_roughness,
                    alpha_mode: pending_material.alpha_mode,
                    ..default()
                });

                apply_material_to_meshes(
                    &mut commands,
                    children,
                    &material,
                    &mesh_query,
                    &children_query,
                );
                commands.entity(entity).remove::<PendingMaterial>();

                log::info!(
                    "Applied material to entity {:?} via SceneInstanceReady",
                    entity
                );
            }
        }
    }
}

// Generic spawn function that adds observer for scene entities
pub fn spawn_interactable_scene<B: Bundle>(commands: &mut Commands, bundle: B) -> Entity {
    commands.spawn(bundle).observe(on_scene_ready).id()
}

fn propagate_to_mesh_children(
    commands: &mut Commands,
    children: &Children,
    interactable: &Interactable,
    parent_entity: Entity,
    mesh_query: &Query<Entity, With<Mesh3d>>,
    children_query: &Query<&Children>,
) {
    for child in children.iter() {
        // If this child is a mesh, add Interactable and mark parent
        if mesh_query.contains(child) {
            commands
                .entity(child)
                .insert((interactable.clone(), InteractableParent(parent_entity)));
        }

        // Recursively check grandchildren
        if let Ok(grandchildren) = children_query.get(child) {
            propagate_to_mesh_children(
                commands,
                grandchildren,
                interactable,
                parent_entity,
                mesh_query,
                children_query,
            );
        }
    }
}

pub fn check_interactions(
    mut commands: Commands,
    camera_query: Query<&Transform, With<PlayerCamera>>,
    interactable_query: Query<
        (
            Entity,
            &GlobalTransform,
            &Interactable,
            Option<&InteractableParent>,
        ),
        Without<PlayerCamera>,
    >,
    marked_query: Query<Entity, With<HoveredOver>>,
    mut raycast: MeshRayCast,
    interactable_filter: Query<(), With<Interactable>>,
) {
    let mut current_interaction: Option<Entity> = None;

    if let Ok(camera_transform) = camera_query.single() {
        let camera_pos = camera_transform.translation;
        let camera_forward = camera_transform.forward().as_vec3();

        // Create ray from camera
        let ray = Ray3d::new(camera_pos, Dir3::new(camera_forward).unwrap());

        // Cast ray using Bevy's proper mesh raycasting — filter to interactable meshes only
        let filter = |entity: Entity| interactable_filter.contains(entity);
        let hits = raycast.cast_ray(ray, &MeshRayCastSettings::default().with_filter(&filter));

        let mut closest_hit: Option<(Entity, f32, String)> = None;

        // Log hit count for debugging
        if !hits.is_empty() {
            log::trace!("Ray hit {} entities", hits.len());
        }

        // Check if any of the hits are interactable entities
        for (entity, hit_data) in hits.iter() {
            // First check if this entity has Interactable
            if let Ok((_, object_global_transform, interactable, parent_ref)) =
                interactable_query.get(*entity)
            {
                // Use parent entity if this is a child mesh, otherwise use the entity itself
                let target_entity = parent_ref.map(|p| p.0).unwrap_or(*entity);

                // For distance calculation, use parent position if available, otherwise child position
                let reference_pos = if let Some(parent) = parent_ref {
                    // Get parent's position for consistent distance calculation
                    if let Ok((_, parent_global_transform, _, _)) = interactable_query.get(parent.0)
                    {
                        parent_global_transform.translation()
                    } else {
                        object_global_transform.translation()
                    }
                } else {
                    object_global_transform.translation()
                };

                // Check distance constraint using the reference position
                let floor_distance = (Vec2::new(camera_pos.x, camera_pos.z)
                    - Vec2::new(reference_pos.x, reference_pos.z))
                .length();

                if floor_distance <= interactable.distance
                    && hit_data.distance <= interactable.distance
                {
                    let name = interactable.name.clone();

                    match closest_hit {
                        None => {
                            closest_hit = Some((target_entity, hit_data.distance, name.clone()));
                            log::trace!(
                                "Found interactable: {} at distance {}",
                                name,
                                hit_data.distance
                            );
                        }
                        Some((_, current_distance, _)) => {
                            if hit_data.distance < current_distance {
                                closest_hit =
                                    Some((target_entity, hit_data.distance, name.clone()));
                                log::trace!(
                                    "Found closer interactable: {} at distance {}",
                                    name,
                                    hit_data.distance
                                );
                            }
                        }
                    }
                }
            } else {
                log::trace!(
                    "Hit entity {:?} but it has no Interactable component",
                    entity
                );
            }
        }

        // Store the entity we're currently interacting with
        if let Some((entity, _, name)) = closest_hit {
            current_interaction = Some(entity);
            log::trace!("Current interaction target: {} (entity {:?})", name, entity);
        }
    }

    // Remove HoveredOver from entities that are no longer being looked at
    for entity in marked_query.iter() {
        if Some(entity) != current_interaction {
            commands.entity(entity).remove::<HoveredOver>();
        }
    }

    // Add HoveredOver to the new interaction target (if any)
    if let Some(entity) = current_interaction {
        // Only insert if it doesn't already have it
        if !marked_query.contains(entity) {
            commands.entity(entity).insert(HoveredOver);
        }
    }
}

#[derive(Message)]
pub struct InteractableClicked {
    pub entity: Entity,
}

pub fn detect_interactable_clicks(
    mouse_input: Res<ButtonInput<MouseButton>>,
    hovered_query: Query<(Entity, &Interactable), With<HoveredOver>>,
    mut click_events: MessageWriter<InteractableClicked>,
) {
    if mouse_input.just_pressed(MouseButton::Left) {
        for (entity, interactable) in hovered_query.iter() {
            log::info!("Clicked on interactable: {}", interactable.name);

            // Emit event
            click_events.write(InteractableClicked { entity });
        }
    }
}

#[derive(Component)]
pub struct InteractionText;

pub fn show_interaction_text(
    mut commands: Commands,
    interactable_query: Query<(Entity, &Transform), (With<Interactable>, With<HoveredOver>)>,
    text_query: Query<Entity, With<InteractionText>>,
) {
    // Remove existing interaction text
    for entity in text_query.iter() {
        commands.entity(entity).despawn();
    }

    // Add text for any interactable being interacted with
    for (_, transform) in interactable_query.iter() {
        let text_position = transform.translation + Vec3::new(0.0, 2.0, 0.0); // Hover above the object

        commands.spawn((
            GameEntity,
            InteractionText,
            Text2d::new("Boo!"),
            TextFont {
                font_size: 60.0,
                ..default()
            },
            TextColor(Color::srgb(1.0, 1.0, 1.0)), // White text
            Transform::from_translation(text_position),
            GlobalTransform::default(),
        ));
    }
}

// Main handler for all interactable clicks
pub fn handle_interactable_clicks(
    mut click_events: MessageReader<super::gltf_loading::InteractableClicked>,
) {
    for event in click_events.read() {
        let _entity = event.entity;
    }
}
