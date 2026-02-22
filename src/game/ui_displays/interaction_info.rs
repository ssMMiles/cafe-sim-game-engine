use bevy::prelude::*;

use crate::game::interaction::{HoveredOver, Interactable};

use super::shared::{DisplayBuilder, DisplayPosition};

#[derive(Component)]
pub struct InteractionInfoDisplay;

pub fn setup_interaction_info_display(mut commands: Commands) {
    create_interaction_info_display(&mut commands, None);
}

pub fn update_interaction_info_display(
    mut commands: Commands,
    interaction_info_query: Query<Entity, With<InteractionInfoDisplay>>,
    interactable_query: Query<&Interactable, With<HoveredOver>>,
) {
    for entity in interaction_info_query.iter() {
        commands.entity(entity).despawn();
    }

    let current_interaction = interactable_query.iter().next();
    create_interaction_info_display(&mut commands, current_interaction);
}

fn create_interaction_info_display(commands: &mut Commands, interaction: Option<&Interactable>) {
    let interaction_text = match interaction {
        Some(interactable) => format!("Interacting: {}", interactable.name),
        None => "No interaction".to_string(),
    };

    DisplayBuilder::new()
        .position(DisplayPosition::TopRight {
            top: 180.0,
            right: 20.0,
        })
        .build(commands, InteractionInfoDisplay, |parent| {
            parent.spawn((
                Text::new(interaction_text),
                TextFont {
                    font_size: 14.0,
                    ..default()
                },
                TextColor(if interaction.is_some() {
                    Color::srgb(0.4, 1.0, 0.4)
                } else {
                    Color::srgb(0.8, 0.8, 0.8)
                }),
            ));
        });
}
