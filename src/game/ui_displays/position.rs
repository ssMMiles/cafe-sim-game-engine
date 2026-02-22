use bevy::prelude::*;

use crate::game::camera::PlayerCamera;

use super::shared::{DisplayBuilder, DisplayPosition};

#[derive(Component)]
pub struct PositionDisplay;

pub fn setup_position_display(mut commands: Commands) {
    create_position_display(&mut commands, Vec3::ZERO);
}

pub fn update_position_display(
    mut commands: Commands,
    position_query: Query<Entity, With<PositionDisplay>>,
    camera_query: Query<&Transform, With<PlayerCamera>>,
) {
    for entity in position_query.iter() {
        commands.entity(entity).despawn();
    }

    let camera_position = if let Ok(transform) = camera_query.single() {
        transform.translation
    } else {
        Vec3::ZERO
    };

    create_position_display(&mut commands, camera_position);
}

fn create_position_display(commands: &mut Commands, position: Vec3) {
    DisplayBuilder::new()
        .position(DisplayPosition::TopRight {
            top: 240.0,
            right: 20.0,
        })
        .build(commands, PositionDisplay, |parent| {
            parent.spawn((
                Text::new("Position"),
                TextFont {
                    font_size: 14.0,
                    ..default()
                },
                TextColor(Color::srgb(1.0, 1.0, 1.0)),
            ));

            parent
                .spawn((Node {
                    flex_direction: FlexDirection::Column,
                    row_gap: Val::Px(4.0),
                    ..default()
                },))
                .with_children(|coords_parent| {
                    coords_parent.spawn((
                        Text::new(format!("X: {:.1}", position.x)),
                        TextFont {
                            font_size: 12.0,
                            ..default()
                        },
                        TextColor(Color::srgb(0.8, 0.8, 0.8)),
                    ));

                    coords_parent.spawn((
                        Text::new(format!("Y: {:.1}", position.y)),
                        TextFont {
                            font_size: 12.0,
                            ..default()
                        },
                        TextColor(Color::srgb(0.8, 0.8, 0.8)),
                    ));

                    coords_parent.spawn((
                        Text::new(format!("Z: {:.1}", position.z)),
                        TextFont {
                            font_size: 12.0,
                            ..default()
                        },
                        TextColor(Color::srgb(0.8, 0.8, 0.8)),
                    ));
                });
        });
}
