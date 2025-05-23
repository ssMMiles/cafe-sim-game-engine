use bevy::{prelude::*, window::CursorGrabMode};

use super::SimulationState;
use crate::{menu::ui_builder::{MenuCommandsExt, MenuContainerExt, UIColors, ButtonStyle}, AppState};

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
    commands
        .spawn_menu_container(PauseMenu, UIColors::BACKGROUND_OVERLAY)
        .with_children(|parent| {
            parent.spawn_title("PAUSED");
            parent.spawn_button("Resume", PauseMenuButton::Resume, ButtonStyle::Primary);
            parent.spawn_button("Exit Game", PauseMenuButton::ExitGame, ButtonStyle::Danger);
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