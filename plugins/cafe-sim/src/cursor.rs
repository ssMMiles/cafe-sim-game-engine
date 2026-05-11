use bevy::prelude::*;
use bevy::window::{CursorGrabMode, CursorOptions};
use engine_api::InputLock;

use crate::PLUGIN_NAME;
use crate::shop::ShopOpen;

pub fn manage_cursor(
    shop: Res<ShopOpen>,
    cursor_options: Option<Single<&mut CursorOptions>>,
    input_lock: Option<ResMut<InputLock>>,
) {
    if !shop.is_changed() {
        return;
    }

    if let Some(co) = cursor_options {
        let mut co = co.into_inner();
        if shop.0 {
            co.grab_mode = CursorGrabMode::None;
            co.visible = true;
        } else {
            co.grab_mode = CursorGrabMode::Locked;
            co.visible = false;
        }
    }

    if let Some(mut lock) = input_lock {
        if shop.0 {
            lock.lock(PLUGIN_NAME);
        } else {
            lock.unlock(PLUGIN_NAME);
        }
    }
}
