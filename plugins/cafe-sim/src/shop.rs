use bevy::prelude::*;
use engine_api::ScriptOwned;

use crate::PLUGIN_NAME;
use crate::catalog::CATALOG;
use crate::placement::PlacementSelection;

const CARD_W: f32 = 140.0;
const CARD_H: f32 = 140.0;
const CARD_GAP: f32 = 8.0;
const CATEGORY_GAP: f32 = 16.0;

#[derive(Resource, Default)]
pub struct ShopOpen(pub bool);

#[derive(Component)]
pub struct ShopRoot;

#[derive(Component)]
pub struct PieceCard {
    pub cat: usize,
    pub piece: usize,
}

pub fn toggle_shop(
    input: Res<ButtonInput<KeyCode>>,
    mut shop: ResMut<ShopOpen>,
    mut selection: ResMut<PlacementSelection>,
) {
    if input.just_pressed(KeyCode::Tab) {
        shop.0 = !shop.0;
        if shop.0 {
            selection.0 = None;
        }
    }
}

pub fn build_shop_ui(
    mut commands: Commands,
    shop: Res<ShopOpen>,
    existing: Query<(), With<ShopRoot>>,
) {
    if !shop.is_changed() || !shop.0 || !existing.is_empty() {
        return;
    }

    let panel_bg = Color::srgba(0.06, 0.07, 0.10, 0.92);
    let header_bg = Color::srgba(0.10, 0.12, 0.16, 1.0);
    let card_bg = Color::srgba(0.18, 0.22, 0.28, 1.0);
    let text_color = Color::srgb(0.95, 0.95, 0.95);

    let root = commands
        .spawn((
            ShopRoot,
            ScriptOwned::new(PLUGIN_NAME),
            Node {
                position_type: PositionType::Absolute,
                left: Val::Percent(10.0),
                right: Val::Percent(10.0),
                top: Val::Percent(10.0),
                bottom: Val::Percent(10.0),
                padding: UiRect::all(Val::Px(16.0)),
                flex_direction: FlexDirection::Column,
                overflow: Overflow::clip(),
                ..default()
            },
            BackgroundColor(panel_bg),
            GlobalZIndex(100),
        ))
        .id();

    let title = commands
        .spawn((
            ScriptOwned::new(PLUGIN_NAME),
            Text::new("Shop"),
            TextFont {
                font_size: 28.0,
                ..default()
            },
            TextColor(text_color),
            Node {
                margin: UiRect::bottom(Val::Px(12.0)),
                ..default()
            },
        ))
        .id();
    commands.entity(root).add_child(title);

    let scroll = commands
        .spawn((
            ScriptOwned::new(PLUGIN_NAME),
            Node {
                flex_direction: FlexDirection::Column,
                width: Val::Percent(100.0),
                flex_grow: 1.0,
                overflow: Overflow::clip(),
                ..default()
            },
            ScrollPosition::default(),
        ))
        .id();
    commands.entity(root).add_child(scroll);

    for (cat_idx, category) in CATALOG.iter().enumerate() {
        let section = commands
            .spawn((
                ScriptOwned::new(PLUGIN_NAME),
                Node {
                    flex_direction: FlexDirection::Column,
                    margin: UiRect::bottom(Val::Px(CATEGORY_GAP)),
                    ..default()
                },
            ))
            .id();
        commands.entity(scroll).add_child(section);

        let header = commands
            .spawn((
                ScriptOwned::new(PLUGIN_NAME),
                Node {
                    padding: UiRect::axes(Val::Px(8.0), Val::Px(4.0)),
                    margin: UiRect::bottom(Val::Px(4.0)),
                    ..default()
                },
                BackgroundColor(header_bg),
            ))
            .with_child((
                ScriptOwned::new(PLUGIN_NAME),
                Text::new(category.name),
                TextFont {
                    font_size: 18.0,
                    ..default()
                },
                TextColor(text_color),
            ))
            .id();
        commands.entity(section).add_child(header);

        let row = commands
            .spawn((
                ScriptOwned::new(PLUGIN_NAME),
                Node {
                    flex_direction: FlexDirection::Row,
                    height: Val::Px(CARD_H),
                    overflow: Overflow::clip(),
                    column_gap: Val::Px(CARD_GAP),
                    padding: UiRect::all(Val::Px(4.0)),
                    ..default()
                },
                ScrollPosition::default(),
            ))
            .id();
        commands.entity(section).add_child(row);

        for (piece_idx, piece) in category.pieces.iter().enumerate() {
            let card = commands
                .spawn((
                    ScriptOwned::new(PLUGIN_NAME),
                    PieceCard {
                        cat: cat_idx,
                        piece: piece_idx,
                    },
                    Button,
                    Node {
                        width: Val::Px(CARD_W),
                        height: Val::Px(CARD_H),
                        flex_shrink: 0.0,
                        flex_direction: FlexDirection::Column,
                        align_items: AlignItems::Center,
                        justify_content: JustifyContent::FlexEnd,
                        padding: UiRect::all(Val::Px(8.0)),
                        ..default()
                    },
                    BackgroundColor(card_bg),
                ))
                .with_child((
                    ScriptOwned::new(PLUGIN_NAME),
                    Text::new(piece.name),
                    TextFont {
                        font_size: 14.0,
                        ..default()
                    },
                    TextColor(text_color),
                ))
                .id();
            commands.entity(row).add_child(card);
        }
    }
}

pub fn teardown_shop_ui(
    mut commands: Commands,
    shop: Res<ShopOpen>,
    panel_q: Query<Entity, With<ShopRoot>>,
) {
    if !shop.is_changed() || shop.0 {
        return;
    }
    for e in panel_q.iter() {
        commands.entity(e).despawn();
    }
}

pub fn handle_card_click(
    interaction_q: Query<(&Interaction, &PieceCard), (Changed<Interaction>, With<Button>)>,
    mut shop: ResMut<ShopOpen>,
    mut selection: ResMut<PlacementSelection>,
    mut mouse: ResMut<ButtonInput<MouseButton>>,
) {
    for (interaction, card) in interaction_q.iter() {
        if *interaction == Interaction::Pressed {
            let piece = &CATALOG[card.cat].pieces[card.piece];
            selection.0 = Some(piece);
            shop.0 = false;
            mouse.clear_just_pressed(MouseButton::Left);
        }
    }
}
