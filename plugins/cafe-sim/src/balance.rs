use bevy::prelude::*;

use crate::spawn::SpawnOwnedWorld;

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

pub fn update_balance_text(
    balance: Res<Balance>,
    mut text_q: Query<&mut Text, With<BalanceText>>,
) {
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
