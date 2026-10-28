//! Cafe-sim "main menu" — the first screen the player sees inside the
//! cafe-sim project, before any save slot is chosen.
//!
//! Drives the plugin-local [`CafePhase`] state machine:
//!
//! ```text
//!  CafePhase::MainMenu  ──New Game──▶ CafePhase::SlotPicker(NewGame)
//!                       ──Load Game──▶ CafePhase::SlotPicker(LoadGame)
//!                       ──Quit────────▶ AppState::MainMenu (engine project picker)
//! ```
//!
//! All UI here is `ScriptOwned`, so the existing plugin teardown handles
//! cleanup when the player drops back to the engine main menu.

use bevy::{prelude::*, ui::Pressed};
use engine_api::AppState;

use crate::spawn::SpawnOwnedCommands;
use crate::ui::{ButtonVariant, UiExt, UiTheme};

// ----------------------------------------------------------------------------
// Phase state
// ----------------------------------------------------------------------------

/// Top-level state machine the cafe-sim plugin walks through within a
/// single engine `InGame` session. Only one variant is active at a time;
/// each variant gates a disjoint set of systems via `run_if`.
#[derive(Resource, Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum CafePhase {
    #[default]
    MainMenu,
    SlotPicker(SlotPickerMode),
    Playing {
        slot: u8,
    },
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SlotPickerMode {
    NewGame,
    LoadGame,
}

pub fn in_main_menu(phase: Res<CafePhase>) -> bool {
    matches!(*phase, CafePhase::MainMenu)
}

pub fn in_slot_picker(phase: Res<CafePhase>) -> bool {
    matches!(*phase, CafePhase::SlotPicker(_))
}

pub fn in_playing(phase: Res<CafePhase>) -> bool {
    matches!(*phase, CafePhase::Playing { .. })
}

// ----------------------------------------------------------------------------
// UI
// ----------------------------------------------------------------------------

#[derive(Component)]
pub struct MainMenuRoot;

#[derive(Component, Debug)]
pub enum MainMenuButton {
    NewGame,
    LoadGame,
    Quit,
}

pub fn build_main_menu(commands: &mut Commands, theme: &UiTheme) {
    commands
        .spawn_owned((
            MainMenuRoot,
            Node {
                position_type: PositionType::Absolute,
                width: Val::Percent(100.0),
                height: Val::Percent(100.0),
                justify_content: JustifyContent::Center,
                align_items: AlignItems::Center,
                flex_direction: FlexDirection::Column,
                ..default()
            },
            BackgroundColor(theme.bg_overlay),
            GlobalZIndex(150),
        ))
        .with_children(|parent| {
            parent.ui_title("Cafe Sim", theme);
            parent.ui_button(
                "New Game",
                MainMenuButton::NewGame,
                ButtonVariant::Primary,
                theme,
            );
            parent.ui_button(
                "Load Game",
                MainMenuButton::LoadGame,
                ButtonVariant::Primary,
                theme,
            );
            parent.ui_button(
                "Quit to Project Picker",
                MainMenuButton::Quit,
                ButtonVariant::Danger,
                theme,
            );
        });
}

pub fn handle_main_menu_buttons(
    pressed: Query<&MainMenuButton, Added<Pressed>>,
    mut phase: ResMut<CafePhase>,
    mut app_state: ResMut<NextState<AppState>>,
    mut mouse: ResMut<ButtonInput<MouseButton>>,
) {
    for action in &pressed {
        match action {
            MainMenuButton::NewGame => {
                *phase = CafePhase::SlotPicker(SlotPickerMode::NewGame);
            }
            MainMenuButton::LoadGame => {
                *phase = CafePhase::SlotPicker(SlotPickerMode::LoadGame);
            }
            MainMenuButton::Quit => {
                app_state.set(AppState::MainMenu);
            }
        }
        mouse.clear_just_pressed(MouseButton::Left);
    }
}
