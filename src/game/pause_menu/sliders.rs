use bevy::prelude::*;

use crate::settings::{slider_constants, SliderSetting, UserSettings};

use super::types::{SliderHandle, SliderLabel, SliderTrack};

pub fn handle_slider<T: Component + SliderSetting>(
    mut handle_query: Query<(&Interaction, &mut Node), With<SliderHandle<T>>>,
    track_query: Query<&Interaction, (With<SliderTrack<T>>, Without<SliderHandle<T>>)>,
    mut settings: ResMut<UserSettings>,
    mouse_input: Res<ButtonInput<MouseButton>>,
    windows: Query<&Window>,
) {
    let (min_value, max_value) = T::get_range();

    let is_dragging = handle_query.iter().any(|(interaction, _)| {
        (*interaction == Interaction::Pressed || *interaction == Interaction::Hovered)
            && mouse_input.pressed(MouseButton::Left)
    });

    for interaction in track_query.iter() {
        if *interaction == Interaction::Pressed {
            if let Ok(window) = windows.single() {
                if let Some(cursor_pos) = window.cursor_position() {
                    let normalized_value = calculate_normalized_position(cursor_pos, window);

                    if let Ok((_, mut handle_node)) = handle_query.single_mut() {
                        handle_node.left =
                            Val::Percent(calculate_handle_position(normalized_value));
                    }

                    let new_value = min_value + normalized_value * (max_value - min_value);
                    T::set_value(&mut settings, new_value);
                }
            }
        }
    }

    if is_dragging {
        if let Ok(window) = windows.single() {
            if let Some(cursor_pos) = window.cursor_position() {
                let normalized_value = calculate_normalized_position(cursor_pos, window);

                if let Ok((_, mut handle_node)) = handle_query.single_mut() {
                    handle_node.left = Val::Percent(calculate_handle_position(normalized_value));
                }

                let new_value = min_value + normalized_value * (max_value - min_value);
                T::set_value(&mut settings, new_value);
            }
        }
    }
}

fn calculate_normalized_position(cursor_pos: Vec2, window: &Window) -> f32 {
    let window_width = window.resolution.width();
    let window_center_x = window_width / 2.0;

    let relative_x = cursor_pos.x - window_center_x;

    let slider_half_width = slider_constants::TRACK_WIDTH / 2.0;
    let clamped_x = relative_x.clamp(-slider_half_width, slider_half_width);
    (clamped_x + slider_half_width) / (slider_constants::TRACK_WIDTH)
}

fn calculate_handle_position(normalized_value: f32) -> f32 {
    let usable_width = slider_constants::TRACK_WIDTH - slider_constants::HANDLE_WIDTH;
    let handle_left_position = normalized_value * usable_width;
    (handle_left_position / slider_constants::TRACK_WIDTH) * 100.0
}

pub fn update_slider_label<T: Component + SliderSetting>(
    settings: Res<UserSettings>,
    mut label_query: Query<&mut Text, With<SliderLabel<T>>>,
) {
    if settings.is_changed() {
        for mut text in label_query.iter_mut() {
            let current_value = T::get_value(&settings);
            let format_str = T::get_format();
            let formatted_value = if format_str == "{:.3}" {
                format!("{:.3}", current_value)
            } else if format_str == "{:.2}" {
                format!("{:.2}", current_value)
            } else {
                format!("{}", current_value)
            };
            **text = format!("{}: {}", T::get_label(), formatted_value);
        }
    }
}
