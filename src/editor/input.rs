//! Shared scroll-wheel handling for the editor.
//!
//! The wheel means different things depending on the held modifier, and four
//! separate systems consume it:
//!
//! | Modifier   | Action |
//! |------------|--------|
//! | none       | cycle the open panel's selection |
//! | Shift      | adjust placement scale |
//! | Alt/Option | adjust placement rotation |
//!
//! Each consumer has its own `MessageReader`, so they all see every event and
//! must agree on which modifier is theirs — otherwise one notch of
//! Alt+Scroll would rotate the preview *and* cycle the catalog. Centralising
//! the modifier test here is what keeps them in agreement.
//!
//! Rotation is on Alt rather than Ctrl because the freecam descends on
//! `ControlLeft` (see `game::gfx::camera`): Ctrl+Scroll in freecam would sink
//! the camera through the floor while rotating.

use bevy::input::mouse::{MouseScrollUnit, MouseWheel};
use bevy::prelude::*;

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

/// Sum this frame's wheel events into a single delta in "notches".
///
/// `pixel_divisor` converts trackpad pixel deltas to notch-equivalents; it
/// differs per call site (the panels were tuned at 40.0, placement at 53.0),
/// so it stays a parameter rather than becoming a constant.
pub fn scroll_delta(events: &mut MessageReader<MouseWheel>, pixel_divisor: f32) -> f32 {
    events
        .read()
        .map(|event| match event.unit {
            MouseScrollUnit::Line => event.y,
            MouseScrollUnit::Pixel => event.y / pixel_divisor,
        })
        .sum()
}
