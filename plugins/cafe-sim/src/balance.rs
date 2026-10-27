use bevy::prelude::*;

use crate::pause_menu::GamePaused;
use crate::spawn::{SpawnOwnedCommands, WithChildOwned};

const POPUP_LIFETIME_SECS: f32 = 1.2;
const POPUP_START_BOTTOM: f32 = 60.0;
const POPUP_RISE_PX: f32 = 36.0;

const STARTING_BALANCE: i64 = 100;

#[derive(Resource, Debug)]
pub struct Balance(pub i64);

impl Default for Balance {
    fn default() -> Self {
        Self(STARTING_BALANCE)
    }
}

/// The balance as it stood at the start of the current day. Saves only happen
/// between days (the resume model resets the clock to opening hour and clears
/// the floor), so we persist this rather than the live mid-day balance —
/// otherwise reloading would bank that day's in-progress earnings *and* let the
/// player re-earn the whole day. Updated whenever a new day begins.
#[derive(Resource, Debug)]
pub struct DayStartBalance(pub i64);

impl Default for DayStartBalance {
    fn default() -> Self {
        Self(STARTING_BALANCE)
    }
}

#[derive(Component)]
pub struct BalanceText;

pub fn spawn_balance_hud(commands: &mut Commands, balance: &Balance) {
    let bg = Color::srgba(0.06, 0.07, 0.10, 0.85);
    let text_color = Color::srgb(0.98, 0.92, 0.55);

    commands
        .spawn_owned((
            Node {
                position_type: PositionType::Absolute,
                left: Val::Px(16.0),
                bottom: Val::Px(16.0),
                padding: UiRect::axes(Val::Px(14.0), Val::Px(8.0)),
                justify_content: JustifyContent::Center,
                align_items: AlignItems::Center,
                ..default()
            },
            BackgroundColor(bg),
            GlobalZIndex(80),
        ))
        .with_child_owned((
            BalanceText,
            Text::new(format_balance(balance.0)),
            TextFont {
                font_size: FontSize::Px(20.0),
                ..default()
            },
            TextColor(text_color),
        ));
}

pub fn update_balance_text(balance: Res<Balance>, mut text_q: Query<&mut Text, With<BalanceText>>) {
    if !balance.is_changed() {
        return;
    }
    let formatted = format_balance(balance.0);
    for mut text in text_q.iter_mut() {
        **text = formatted.clone();
    }
}

fn format_balance(amount: i64) -> String {
    format!("${amount}")
}

/// Transient floater that rises above the balance HUD and fades out, shown
/// briefly each time the player's balance changes. `rgb` is the base text color
/// (green for gains, red for deductions); `tick_balance_popups` applies the fade
/// alpha on top of it.
#[derive(Component)]
pub struct BalancePopup {
    pub elapsed: f32,
    pub rgb: (f32, f32, f32),
}

const POPUP_GAIN_RGB: (f32, f32, f32) = (0.55, 1.0, 0.55);
const POPUP_LOSS_RGB: (f32, f32, f32) = (1.0, 0.45, 0.45);

/// Green `+$amount` floater shown when the player earns money.
pub fn spawn_balance_popup(commands: &mut Commands, amount: i64) {
    spawn_change_popup(commands, format!("+${amount}"), POPUP_GAIN_RGB);
}

/// Red `-$amount` floater shown when money is deducted (e.g. staff payroll).
pub fn spawn_balance_loss_popup(commands: &mut Commands, amount: i64) {
    spawn_change_popup(commands, format!("-${amount}"), POPUP_LOSS_RGB);
}

fn spawn_change_popup(commands: &mut Commands, label: String, rgb: (f32, f32, f32)) {
    commands
        .spawn_owned((
            BalancePopup { elapsed: 0.0, rgb },
            Node {
                position_type: PositionType::Absolute,
                left: Val::Px(16.0),
                bottom: Val::Px(POPUP_START_BOTTOM),
                padding: UiRect::axes(Val::Px(14.0), Val::Px(4.0)),
                ..default()
            },
            GlobalZIndex(81),
        ))
        .with_child_owned((
            Text::new(label),
            TextFont {
                font_size: FontSize::Px(18.0),
                ..default()
            },
            TextColor(Color::srgba(rgb.0, rgb.1, rgb.2, 1.0)),
        ));
}

pub fn tick_balance_popups(
    mut commands: Commands,
    time: Res<Time>,
    paused: Res<GamePaused>,
    mut popups_q: Query<(Entity, &mut BalancePopup, &mut Node, &Children)>,
    mut text_color_q: Query<&mut TextColor>,
) {
    if paused.0 {
        return;
    }
    let dt = time.delta_secs();
    for (entity, mut popup, mut node, children) in popups_q.iter_mut() {
        popup.elapsed += dt;
        let t = (popup.elapsed / POPUP_LIFETIME_SECS).clamp(0.0, 1.0);
        if t >= 1.0 {
            commands.entity(entity).despawn();
            continue;
        }
        node.bottom = Val::Px(POPUP_START_BOTTOM + POPUP_RISE_PX * t);
        let alpha = 1.0 - t;
        let (r, g, b) = popup.rgb;
        for child in children.iter() {
            if let Ok(mut color) = text_color_q.get_mut(child) {
                *color = TextColor(Color::srgba(r, g, b, alpha));
            }
        }
    }
}
