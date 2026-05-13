use bevy::prelude::*;
use bevy::window::{CursorGrabMode, CursorOptions, WindowFocused};
use engine_api::InputLock;

use crate::PLUGIN_NAME;
use crate::main_menu::CafePhase;
use crate::menus::ActiveMenu;
use crate::pause_menu::GamePaused;

pub fn manage_cursor(
    active: Res<ActiveMenu>,
    paused: Res<GamePaused>,
    phase: Res<CafePhase>,
    mut focus_events: MessageReader<WindowFocused>,
    mut windows: Query<&mut Window>,
    cursor_options: Option<Single<&mut CursorOptions>>,
    input_lock: Option<ResMut<InputLock>>,
) {
    let focus_regained = focus_events.read().any(|e| e.focused);
    if !active.is_changed() && !paused.is_changed() && !phase.is_changed() && !focus_regained {
        return;
    }

    // The main menu / slot picker need a free cursor for clicking buttons;
    // gameplay menus (shop, pause) already free it via `active`/`paused`.
    let in_menu_phase = !matches!(*phase, CafePhase::Playing { .. });
    let any_open = active.is_open() || paused.0 || in_menu_phase;

    if let Some(mut co) = cursor_options {
        if any_open {
            co.grab_mode = CursorGrabMode::None;
            co.visible = true;
            // macOS doesn't support Confined, so if focus came back with the
            // cursor outside the window, warp it to the center.
            if focus_regained
                && let Ok(mut win) = windows.single_mut()
                && win.cursor_position().is_none()
            {
                let center = Vec2::new(win.width() / 2.0, win.height() / 2.0);
                win.set_cursor_position(Some(center));
            }
        } else {
            co.grab_mode = CursorGrabMode::Locked;
            co.visible = false;
        }
    }

    if let Some(mut lock) = input_lock {
        if any_open {
            lock.lock(PLUGIN_NAME);
        } else {
            lock.unlock(PLUGIN_NAME);
        }
    }
}
