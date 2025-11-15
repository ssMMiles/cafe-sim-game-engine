use bevy::prelude::*;
// use bevy_mod_outline::OutlineVolume;

use super::interaction::{HoveredOver, Interactable};
use crate::settings::UserSettings;

pub fn update_outline(
    mut commands: Commands,
    settings: Res<UserSettings>,
    interactable_query: Query<
        (Entity, Option<&HoveredOver>, Option<&Children>),
        (With<Interactable>, Changed<HoveredOver>),
    >,
    all_interactables: Query<(Entity, Option<&Children>), With<Interactable>>,
    mut removed_interactions: RemovedComponents<HoveredOver>,
    mesh_query: Query<Entity, With<Mesh3d>>,
    children_query: Query<&Children>,
) {
    // Only process outlines if they're enabled
    if !settings.outline_visible {
        // If outlines were just disabled, remove any existing outlines
        if settings.is_changed() {
            for (entity, _, children) in interactable_query.iter() {
                // commands.entity(entity).remove::<OutlineVolume>();
                if let Some(children) = children {
                    propagate_outline_to_children(
                        &mut commands,
                        children,
                        false,
                        &mesh_query,
                        &children_query,
                    );
                }
            }
        }
        return;
    }

    // Handle entities that are being interacted with (add white outline)
    for (entity, interaction, children) in interactable_query.iter() {
        if interaction.is_some() {
            // Add outline to the main entity
            // commands.entity(entity).insert(OutlineVolume {
            //     visible: true,
            //     width: 3.0,
            //     colour: Color::srgb(1.0, 1.0, 1.0), // White outline
            // });

            // If entity has children (like GLTF scenes), propagate outline to mesh children
            if let Some(children) = children {
                propagate_outline_to_children(
                    &mut commands,
                    children,
                    true,
                    &mesh_query,
                    &children_query,
                );
            }
        }
    }

    // Handle entities that are no longer being interacted with (remove outline)
    for entity in removed_interactions.read() {
        // Check if entity still exists before trying to remove component
        if let Ok(mut entity_commands) = commands.get_entity(entity) {
            // entity_commands.remove::<OutlineVolume>();
        }

        // Also remove outlines from children if this is a scene
        if let Ok((_, children)) = all_interactables.get(entity) {
            if let Some(children) = children {
                propagate_outline_to_children(
                    &mut commands,
                    children,
                    false,
                    &mesh_query,
                    &children_query,
                );
            }
        }
    }
}

fn propagate_outline_to_children(
    commands: &mut Commands,
    children: &Children,
    add_outline: bool,
    mesh_query: &Query<Entity, With<Mesh3d>>,
    children_query: &Query<&Children>,
) {
    for child in children.iter() {
        // If this child is a mesh, add or remove outline
        if mesh_query.contains(child) {
            // Check if entity still exists before trying to modify it
            if let Ok(mut entity_commands) = commands.get_entity(child) {
                if add_outline {
                    // entity_commands.insert(OutlineVolume {
                    //     visible: true,
                    //     width: 3.0,
                    //     colour: Color::srgb(1.0, 1.0, 1.0),
                    // });
                } else {
                    // entity_commands.remove::<OutlineVolume>();
                }
            }
        }

        // Recursively check grandchildren
        if let Ok(grandchildren) = children_query.get(child) {
            propagate_outline_to_children(
                commands,
                grandchildren,
                add_outline,
                mesh_query,
                children_query,
            );
        }
    }
}

pub fn toggle_outline(mut settings: ResMut<UserSettings>, input: Res<ButtonInput<KeyCode>>) {
    if input.just_pressed(KeyCode::KeyO)
        && (input.pressed(KeyCode::ShiftLeft) || input.pressed(KeyCode::ShiftRight))
    {
        settings.toggle_outline();
    }
}
