use bevy::prelude::*;

use crate::pause_menu::GamePaused;
use crate::spawn::{SpawnOwnedCommands, SpawnOwnedWorld, WithChildOwned};

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

#[derive(Component)]
pub struct BalanceText;

pub fn spawn_balance_hud(world: &mut World) {
    let bg = Color::srgba(0.06, 0.07, 0.10, 0.85);
    let text_color = Color::srgb(0.98, 0.92, 0.55);

    let root = world
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
        .id();

    let text = world
        .spawn_owned((
            BalanceText,
            Text::new(format_balance(STARTING_BALANCE)),
            TextFont {
                font_size: 20.0,
                ..default()
            },
            TextColor(text_color),
        ))
        .id();

    world.entity_mut(root).add_child(text);
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
/// briefly each time the player earns money.
#[derive(Component)]
pub struct BalancePopup {
    pub elapsed: f32,
}

pub fn spawn_balance_popup(commands: &mut Commands, amount: i64) {
    commands
        .spawn_owned((
            BalancePopup { elapsed: 0.0 },
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
            Text::new(format!("+${amount}")),
            TextFont {
                font_size: 18.0,
                ..default()
            },
            TextColor(Color::srgba(0.55, 1.0, 0.55, 1.0)),
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
        for child in children.iter() {
            if let Ok(mut color) = text_color_q.get_mut(child) {
                *color = TextColor(Color::srgba(0.55, 1.0, 0.55, alpha));
            }
        }
    }
}
