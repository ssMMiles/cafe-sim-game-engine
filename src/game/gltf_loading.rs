use crate::game::{
    GameEntity,
    gfx::toon_material::{
        DefaultToonLightmap, ProbeVolumeImages, ToonMat, ToonMaterial, ToonSource, ToonSun,
    },
};
pub use engine_api::DynamicLit;

use super::gfx::camera::PlayerCamera;
use bevy::{
    log,
    pbr::ExtendedMaterial,
    picking::mesh_picking::ray_cast::{MeshRayCast, MeshRayCastSettings},
    prelude::*,
    world_serialization::WorldInstanceReady,
};

/// Overrides a specific material after scene load, replacing all mesh materials.
#[derive(Component, Clone)]
pub struct PendingMaterial {
    pub base_color: Color,
    pub metallic: f32,
    pub perceptual_roughness: f32,
    pub alpha_mode: bevy::material::AlphaMode,
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

pub fn on_scene_ready(
    trigger: On<WorldInstanceReady>,
    mut commands: Commands,
    entity_query: Query<(
        Option<&Interactable>,
        Option<&PendingMaterial>,
        Option<&ConvertToToon>,
        Option<&DynamicLit>,
        Option<&Children>,
    )>,
    mesh_query: Query<(Entity, Option<&MeshMaterial3d<StandardMaterial>>), With<Mesh3d>>,
    children_query: Query<&Children>,
    mut std_materials: ResMut<Assets<StandardMaterial>>,
    mut toon_materials: ResMut<Assets<ExtendedMaterial<StandardMaterial, ToonMaterial>>>,
    default_lightmap: Option<Res<DefaultToonLightmap>>,
    probes: Res<ProbeVolumeImages>,
    sun: Res<ToonSun>,
) {
    let entity = trigger.entity;

    let Ok((
        maybe_interactable,
        maybe_pending_material,
        maybe_convert_toon,
        maybe_dynamic,
        maybe_children,
    )) = entity_query.get(entity)
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
            "Applied material to entity {:?} via WorldInstanceReady",
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
        // A scene that moves is lit from the probe volume at its own world
        // position; everything else from its baked lightmap.
        let source = if maybe_dynamic.is_some() {
            ToonSource::ProbeVolume
        } else {
            ToonSource::Lightmap
        };
        let template = {
            let mut m = ToonMaterial::new(source, lightmap_handle, &probes);
            sun.apply(&mut m.shading);
            m
        };

        convert_to_toon_recursive(
            &mut commands,
            children,
            &mesh_query,
            &children_query,
            &std_materials,
            &mut toon_materials,
            &template,
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
    template: &ToonMaterial,
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
                extension: template.clone(),
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
                template,
            );
        }
    }
}

pub fn spawn_interactable_scene<B: Bundle>(commands: &mut Commands, bundle: B) -> Entity {
    commands.spawn(bundle).observe(on_scene_ready).id()
}

/// Keep converted scenes' material source in step with their [`DynamicLit`]
/// marker.
///
/// [`on_scene_ready`] picks lightmap-vs-probes once, when the instance
/// readies, and removes `ConvertToToon` — so a marker that arrives later (a
/// script binding hydrating after the scene settled, or attached in the
/// inspector) or leaves (detach, hot-reload dehydration) would otherwise be
/// silently ignored until the entity respawns. Every toon material binds the
/// shared probe volume precisely so `shading.source` can flip without
/// touching a bind group; this rewrites it on the meshes under a root whose
/// marker just changed.
///
/// Editor-only (`run_if(in_state(EditorMode::Editor))`): attach/detach and
/// hydration timing are authoring traffic. A removal while in gameplay (a
/// plugin unloaded mid-session) is missed — `RemovedComponents` buffers for
/// two frames — leaving that instance probe-lit until it respawns and the
/// conversion reads the marker's absence.
pub fn sync_dynamic_lit_source(
    added: Query<Entity, Added<DynamicLit>>,
    mut removed: RemovedComponents<DynamicLit>,
    children_query: Query<&Children>,
    material_query: Query<&MeshMaterial3d<ToonMat>>,
    mut materials: ResMut<Assets<ToonMat>>,
) {
    // Removals first, so an entity dehydrated and re-hydrated inside the same
    // detection window (hot-reload) lands on the re-add's source.
    let changes = removed
        .read()
        .map(|e| (e, ToonSource::Lightmap))
        .chain(added.iter().map(|e| (e, ToonSource::ProbeVolume)));
    for (root, source) in changes {
        let mut stack = vec![root];
        while let Some(entity) = stack.pop() {
            if let Ok(handle) = material_query.get(entity)
                && let Some(mut mat) = materials.get_mut(&handle.0)
            {
                mat.extension.shading.source = source as u32;
            }
            if let Ok(children) = children_query.get(entity) {
                stack.extend(children.iter());
            }
        }
    }
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

    if let Some(entity) = current_interaction
        && !marked_query.contains(entity)
    {
        commands.entity(entity).insert(HoveredOver);
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
                font_size: FontSize::Px(60.0),
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

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::ecs::system::RunSystemOnce;

    fn source_of(world: &World, handle: &Handle<ToonMat>) -> u32 {
        world
            .resource::<Assets<ToonMat>>()
            .get(handle)
            .unwrap()
            .extension
            .shading
            .source
    }

    /// `on_scene_ready` picks the source exactly once, so a `DynamicLit`
    /// toggled afterwards (script hydration, inspector attach/detach,
    /// hot-reload) must be carried into the already-built materials by
    /// `sync_dynamic_lit_source` — including on nested sub-meshes.
    #[test]
    fn toggling_dynamic_lit_rewrites_a_converted_scenes_source() {
        let mut world = World::new();
        world.insert_resource(Assets::<ToonMat>::default());
        let add_material = |world: &mut World| {
            world
                .resource_mut::<Assets<ToonMat>>()
                .add(bevy::pbr::ExtendedMaterial {
                    base: StandardMaterial::default(),
                    extension: ToonMaterial::default(), // ToonSource::Lightmap
                })
        };
        let child_mat = add_material(&mut world);
        let nested_mat = add_material(&mut world);

        let root = world.spawn_empty().id();
        let child = world.spawn(MeshMaterial3d(child_mat.clone())).id();
        let nested = world.spawn(MeshMaterial3d(nested_mat.clone())).id();
        world.entity_mut(root).add_child(child);
        world.entity_mut(child).add_child(nested);

        world.entity_mut(root).insert(DynamicLit);
        world.run_system_once(sync_dynamic_lit_source).unwrap();
        assert_eq!(
            source_of(&world, &child_mat),
            ToonSource::ProbeVolume as u32
        );
        assert_eq!(
            source_of(&world, &nested_mat),
            ToonSource::ProbeVolume as u32
        );

        world.entity_mut(root).remove::<DynamicLit>();
        world.run_system_once(sync_dynamic_lit_source).unwrap();
        assert_eq!(source_of(&world, &child_mat), ToonSource::Lightmap as u32);
        assert_eq!(source_of(&world, &nested_mat), ToonSource::Lightmap as u32);
    }
}
