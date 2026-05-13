use bevy::prelude::*;

use crate::catalog::CATALOG;
use crate::menus::ActiveMenu;
use crate::pause_menu::GamePaused;
use crate::placement::PlacementSelection;
use crate::spawn::{SpawnOwnedCommands, WithChildOwned};
use crate::thumbnails::PieceThumbnails;

const CARD_W: f32 = 140.0;
const CARD_H: f32 = 140.0;
const CARD_GAP: f32 = 8.0;
const CATEGORY_GAP: f32 = 16.0;

#[derive(Component)]
pub struct ShopRoot;

#[derive(Component)]
pub struct PieceCard {
    pub cat: usize,
    pub piece: usize,
}

pub fn toggle_shop(
    input: Res<ButtonInput<KeyCode>>,
    paused: Res<GamePaused>,
    mut active: ResMut<ActiveMenu>,
    mut selection: ResMut<PlacementSelection>,
) {
    if paused.0 {
        return;
    }
    let shift = input.pressed(KeyCode::ShiftLeft) || input.pressed(KeyCode::ShiftRight);
    if !shift && input.just_pressed(KeyCode::KeyB) {
        active.toggle(ActiveMenu::Shop);
        if active.is(ActiveMenu::Shop) {
            selection.0 = None;
        }
    }
}

pub fn sync_shop_ui(
    mut commands: Commands,
    active: Res<ActiveMenu>,
    existing: Query<Entity, With<ShopRoot>>,
    thumbnails: Res<PieceThumbnails>,
) {
    if !active.is_changed() {
        return;
    }

    for e in &existing {
        commands.entity(e).despawn();
    }

    if !active.is(ActiveMenu::Shop) {
        return;
    }

    let panel_bg = Color::srgba(0.06, 0.07, 0.10, 0.92);
    let header_bg = Color::srgba(0.10, 0.12, 0.16, 1.0);
    let card_bg = Color::srgba(0.18, 0.22, 0.28, 1.0);
    let text_color = Color::srgb(0.95, 0.95, 0.95);

    let root = commands
        .spawn_owned((
            ShopRoot,
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
        .spawn_owned((
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
        .spawn_owned((
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
            .spawn_owned((Node {
                flex_direction: FlexDirection::Column,
                margin: UiRect::bottom(Val::Px(CATEGORY_GAP)),
                ..default()
            },))
            .id();
        commands.entity(scroll).add_child(section);

        let header = commands
            .spawn_owned((
                Node {
                    padding: UiRect::axes(Val::Px(8.0), Val::Px(4.0)),
                    margin: UiRect::bottom(Val::Px(4.0)),
                    ..default()
                },
                BackgroundColor(header_bg),
            ))
            .with_child_owned((
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
            .spawn_owned((
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
                .spawn_owned((
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
                        overflow: Overflow::clip(),
                        ..default()
                    },
                    BackgroundColor(card_bg),
                ))
                .id();
            commands.entity(row).add_child(card);

            let mut swatch_cmd = commands.spawn_owned((Node {
                width: Val::Percent(100.0),
                flex_grow: 1.0,
                flex_shrink: 1.0,
                min_height: Val::Px(0.0),
                overflow: Overflow::clip(),
                ..default()
            },));
            if let Some(thumb) = thumbnails.0.get(piece.name) {
                swatch_cmd.insert(ImageNode {
                    image: thumb.clone(),
                    image_mode: NodeImageMode::Stretch,
                    ..default()
                });
            } else {
                swatch_cmd.insert(BackgroundColor(piece.color()));
            }
            let swatch = swatch_cmd.id();
            commands.entity(card).add_child(swatch);

            let label = commands
                .spawn_owned((Node {
                    width: Val::Percent(100.0),
                    flex_shrink: 0.0,
                    padding: UiRect::all(Val::Px(8.0)),
                    justify_content: JustifyContent::Center,
                    align_items: AlignItems::Center,
                    ..default()
                },))
                .with_child_owned((
                    Text::new(piece.name),
                    TextFont {
                        font_size: 14.0,
                        ..default()
                    },
                    TextColor(text_color),
                ))
                .id();
            commands.entity(card).add_child(label);
        }
    }
}

pub fn handle_card_click(
    interaction_q: Query<(&Interaction, &PieceCard), (Changed<Interaction>, With<Button>)>,
    mut active: ResMut<ActiveMenu>,
    mut selection: ResMut<PlacementSelection>,
    mut mouse: ResMut<ButtonInput<MouseButton>>,
) {
    for (interaction, card) in interaction_q.iter() {
        if *interaction == Interaction::Pressed {
            let piece = &CATALOG[card.cat].pieces[card.piece];
            selection.0 = Some(piece);
            active.close();
            mouse.clear_just_pressed(MouseButton::Left);
        }
    }
}
