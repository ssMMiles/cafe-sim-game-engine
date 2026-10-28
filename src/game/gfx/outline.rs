//! Hover outlines for interactable objects.
//!
//! **Currently inert.** This was built on `bevy_mod_outline`; that dependency
//! is gone and every `OutlineVolume` insert below is commented out, so
//! `update_outline` walks entity hierarchies each frame without producing any
//! visual effect, and `Shift+O` toggles a setting nothing reads. Either wire
//! up a replacement or delete the module and its two system registrations
//! (`gfx/mod.rs`, `editor/mod.rs`).

use bevy::prelude::*;
// use bevy_mod_outline::OutlineVolume;

use crate::{
    game::gltf_loading::{HoveredOver, Interactable},
    settings::UserSettings,
};

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
            for (_, _, children) in interactable_query.iter() {
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
    for (_, interaction, children) in interactable_query.iter() {
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
        // Also remove outlines from children if this is a scene
        if let Ok((_, children)) = all_interactables.get(entity)
            && let Some(children) = children
        {
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

// NOTE: this walk is currently inert. The `OutlineVolume` inserts it existed to
// perform were commented out when the `bevy_mod_outline` dependency was dropped,
// so the body only recurses. Kept as scaffolding for a replacement outline
// implementation; delete it (and `update_outline`'s registration) if none is
// coming — see the note at the top of this file.
#[allow(clippy::only_used_in_recursion)]
fn propagate_outline_to_children(
    commands: &mut Commands,
    children: &Children,
    add_outline: bool,
    mesh_query: &Query<Entity, With<Mesh3d>>,
    children_query: &Query<&Children>,
) {
    for child in children.iter() {
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
