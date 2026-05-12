//! Cafe open/closed state.
//!
//! The cafe starts closed each day. The player opens it from the shop overview
//! (Tab) panel. At the configured closing hour the cafe auto-closes and the
//! clock skips to the next day's opening hour.

use bevy::prelude::*;

use crate::time::{CLOSING_HOUR, DAY_START_HOUR, GameTime};

#[derive(Resource, Default, Debug, Clone, Copy)]
pub struct CafeOpen(pub bool);

pub fn enforce_closing_time(mut game_time: ResMut<GameTime>, mut cafe_open: ResMut<CafeOpen>) {
    let (hour, _) = game_time.hour_minute();
    if hour >= CLOSING_HOUR || hour < DAY_START_HOUR {
        cafe_open.0 = false;
        game_time.start_next_day();
    }
}
