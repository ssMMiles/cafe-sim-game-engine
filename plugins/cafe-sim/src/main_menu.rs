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
//! New Game and Load Game wait for the level's cafe shell (see
//! [`crate::cafe_scene`]): a status line under the title says whether it has
//! been found, and the buttons do nothing until it has.
//!
//! All UI here is `ScriptOwned`, so the existing plugin teardown handles
//! cleanup when the player drops back to the engine main menu.

use bevy::{prelude::*, ui::Pressed};
use engine_api::AppState;

use crate::grid::CafeAnchor;
use crate::spawn::{SpawnOwnedCommands, WithChildOwned};
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

/// The "cafe shell found / missing" line under the title.
#[derive(Component)]
pub struct ShellStatusLabel;

const SHELL_FOUND: &str = "Cafe shell found";
const SHELL_LOADING: &str = "Loading the cafe shell…";
const SHELL_MISSING: &str =
    "No cafe shell in this level — place the cafe-shell prefab in the editor (F1) and save";

#[derive(Component, Debug)]
pub enum MainMenuButton {
    NewGame,
    LoadGame,
    Quit,
}

pub fn build_main_menu(commands: &mut Commands, theme: &UiTheme, anchor: &CafeAnchor) {
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
            let (text, color) = shell_status(anchor, theme);
            parent
                .spawn_owned((
                    ShellStatusLabel,
                    Node {
                        margin: UiRect::bottom(Val::Px(theme.pad_lg)),
                        max_width: Val::Px(520.0),
                        ..default()
                    },
                ))
                .with_child_owned((
                    Text::new(text),
                    TextFont {
                        font_size: FontSize::Px(theme.size_label),
                        ..default()
                    },
                    TextColor(color),
                    TextLayout::justify(Justify::Center),
                ));
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

fn shell_status(anchor: &CafeAnchor, theme: &UiTheme) -> (&'static str, Color) {
    match (anchor.is_resolved(), anchor.is_loading()) {
        (true, _) => (SHELL_FOUND, theme.text_dim),
        // Its glTF is still streaming; the grid's size isn't known yet.
        (_, true) => (SHELL_LOADING, theme.text_dim),
        _ => (SHELL_MISSING, theme.danger_hover),
    }
}

/// Keep the status line in step with the anchor — the shell can stream in
/// (or be lost) while the menu is up.
pub fn sync_shell_status(
    anchor: Res<CafeAnchor>,
    theme: Res<UiTheme>,
    labels: Query<&Children, With<ShellStatusLabel>>,
    mut texts: Query<(&mut Text, &mut TextColor)>,
) {
    if !anchor.is_changed() {
        return;
    }
    let (text, color) = shell_status(&anchor, &theme);
    for children in &labels {
        for child in children.iter() {
            if let Ok((mut t, mut c)) = texts.get_mut(child) {
                t.0 = text.to_owned();
                c.0 = color;
            }
        }
    }
}

pub fn handle_main_menu_buttons(
    pressed: Query<&MainMenuButton, Added<Pressed>>,
    anchor: Res<CafeAnchor>,
    mut phase: ResMut<CafePhase>,
    mut app_state: ResMut<NextState<AppState>>,
    mut mouse: ResMut<ButtonInput<MouseButton>>,
) {
    for action in &pressed {
        match action {
            // Both paths end in `apply_save`/`apply_new_game` against the
            // anchor, so neither may start without a shell; the status line
            // already says why.
            MainMenuButton::NewGame | MainMenuButton::LoadGame if !anchor.is_resolved() => {
                warn!("[cafe-sim] cannot start: no cafe shell in this level");
            }
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
