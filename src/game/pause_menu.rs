use bevy::{prelude::*, window::CursorGrabMode};

use super::SimulationState;
use crate::AppState;

#[derive(Component)]
pub struct PauseMenu;

#[derive(Component)]
pub enum PauseMenuButton {
    Resume,
    ExitGame,
}

pub fn handle_pause_input(
    keyboard_input: Res<ButtonInput<KeyCode>>,
    current_state: Res<State<SimulationState>>,
    mut next_state: ResMut<NextState<SimulationState>>,
    mut windows: Query<&mut Window>,
) {
    if keyboard_input.just_pressed(KeyCode::Escape) {
        match current_state.get() {
            SimulationState::Running => {
                next_state.set(SimulationState::Paused);
                // Unlock cursor when pausing
                if let Ok(mut window) = windows.get_single_mut() {
                    window.cursor_options.grab_mode = CursorGrabMode::None;
                    window.cursor_options.visible = true;
                }
            }
            SimulationState::Paused => {
                next_state.set(SimulationState::Running);
                // Lock cursor when resuming
                if let Ok(mut window) = windows.get_single_mut() {
                    window.cursor_options.grab_mode = CursorGrabMode::Locked;
                    window.cursor_options.visible = false;
                }
            }
        }
    }
}

pub fn setup_pause_menu(mut commands: Commands) {
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
            PauseMenu,
            background,
            BackgroundColor(Color::srgba(0.0, 0.0, 0.0, 0.8)),
        ))
        .with_children(|parent| {
            // Pause title
            parent.spawn((
                Text::new("PAUSED"),
                TextFont {
                    font_size: 48.0,
                    ..Default::default()
                },
                TextColor(Color::WHITE),
                Node {
                    margin: UiRect::bottom(Val::Px(50.0)),
                    ..Default::default()
                },
            ));

            // Resume button
            parent
                .spawn((
                    Button,
                    Node {
                        width: Val::Px(200.0),
                        height: Val::Px(65.0),
                        border: UiRect::all(Val::Px(5.0)),
                        justify_content: JustifyContent::Center,
                        align_items: AlignItems::Center,
                        margin: UiRect::bottom(Val::Px(20.0)),
                        ..default()
                    },
                    BackgroundColor(Color::srgb(0.4, 0.4, 0.2)),
                    PauseMenuButton::Resume,
                ))
                .with_child((
                    Text::new("Resume"),
                    TextColor(Color::srgb(0.9, 0.9, 0.9)),
                ));

            // Exit button
            parent
                .spawn((
                    Button,
                    Node {
                        width: Val::Px(200.0),
                        height: Val::Px(65.0),
                        border: UiRect::all(Val::Px(5.0)),
                        justify_content: JustifyContent::Center,
                        align_items: AlignItems::Center,
                        ..default()
                    },
                    BackgroundColor(Color::srgb(0.6, 0.2, 0.2)),
                    PauseMenuButton::ExitGame,
                ))
                .with_child((
                    Text::new("Exit Game"),
                    TextColor(Color::srgb(0.9, 0.9, 0.9)),
                ));
        });
}

pub fn cleanup_pause_menu(mut commands: Commands, query: Query<Entity, With<PauseMenu>>) {
    for entity in query.iter() {
        commands.entity(entity).despawn_recursive();
    }
}

pub fn handle_pause_menu(
    interaction_query: Query<
        (&Interaction, &PauseMenuButton),
        (Changed<Interaction>, With<Button>),
    >,
    mut simulation_state: ResMut<NextState<SimulationState>>,
    mut app_state: ResMut<NextState<AppState>>,
    mut windows: Query<&mut Window>,
) {
    for (interaction, action) in interaction_query.iter() {
        if *interaction == Interaction::Pressed {
            match action {
                PauseMenuButton::Resume => {
                    simulation_state.set(SimulationState::Running);
                    // Lock cursor when resuming
                    if let Ok(mut window) = windows.get_single_mut() {
                        window.cursor_options.grab_mode = CursorGrabMode::Locked;
                        window.cursor_options.visible = false;
                    }
                }
                PauseMenuButton::ExitGame => {
                    app_state.set(AppState::MainMenu);
                    simulation_state.set(SimulationState::Running);
                    // Unlock cursor when exiting to main menu
                    if let Ok(mut window) = windows.get_single_mut() {
                        window.cursor_options.grab_mode = CursorGrabMode::None;
                        window.cursor_options.visible = true;
                    }
                }
            }
        }
    }
}