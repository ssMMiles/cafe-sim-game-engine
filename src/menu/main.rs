use bevy::{log, prelude::*};

use crate::{
    AppState,
    ui::{ButtonVariant, UiExt, UiTheme},
};

use super::MenuScreen;

#[derive(Component, Debug)]
pub struct MainMenuScreen;

#[derive(Component, Debug)]
pub enum MainMenuButton {
    StartGame,
    Settings,
}

pub fn setup_main_menu(mut commands: Commands, theme: Res<UiTheme>) {
    log::info!("Setting up the main menu screen");

    commands
        .spawn((
            MainMenuScreen,
            Node {
                width: Val::Percent(100.0),
                height: Val::Percent(100.0),
                align_items: AlignItems::Center,
                justify_content: JustifyContent::Center,
                flex_direction: FlexDirection::Column,
                ..default()
            },
            BackgroundColor(theme.bg_dark),
        ))
        .with_children(|parent| {
            parent.ui_button(
                "Start Game",
                MainMenuButton::StartGame,
                ButtonVariant::Primary,
                &theme,
            );
            parent.ui_button(
                "Settings",
                MainMenuButton::Settings,
                ButtonVariant::Primary,
                &theme,
            );
        });
}

pub fn handle_main_menu_button_press(
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
