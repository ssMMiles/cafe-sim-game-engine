use bevy::{log, prelude::*};

use crate::AppState;

use super::MenuScreen;

#[derive(Component, Debug)]
pub struct MainMenuScreen;

#[derive(Component, Debug)]
pub enum MainMenuButton {
    StartGame,
    Settings,
}

pub fn setup_main_menu(mut commands: Commands) {
    log::info!("Setting up the main menu screen");

    let background = Node {
        width: Val::Percent(100.0),
        height: Val::Percent(100.0),
        align_items: AlignItems::Center,
        justify_content: JustifyContent::Center,
        flex_direction: FlexDirection::Column,
        ..Default::default()
    };

    commands
        .spawn((
            MainMenuScreen,
            background,
            BackgroundColor(Color::srgba(0.1, 0.1, 0.8, 1.0)),
        ))
        .with_children(|parent| {
            parent
                .spawn((
                    Button,
                    Node {
                        width: Val::Px(150.0),
                        height: Val::Px(65.0),
                        border: UiRect::all(Val::Px(5.0)),
                        // horizontally center child text
                        justify_content: JustifyContent::Center,
                        // vertically center child text
                        align_items: AlignItems::Center,
                        ..default()
                    },
                    BackgroundColor(Color::srgb(0.4, 0.4, 0.2)),
                    MainMenuButton::StartGame,
                ))
                .with_child((
                    Text::new("Start Game"),
                    TextColor(Color::srgb(0.9, 0.9, 0.9)),
                ));

            parent
                .spawn((
                    Button,
                    Node {
                        width: Val::Px(150.0),
                        height: Val::Px(65.0),
                        border: UiRect::all(Val::Px(5.0)),
                        // horizontally center child text
                        justify_content: JustifyContent::Center,
                        // vertically center child text
                        align_items: AlignItems::Center,
                        ..default()
                    },
                    BackgroundColor(Color::srgb(0.4, 0.4, 0.2)),
                    MainMenuButton::Settings,
                ))
                .with_child((Text::new("Settings"), TextColor(Color::srgb(0.9, 0.9, 0.9))));
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
