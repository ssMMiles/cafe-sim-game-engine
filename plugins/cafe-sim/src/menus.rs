//! Gameplay menu state.
//!
//! At most one gameplay menu may be open at a time (build/shop, shop overview,
//! brew menu). Mutual exclusion is implicit: opening a new menu just sets
//! [`ActiveMenu`] to that variant, which automatically retires whichever menu
//! was previously active. Each menu's UI builder, plus any sub-state cleanup,
//! hangs off `ActiveMenu` change-detection.
//!
//! The pause menu lives in [`GamePaused`] because it also freezes time and is
//! broader than a menu. Pausing closes whatever gameplay menu is active.

use bevy::prelude::*;

use crate::pause_menu::GamePaused;

#[derive(Resource, Default, Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActiveMenu {
    #[default]
    None,
    Shop,
    ShopOverview,
    Coffee,
    HireStaff,
}

impl ActiveMenu {
    pub fn is(self, other: ActiveMenu) -> bool {
        self == other
    }

    pub fn is_open(self) -> bool {
        !self.is(ActiveMenu::None)
    }

    pub fn close(&mut self) {
        *self = ActiveMenu::None;
    }

    /// Open `m`, or close if it's already the active menu.
    pub fn toggle(&mut self, m: ActiveMenu) {
        *self = if *self == m { ActiveMenu::None } else { m };
    }
}

/// Pause is sticky: when it flips on, the active gameplay menu closes.
pub fn close_menus_when_paused(paused: Res<GamePaused>, mut active: ResMut<ActiveMenu>) {
    if paused.is_changed() && paused.0 && active.is_open() {
        active.close();
    }
}
