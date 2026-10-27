use bevy::prelude::*;

use crate::pause_menu::GamePaused;
use crate::spawn::SpawnOwnedCommands;

/// Real seconds → in-game seconds. One real minute equals one in-game hour
/// (60 real seconds → 3600 in-game seconds).
const TIME_SCALE: f32 = 60.0;
pub const DAY_START_HOUR: u32 = 7;
pub const CLOSING_HOUR: u32 = 19;
const SECONDS_PER_DAY: f32 = 24.0 * 3600.0;

#[derive(Resource, Debug, Clone, Copy)]
pub struct GameTime {
    pub day: u32,
    /// Seconds since midnight on the current day, in in-game time.
    pub seconds: f32,
}

impl Default for GameTime {
    fn default() -> Self {
        Self {
            day: 1,
            seconds: DAY_START_HOUR as f32 * 3600.0,
        }
    }
}

impl GameTime {
    pub fn hour_minute(&self) -> (u32, u32) {
        let total_minutes = (self.seconds / 60.0).floor() as u32;
        let hour = (total_minutes / 60) % 24;
        let minute = total_minutes % 60;
        (hour, minute)
    }

    /// Skip to the opening hour of the following day.
    pub fn start_next_day(&mut self) {
        self.day += 1;
        self.seconds = DAY_START_HOUR as f32 * 3600.0;
    }

    pub fn formatted(&self) -> String {
        let (h, m) = self.hour_minute();
        let suffix = if h < 12 { "AM" } else { "PM" };
        let display_hour = match h % 12 {
            0 => 12,
            v => v,
        };
        format!("{display_hour}:{m:02} {suffix}")
    }
}

#[derive(Component)]
pub struct TimeClockText;

#[derive(Component)]
pub struct TimeDayText;

pub fn spawn_time_hud(commands: &mut Commands, initial: &GameTime) {
    let bg = Color::srgba(0.06, 0.07, 0.10, 0.85);
    let day_color = Color::srgb(0.65, 0.70, 0.78);
    let clock_color = Color::srgb(0.98, 0.92, 0.55);

    let root = commands
        .spawn_owned((
            Node {
                position_type: PositionType::Absolute,
                left: Val::Px(16.0),
                top: Val::Px(16.0),
                padding: UiRect::axes(Val::Px(14.0), Val::Px(8.0)),
                flex_direction: FlexDirection::Column,
                row_gap: Val::Px(2.0),
                align_items: AlignItems::FlexStart,
                ..default()
            },
            BackgroundColor(bg),
            GlobalZIndex(80),
        ))
        .id();

    let day = commands
        .spawn_owned((
            TimeDayText,
            Text::new(format!("Day {}", initial.day)),
            TextFont {
                font_size: FontSize::Px(12.0),
                ..default()
            },
            TextColor(day_color),
        ))
        .id();

    let clock = commands
        .spawn_owned((
            TimeClockText,
            Text::new(initial.formatted()),
            TextFont {
                font_size: FontSize::Px(20.0),
                ..default()
            },
            TextColor(clock_color),
        ))
        .id();

    commands.entity(root).add_child(day);
    commands.entity(root).add_child(clock);
}

pub fn advance_game_time(
    time: Res<Time>,
    paused: Res<GamePaused>,
    mut game_time: ResMut<GameTime>,
) {
    if paused.0 {
        return;
    }
    game_time.seconds += time.delta_secs() * TIME_SCALE;
    while game_time.seconds >= SECONDS_PER_DAY {
        game_time.seconds -= SECONDS_PER_DAY;
        game_time.day += 1;
    }
}

pub fn update_time_text(
    game_time: Res<GameTime>,
    mut last: Local<Option<(u32, u32, u32)>>,
    mut clock_q: Query<&mut Text, (With<TimeClockText>, Without<TimeDayText>)>,
    mut day_q: Query<&mut Text, (With<TimeDayText>, Without<TimeClockText>)>,
) {
    let (h, m) = game_time.hour_minute();
    let current = (game_time.day, h, m);
    if *last == Some(current) {
        return;
    }
    *last = Some(current);

    for mut text in clock_q.iter_mut() {
        **text = game_time.formatted();
    }
    for mut text in day_q.iter_mut() {
        **text = format!("Day {}", game_time.day);
    }
}
