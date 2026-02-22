use bevy::prelude::*;

use crate::{
    menu::ui_builder::{ButtonStyle, MenuCommandsExt, MenuContainerExt, UIColors},
    settings::{MouseSensitivitySetting, MovementSpeedSetting, SliderSetting, UserSettings},
};

use super::types::{
    PauseMenu, PauseMenuButton, PauseSettings, SettingsButton, SliderHandle, SliderLabel,
    SliderTrack,
};

pub fn setup_pause_menu(mut commands: Commands) {
    commands
        .spawn_menu_container(PauseMenu, UIColors::BACKGROUND_OVERLAY)
        .with_children(|parent| {
            parent.spawn_title("PAUSED");
            parent.spawn_button("Resume", PauseMenuButton::Resume, ButtonStyle::Primary);
            parent.spawn_button("Settings", PauseMenuButton::Settings, ButtonStyle::Primary);
            parent.spawn_button("Exit Game", PauseMenuButton::ExitGame, ButtonStyle::Danger);
        });
}

pub fn setup_pause_settings(mut commands: Commands, settings: Res<UserSettings>) {
    commands
        .spawn_menu_container(PauseSettings, UIColors::BACKGROUND_OVERLAY)
        .with_children(|parent| {
            parent.spawn_title("SETTINGS");
            parent.spawn_slider(
                MouseSensitivitySetting::get_label(),
                MouseSensitivitySetting::get_value(&settings),
                MouseSensitivitySetting::get_range().0,
                MouseSensitivitySetting::get_range().1,
                SliderHandle::<MouseSensitivitySetting>::default(),
                SliderLabel::<MouseSensitivitySetting>::default(),
                SliderTrack::<MouseSensitivitySetting>::default(),
            );
            parent.spawn_slider(
                MovementSpeedSetting::get_label(),
                MovementSpeedSetting::get_value(&settings),
                MovementSpeedSetting::get_range().0,
                MovementSpeedSetting::get_range().1,
                SliderHandle::<MovementSpeedSetting>::default(),
                SliderLabel::<MovementSpeedSetting>::default(),
                SliderTrack::<MovementSpeedSetting>::default(),
            );
            parent.spawn_button("Back", SettingsButton::Back, ButtonStyle::Primary);
        });
}

pub fn cleanup_pause_menu(mut commands: Commands, query: Query<Entity, With<PauseMenu>>) {
    for entity in query.iter() {
        commands.entity(entity).despawn();
    }
}

pub fn cleanup_pause_settings(mut commands: Commands, query: Query<Entity, With<PauseSettings>>) {
    for entity in query.iter() {
        commands.entity(entity).despawn();
    }
}
