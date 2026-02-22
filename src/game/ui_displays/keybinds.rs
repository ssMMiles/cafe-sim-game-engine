use bevy::prelude::*;

use crate::settings::UserSettings;

use super::shared::{DisplayBuilder, DisplayPosition};

#[derive(Component)]
pub struct KeybindsDisplay;

pub fn setup_keybinds_display(mut commands: Commands, settings: Res<UserSettings>) {
    create_keybinds_display(&mut commands, &settings);
}

pub fn update_keybinds_display(
    mut commands: Commands,
    settings: Res<UserSettings>,
    keybinds_query: Query<Entity, With<KeybindsDisplay>>,
) {
    if settings.is_changed() {
        for entity in keybinds_query.iter() {
            commands.entity(entity).despawn();
        }

        create_keybinds_display(&mut commands, &settings);
    }
}

fn create_keybinds_display(commands: &mut Commands, settings: &UserSettings) {
    let settings_info = settings.get_settings_info();

    DisplayBuilder::new()
        .position(DisplayPosition::TopRight {
            top: 20.0,
            right: 20.0,
        })
        .build(commands, KeybindsDisplay, |parent| {
            parent.spawn((
                Text::new("Controls"),
                TextFont {
                    font_size: 16.0,
                    ..default()
                },
                TextColor(Color::srgb(1.0, 1.0, 1.0)),
            ));

            parent
                .spawn((Node {
                    flex_direction: FlexDirection::Column,
                    row_gap: Val::Px(8.0),
                    ..default()
                },))
                .with_children(|settings_parent| {
                    for setting in settings_info {
                        settings_parent
                            .spawn((Node {
                                flex_direction: FlexDirection::Row,
                                justify_content: JustifyContent::SpaceBetween,
                                width: Val::Px(200.0),
                                ..default()
                            },))
                            .with_children(|row| {
                                row.spawn((
                                    Text::new(format!("{} [{}]:", setting.name, setting.keybind)),
                                    TextFont {
                                        font_size: 14.0,
                                        ..default()
                                    },
                                    TextColor(Color::srgb(0.8, 0.8, 0.8)),
                                ));

                                row.spawn((
                                    Text::new(&setting.current_value),
                                    TextFont {
                                        font_size: 14.0,
                                        ..default()
                                    },
                                    TextColor(Color::srgb(0.6, 0.8, 1.0)),
                                ));
                            });
                    }
                });
        });
}
