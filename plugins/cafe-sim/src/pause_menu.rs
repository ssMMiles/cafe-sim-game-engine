//! Cafe-sim gameplay pause menu.
//!
//! Owned entirely by this plugin and driven by the plugin-local [`GamePaused`]
//! resource. Pressing Escape flips `GamePaused`; flipping it spawns or
//! despawns the menu overlay. Resume clears the flag, Exit Game clears the
//! flag and asks the engine to drop back to the main menu.
//!
//! Independent of the engine's own pause UX (`SimulationState` /
//! `PauseMenuState`), which is editor-only.

use bevy::prelude::*;
use engine_api::AppState;

use crate::spawn::SpawnOwnedCommands;
use crate::ui::{ButtonVariant, UiExt, UiTheme};

/// Plugin-local pause flag for gameplay. Other cafe-sim systems read this to
/// freeze time, customer spawning, etc. while the pause menu is up.
#[derive(Resource, Default, Debug, Clone, Copy)]
pub struct GamePaused(pub bool);

#[derive(Component)]
pub struct PauseMenuRoot;

#[derive(Component)]
pub enum PauseMenuButton {
    Resume,
    Exit,
}

/// Esc flips `GamePaused`. Always-on while the plugin is loaded — the engine
/// no longer listens to Escape in gameplay mode (`EditorMode::Gameplay`).
pub fn toggle_pause_on_escape(input: Res<ButtonInput<KeyCode>>, mut paused: ResMut<GamePaused>) {
    if input.just_pressed(KeyCode::Escape) {
        paused.0 = !paused.0;
    }
}

/// Mirror `GamePaused` into the UI: build the overlay when it goes true,
/// despawn the existing one when it goes false (or rebuild on hot-reload).
pub fn sync_pause_menu_ui(
    mut commands: Commands,
    paused: Res<GamePaused>,
    theme: Res<UiTheme>,
    existing: Query<Entity, With<PauseMenuRoot>>,
) {
    if !paused.is_changed() {
        return;
    }

    for e in &existing {
        commands.entity(e).despawn();
    }

    if !paused.0 {
        return;
    }

    commands
        .spawn_owned((
            PauseMenuRoot,
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
            GlobalZIndex(200),
        ))
        .with_children(|parent| {
            parent.ui_title("PAUSED", &theme);
            parent.ui_button(
                "Resume",
                PauseMenuButton::Resume,
                ButtonVariant::Primary,
                &theme,
            );
            parent.ui_button(
                "Exit Game",
                PauseMenuButton::Exit,
                ButtonVariant::Danger,
                &theme,
            );
        });
}

pub fn handle_pause_buttons(
    interactions: Query<(&Interaction, &PauseMenuButton), Changed<Interaction>>,
    mut paused: ResMut<GamePaused>,
    mut app_state: ResMut<NextState<AppState>>,
) {
    for (interaction, action) in &interactions {
        if *interaction != Interaction::Pressed {
            continue;
        }
        match action {
            PauseMenuButton::Resume => {
                paused.0 = false;
            }
            PauseMenuButton::Exit => {
                paused.0 = false;
                app_state.set(AppState::MainMenu);
            }
        }
    }
}
