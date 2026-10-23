use crate::game::{
    GameEntity,
    gfx::toon_material::{DefaultToonLightmap, ToonMaterial},
};

use super::gfx::camera::PlayerCamera;
use bevy::{
    log,
    pbr::ExtendedMaterial,
    picking::mesh_picking::ray_cast::{MeshRayCast, MeshRayCastSettings},
    prelude::*,
    scene::SceneInstanceReady,
};

/// Overrides a specific material after scene load, replacing all mesh materials.
#[derive(Component, Clone)]
pub struct PendingMaterial {
    pub base_color: Color,
    pub metallic: f32,
    pub perceptual_roughness: f32,
    pub alpha_mode: bevy::render::alpha::AlphaMode,
}

/// Converts GLTF-embedded StandardMaterials to toon materials on scene load,
/// preserving each mesh's original base colour. Saved lightmaps are applied
/// afterwards by `baking::object_lightmaps::apply_object_lightmaps`.
#[derive(Component)]
pub struct ConvertToToon;

pub fn apply_material_to_meshes(
    commands: &mut Commands,
    children: &Children,
    material: &Handle<StandardMaterial>,
    mesh_query: &Query<(Entity, Option<&MeshMaterial3d<StandardMaterial>>), With<Mesh3d>>,
    children_query: &Query<&Children>,
) {
    for child in children.iter() {
        if let Ok((entity, _)) = mesh_query.get(child) {
            commands
                .entity(entity)
                .insert(MeshMaterial3d(material.clone()));
        }

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

fn on_scene_ready(
    trigger: On<SceneInstanceReady>,
    mut commands: Commands,
    entity_query: Query<(
        Option<&Interactable>,
        Option<&PendingMaterial>,
        Option<&ConvertToToon>,
        Option<&Children>,
    )>,
    mesh_query: Query<(Entity, Option<&MeshMaterial3d<StandardMaterial>>), With<Mesh3d>>,
    children_query: Query<&Children>,
    mut std_materials: ResMut<Assets<StandardMaterial>>,
    mut toon_materials: ResMut<Assets<ExtendedMaterial<StandardMaterial, ToonMaterial>>>,
    default_lightmap: Option<Res<DefaultToonLightmap>>,
) {
    let entity = trigger.entity;

    let Ok((maybe_interactable, maybe_pending_material, maybe_convert_toon, maybe_children)) =
        entity_query.get(entity)
    else {
        return;
    };

    let Some(children) = maybe_children else {
        return;
    };

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

    if let Some(pending_material) = maybe_pending_material {
        let material = std_materials.add(StandardMaterial {
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

    if maybe_convert_toon.is_some() {
        // Every mesh starts on the shared white (unbaked) lightmap; the
        // object-lightmap system swaps in saved bakes per sub-mesh once the
        // toon materials exist.
        let lightmap_handle = default_lightmap
            .as_ref()
            .map(|r| r.0.clone())
            .unwrap_or_default();

        convert_to_toon_recursive(
            &mut commands,
            children,
            &mesh_query,
            &children_query,
            &std_materials,
            &mut toon_materials,
            lightmap_handle,
        );
        commands.entity(entity).remove::<ConvertToToon>();
    }
}

fn convert_to_toon_recursive(
    commands: &mut Commands,
    children: &Children,
    mesh_query: &Query<(Entity, Option<&MeshMaterial3d<StandardMaterial>>), With<Mesh3d>>,
    children_query: &Query<&Children>,
    std_materials: &Assets<StandardMaterial>,
    toon_materials: &mut Assets<ExtendedMaterial<StandardMaterial, ToonMaterial>>,
    lightmap: Handle<Image>,
) {
    for child in children.iter() {
        if let Ok((entity, maybe_mat)) = mesh_query.get(child) {
            // Clone the original GLTF material so textures and colours are preserved.
            let base = maybe_mat
                .and_then(|m| std_materials.get(&m.0))
                .cloned()
                .unwrap_or_default();

            let handle = toon_materials.add(ExtendedMaterial {
                base,
                extension: ToonMaterial {
                    lightmap: lightmap.clone(),
                    use_lightmap: 1,
                },
            });

            commands
                .entity(entity)
                .remove::<MeshMaterial3d<StandardMaterial>>()
                .insert(MeshMaterial3d(handle));
        }

        if let Ok(grandchildren) = children_query.get(child) {
            convert_to_toon_recursive(
                commands,
                grandchildren,
                mesh_query,
                children_query,
                std_materials,
                toon_materials,
                lightmap.clone(),
            );
        }
    }
}

pub fn spawn_interactable_scene<B: Bundle>(commands: &mut Commands, bundle: B) -> Entity {
    commands.spawn(bundle).observe(on_scene_ready).id()
}

fn propagate_to_mesh_children(
    commands: &mut Commands,
    children: &Children,
    interactable: &Interactable,
    parent_entity: Entity,
    mesh_query: &Query<(Entity, Option<&MeshMaterial3d<StandardMaterial>>), With<Mesh3d>>,
    children_query: &Query<&Children>,
) {
    for child in children.iter() {
        if let Ok((entity, _)) = mesh_query.get(child) {
            commands
                .entity(entity)
                .insert((interactable.clone(), InteractableParent(parent_entity)));
        }

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

        let ray = Ray3d::new(camera_pos, Dir3::new(camera_forward).unwrap());

        let filter = |entity: Entity| interactable_filter.contains(entity);
        let hits = raycast.cast_ray(ray, &MeshRayCastSettings::default().with_filter(&filter));

        let mut closest_hit: Option<(Entity, f32, String)> = None;

        if !hits.is_empty() {
            log::trace!("Ray hit {} entities", hits.len());
        }

        for (entity, hit_data) in hits.iter() {
            if let Ok((_, object_global_transform, interactable, parent_ref)) =
                interactable_query.get(*entity)
            {
                let target_entity = parent_ref.map(|p| p.0).unwrap_or(*entity);

                let reference_pos = if let Some(parent) = parent_ref {
                    if let Ok((_, parent_global_transform, _, _)) = interactable_query.get(parent.0)
                    {
                        parent_global_transform.translation()
                    } else {
                        object_global_transform.translation()
                    }
                } else {
                    object_global_transform.translation()
                };

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

        if let Some((entity, _, name)) = closest_hit {
            current_interaction = Some(entity);
            log::trace!("Current interaction target: {} (entity {:?})", name, entity);
        }
    }

    for entity in marked_query.iter() {
        if Some(entity) != current_interaction {
            commands.entity(entity).remove::<HoveredOver>();
        }
    }

    if let Some(entity) = current_interaction {
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
    for entity in text_query.iter() {
        commands.entity(entity).despawn();
    }

    for (_, transform) in interactable_query.iter() {
        let text_position = transform.translation + Vec3::new(0.0, 2.0, 0.0);

        commands.spawn((
            GameEntity,
            InteractionText,
            Text2d::new("Boo!"),
            TextFont {
                font_size: 60.0,
                ..default()
            },
            TextColor(Color::srgb(1.0, 1.0, 1.0)),
            Transform::from_translation(text_position),
            GlobalTransform::default(),
        ));
    }
}

pub fn handle_interactable_clicks(
    mut click_events: MessageReader<super::gltf_loading::InteractableClicked>,
) {
    for event in click_events.read() {
        let _entity = event.entity;
    }
}
