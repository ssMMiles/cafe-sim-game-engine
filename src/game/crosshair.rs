use bevy::prelude::*;

use crate::{game::GameEntity, settings::UserSettings};

#[derive(Component)]
pub struct Crosshair;

pub fn toggle_crosshair(mut settings: ResMut<UserSettings>, input: Res<ButtonInput<KeyCode>>) {
    if input.just_pressed(KeyCode::KeyT)
        && (input.pressed(KeyCode::ShiftLeft) || input.pressed(KeyCode::ShiftRight))
    {
        settings.toggle_crosshair();
    }
}

pub fn update_crosshair_display(
    mut commands: Commands,
    settings: Res<UserSettings>,
    crosshair_query: Query<Entity, With<Crosshair>>,
) {
    if settings.is_changed() {
        // Remove existing crosshair
        for entity in crosshair_query.iter() {
            commands.entity(entity).despawn();
        }

        // Add crosshair if visible
        if settings.crosshair_visible {
            // Create simple square crosshair at center of screen
            commands.spawn((
                GameEntity,
                Crosshair,
                Node {
                    position_type: PositionType::Absolute,
                    left: Val::Percent(50.0),
                    top: Val::Percent(50.0),
                    width: Val::Px(4.0),
                    height: Val::Px(4.0),
                    ..default()
                },
                BackgroundColor(Color::srgb(1.0, 1.0, 1.0)), // White square
            ));
        }
    }
}
