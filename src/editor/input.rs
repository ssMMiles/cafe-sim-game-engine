//! Shared input arbitration for the editor.
//!
//! Two questions every editor binding has to answer before acting:
//!
//! 1. **Does egui want the keyboard?** If a text field has focus, `R`, `K`,
//!    `Delete`, `Shift+S`… must not fire. [`keyboard_free`] is the run
//!    condition for every key binding (F1 and Escape excepted).
//! 2. **Is the pointer on the viewport?** bevy_egui's own "wants pointer"
//!    helpers are useless once the whole window is a dock — the pointer is
//!    always over *some* egui area — so mouse buttons and the wheel are gated
//!    on the viewport tab's own hover state instead
//!    (`picking::viewport_pointer_free`).
//!
//! The wheel also means different things depending on the held modifier:
//!
//! | Modifier   | Action |
//! |------------|--------|
//! | none       | cycle the open strip's selection, else dolly the camera |
//! | Shift      | adjust placement scale |
//! | Alt/Option | adjust placement rotation |
//!
//! Every consumer reads the same [`ViewportInput::scroll`] value, so they
//! must agree on which modifier is theirs — centralising the modifier test
//! here is what keeps them in agreement. Rotation is on Alt rather than Ctrl
//! because Ctrl+LMB lowers terrain.

use bevy::prelude::*;
use bevy_egui::input::EguiWantsInput;

use super::picking::ViewportInput;

/// egui points per wheel notch. egui expands a line-unit wheel event by its
/// `line_scroll_speed`; this undoes that so consumers think in notches again.
const POINTS_PER_NOTCH: f32 = 50.0;

#[derive(PartialEq, Eq, Clone, Copy, Debug)]
pub enum ScrollMod {
    None,
    Shift,
    Alt,
}

/// Which modifier is held this frame. Shift wins if both are down, matching
/// the pre-existing behaviour where Shift+Scroll meant "scale".
pub fn scroll_modifier(input: &ButtonInput<KeyCode>) -> ScrollMod {
    if input.pressed(KeyCode::ShiftLeft) || input.pressed(KeyCode::ShiftRight) {
        ScrollMod::Shift
    } else if input.pressed(KeyCode::AltLeft) || input.pressed(KeyCode::AltRight) {
        ScrollMod::Alt
    } else {
        ScrollMod::None
    }
}

/// This frame's wheel movement over the viewport, in notches (positive = up).
pub fn scroll_notches(viewport: &ViewportInput) -> f32 {
    viewport.scroll.y / POINTS_PER_NOTCH
}

/// Run condition: no egui text field is taking keystrokes.
pub fn keyboard_free(wants: Option<Res<EguiWantsInput>>) -> bool {
    wants.is_none_or(|w| !w.wants_any_keyboard_input())
}
