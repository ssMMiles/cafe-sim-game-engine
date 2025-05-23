use bevy::{log, prelude::*};

use crate::AppState;

use super::{ui_builder::{MenuCommandsExt, MenuContainerExt, UIColors, ButtonStyle}, MenuScreen};

#[derive(Component, Debug)]
pub struct MainMenuScreen;

#[derive(Component, Debug)]
pub enum MainMenuButton {
    StartGame,
    Settings,
}

pub fn setup_main_menu(mut commands: Commands) {
    log::info!("Setting up the main menu screen");

    commands
        .spawn_menu_container(MainMenuScreen, UIColors::BACKGROUND_DARK)
        .with_children(|parent| {
            parent.spawn_button("Start Game", MainMenuButton::StartGame, ButtonStyle::Primary);
            parent.spawn_button("Settings", MainMenuButton::Settings, ButtonStyle::Primary);
        });
}

pub fn handle_main_menu_button_press(
    // mut commands: Commands,
    mut interaction_query: Query<
        (&Interaction, &MainMenuButton),
        (Changed<Interaction>, With<Button>),
    >,
    mut next_app_state: ResMut<NextState<AppState>>,
    mut next_menu_screen: ResMut<NextState<MenuScreen>>,
) {
    for (interaction, action) in interaction_query.iter_mut() {
        match *interaction {
            Interaction::Pressed => match action {
                MainMenuButton::StartGame => {
                    log::info!("Starting game");
                    next_app_state.set(AppState::InGame);
                }
                MainMenuButton::Settings => {
                    log::info!("Opening settings");
                    next_menu_screen.set(MenuScreen::Settings);
                }
            },
            _ => {}
        }
    }
}
