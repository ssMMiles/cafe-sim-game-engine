use bevy::{
    prelude::*,
    window::{CursorGrabMode, CursorOptions},
};

use crate::game::SimulationState;

pub fn handle_cursor_lock(
    mut windows: Query<&mut Window>,
    mut cursor_options: Single<&mut CursorOptions>,
    current_sim_state: &Res<State<SimulationState>>,
) {
    if let Ok(mut window) = windows.single_mut() {
        match current_sim_state.get() {
            SimulationState::Running => {
                cursor_options.grab_mode = CursorGrabMode::None;
                cursor_options.visible = true;
            }
            SimulationState::Paused => {
                let height = window.height();
                let width = window.width();

                window.set_cursor_position(Some(Vec2::new(width / 2.0, height / 2.0)));

                cursor_options.grab_mode = CursorGrabMode::Locked;
                cursor_options.visible = false;
            }
        }
    }
}

pub fn set_cursor_for_running(cursor_options: &mut CursorOptions) {
    cursor_options.grab_mode = CursorGrabMode::Locked;
    cursor_options.visible = false;
}

pub fn set_cursor_for_menu(cursor_options: &mut CursorOptions) {
    cursor_options.grab_mode = CursorGrabMode::None;
    cursor_options.visible = true;
}
