use bevy::prelude::*;
use bevy::window::{CursorGrabMode, CursorOptions};
use engine_api::InputLock;

use crate::PLUGIN_NAME;
use crate::pause_menu::GamePaused;
use crate::shop::ShopOpen;
use crate::shop_overview::ShopOverviewOpen;

pub fn manage_cursor(
    shop: Res<ShopOpen>,
    overview: Res<ShopOverviewOpen>,
    paused: Res<GamePaused>,
    cursor_options: Option<Single<&mut CursorOptions>>,
    input_lock: Option<ResMut<InputLock>>,
) {
    if !shop.is_changed() && !overview.is_changed() && !paused.is_changed() {
        return;
    }

    let any_open = shop.0 || overview.0 || paused.0;

    if let Some(co) = cursor_options {
        let mut co = co.into_inner();
        if any_open {
            co.grab_mode = CursorGrabMode::None;
            co.visible = true;
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
