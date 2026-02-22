use bevy::{prelude::*, window::CursorOptions};

use crate::{game::SimulationState, AppState};

use super::{
    cursor::{handle_cursor_lock, set_cursor_for_menu, set_cursor_for_running},
    types::{PauseMenuButton, PauseMenuState, SettingsButton},
};

pub fn handle_pause_input(
    keyboard_input: Res<ButtonInput<KeyCode>>,
    current_sim_state: Res<State<SimulationState>>,
    current_menu_state: Res<State<PauseMenuState>>,
    mut sim_next_state: ResMut<NextState<SimulationState>>,
    mut menu_next_state: ResMut<NextState<PauseMenuState>>,
    windows: Query<&mut Window>,
    cursor_options: Single<&mut CursorOptions>,
) {
    if keyboard_input.just_pressed(KeyCode::Escape) {
        handle_cursor_lock(windows, cursor_options, &current_sim_state);

        match current_sim_state.get() {
            SimulationState::Running => {
                sim_next_state.set(SimulationState::Paused);
            }
            SimulationState::Paused => match current_menu_state.get() {
                PauseMenuState::Settings => {
                    menu_next_state.set(PauseMenuState::Main);
                }
                PauseMenuState::Main => {
                    sim_next_state.set(SimulationState::Running);
                }
            },
        }
    }
}

pub fn handle_pause_menu(
    interaction_query: Query<
        (&Interaction, &PauseMenuButton),
        (Changed<Interaction>, With<Button>),
    >,
    mut simulation_state: ResMut<NextState<SimulationState>>,
    mut pause_menu_state: ResMut<NextState<PauseMenuState>>,
    mut app_state: ResMut<NextState<AppState>>,
    mut cursor_options: Single<&mut CursorOptions>,
) {
    for (interaction, action) in interaction_query.iter() {
        if *interaction == Interaction::Pressed {
            match action {
                PauseMenuButton::Resume => {
                    simulation_state.set(SimulationState::Running);
                    set_cursor_for_running(&mut cursor_options);
                }
                PauseMenuButton::Settings => {
                    pause_menu_state.set(PauseMenuState::Settings);
                }
                PauseMenuButton::ExitGame => {
                    app_state.set(AppState::MainMenu);
                    simulation_state.set(SimulationState::Running);
                    set_cursor_for_menu(&mut cursor_options);
                }
            }
        }
    }
}

pub fn handle_settings_menu(
    interaction_query: Query<(&Interaction, &SettingsButton), (Changed<Interaction>, With<Button>)>,
    mut pause_menu_state: ResMut<NextState<PauseMenuState>>,
) {
    for (interaction, action) in interaction_query.iter() {
        if *interaction == Interaction::Pressed {
            match action {
                SettingsButton::Back => pause_menu_state.set(PauseMenuState::Main),
            }
        }
    }
}
