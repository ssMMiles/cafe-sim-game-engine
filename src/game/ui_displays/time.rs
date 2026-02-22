use bevy::prelude::*;

use crate::game::day_night::TimeOfDay;

use super::shared::{DisplayBuilder, DisplayPosition};

#[derive(Component)]
pub struct TimeDisplay;

pub fn setup_time_display(mut commands: Commands, time_of_day: Res<TimeOfDay>) {
    create_time_display(&mut commands, &time_of_day);
}

pub fn update_time_display(
    mut commands: Commands,
    time_of_day: Res<TimeOfDay>,
    time_query: Query<Entity, With<TimeDisplay>>,
) {
    for entity in time_query.iter() {
        commands.entity(entity).despawn();
    }

    create_time_display(&mut commands, &time_of_day);
}

fn create_time_display(commands: &mut Commands, time_of_day: &TimeOfDay) {
    let time_str = time_of_day.format_time();

    DisplayBuilder::new()
        .position(DisplayPosition::TopLeft {
            top: 70.0,
            left: 20.0,
        })
        .build(commands, TimeDisplay, |parent| {
            parent.spawn((
                Text::new(format!("Time: {}", time_str)),
                TextFont {
                    font_size: 16.0,
                    ..default()
                },
                TextColor(Color::srgb(1.0, 1.0, 1.0)),
            ));
        });
}
