//! Save-slot picker. Reached from the cafe-sim main menu (New Game or
//! Load Game) and from there back to [`CafePhase::MainMenu`] or forward
//! into [`CafePhase::Playing`].

use bevy::prelude::*;

use crate::balance::Balance;
use crate::cafe::CafeOpen;
use crate::main_menu::{CafePhase, SlotPickerMode};
use crate::placement::GridOccupancy;
use crate::saves::{self, SLOT_COUNT, SlotMetadataCache, SlotStatus};
use crate::shop_overview::ShopState;
use crate::spawn::{SpawnOwnedCommands, WithChildOwned};
use crate::time::GameTime;
use crate::ui::{ButtonVariant, InteractiveStyle, UiExt, UiTheme};

// ----------------------------------------------------------------------------
// Components
// ----------------------------------------------------------------------------

#[derive(Component)]
pub struct SlotPickerRoot;

#[derive(Component, Clone, Copy)]
pub struct SlotButton(pub u8);

#[derive(Component)]
pub struct SlotBackButton;

// ----------------------------------------------------------------------------
// Systems
// ----------------------------------------------------------------------------

/// Build the picker. Re-probes every slot from disk so the cards always
/// reflect current state — cheap because there are only 4 files.
pub fn build_slot_picker(
    commands: &mut Commands,
    theme: &UiTheme,
    mode: SlotPickerMode,
    cache: &mut SlotMetadataCache,
) {
    cache.0.clear();
    for slot in 1..=SLOT_COUNT {
        cache.0.insert(slot, saves::probe_slot(slot));
    }

    let title = match mode {
        SlotPickerMode::NewGame => "Choose a Slot - New Game",
        SlotPickerMode::LoadGame => "Choose a Slot - Load Game",
    };

    commands
        .spawn_owned((
            SlotPickerRoot,
            Node {
                position_type: PositionType::Absolute,
                width: Val::Percent(100.0),
                height: Val::Percent(100.0),
                justify_content: JustifyContent::Center,
                align_items: AlignItems::Center,
                flex_direction: FlexDirection::Column,
                row_gap: Val::Px(theme.pad_md),
                ..default()
            },
            BackgroundColor(theme.bg_overlay),
            GlobalZIndex(150),
        ))
        .with_children(|parent| {
            parent.ui_title(title, theme);

            for slot in 1..=SLOT_COUNT {
                let status = cache.0.get(&slot).cloned().unwrap_or(SlotStatus::Empty);
                spawn_slot_card(parent, slot, &status, mode, theme);
            }

            parent.ui_button("Back", SlotBackButton, ButtonVariant::Danger, theme);
        });
}

fn spawn_slot_card(
    parent: &mut bevy::ecs::relationship::RelatedSpawnerCommands<ChildOf>,
    slot: u8,
    status: &SlotStatus,
    mode: SlotPickerMode,
    theme: &UiTheme,
) {
    // In LoadGame mode, only occupied slots are clickable. NewGame is
    // willing to overwrite anything (we still show what's there so the
    // player can pick an empty slot to avoid clobbering).
    let interactable = match (mode, status) {
        (SlotPickerMode::LoadGame, SlotStatus::Occupied(_)) => true,
        (SlotPickerMode::LoadGame, _) => false,
        (SlotPickerMode::NewGame, _) => true,
    };

    let bg = if interactable {
        theme.bg_panel
    } else {
        theme.bg_surface
    };
    let text_color = if interactable {
        theme.text
    } else {
        theme.text_dim
    };

    let mut row = parent.spawn_owned((
        SlotButton(slot),
        Button,
        Node {
            width: Val::Px(420.0),
            height: Val::Px(74.0),
            padding: UiRect::axes(Val::Px(16.0), Val::Px(8.0)),
            margin: UiRect::all(Val::Px(theme.pad_sm)),
            border: UiRect::all(Val::Px(theme.btn_border)),
            border_radius: BorderRadius::all(Val::Px(6.0)),
            flex_direction: FlexDirection::Row,
            align_items: AlignItems::Center,
            justify_content: JustifyContent::SpaceBetween,
            column_gap: Val::Px(theme.pad_md),
            ..default()
        },
        BackgroundColor(bg),
        BorderColor::all(theme.border),
    ));
    if interactable {
        row.insert(InteractiveStyle::button(theme, ButtonVariant::Primary));
    }
    row.with_child_owned((
        Text::new(format!("Slot {slot}")),
        TextFont {
            font_size: theme.size_body,
            ..default()
        },
        TextColor(text_color),
    ));

    let (right_primary, right_secondary) = match status {
        SlotStatus::Occupied(info) => (
            info.shop_name.clone(),
            format!("Day {}    ${}", info.day, info.balance),
        ),
        SlotStatus::Empty => ("Empty".to_string(), String::new()),
        SlotStatus::Incompatible => ("Incompatible save".to_string(), String::new()),
    };

    row.with_children(|card| {
        card.spawn_owned((Node {
            flex_direction: FlexDirection::Column,
            align_items: AlignItems::FlexEnd,
            row_gap: Val::Px(2.0),
            ..default()
        },))
            .with_children(|col| {
                col.spawn_owned((
                    Text::new(right_primary),
                    TextFont {
                        font_size: theme.size_body,
                        ..default()
                    },
                    TextColor(text_color),
                ));
                if !right_secondary.is_empty() {
                    col.spawn_owned((
                        Text::new(right_secondary),
                        TextFont {
                            font_size: theme.size_label,
                            ..default()
                        },
                        TextColor(theme.text_dim),
                    ));
                }
            });
    });
}

pub fn handle_slot_picker_buttons(
    slot_interactions: Query<(&Interaction, &SlotButton), Changed<Interaction>>,
    back_interactions: Query<&Interaction, (Changed<Interaction>, With<SlotBackButton>)>,
    cache: Res<SlotMetadataCache>,
    mut phase: ResMut<CafePhase>,
    mut mouse: ResMut<ButtonInput<MouseButton>>,
    mut commands: Commands,
    asset_server: Res<AssetServer>,
    mut occupancy: ResMut<GridOccupancy>,
    mut shop: ResMut<ShopState>,
    mut balance: ResMut<Balance>,
    mut time: ResMut<GameTime>,
    mut cafe_open: ResMut<CafeOpen>,
) {
    for interaction in &back_interactions {
        if *interaction == Interaction::Pressed {
            *phase = CafePhase::MainMenu;
            mouse.clear_just_pressed(MouseButton::Left);
            return;
        }
    }

    let mode = match *phase {
        CafePhase::SlotPicker(m) => m,
        _ => return,
    };

    for (interaction, slot) in &slot_interactions {
        if *interaction != Interaction::Pressed {
            continue;
        }
        let slot = slot.0;
        let status = cache.0.get(&slot).cloned().unwrap_or(SlotStatus::Empty);
        match (mode, status) {
            (SlotPickerMode::LoadGame, SlotStatus::Occupied(_)) => match saves::load_slot(slot) {
                Ok(Some(file)) => {
                    saves::apply_save(
                        &file,
                        &mut commands,
                        &asset_server,
                        &mut occupancy,
                        &mut shop,
                        &mut balance,
                        &mut time,
                        &mut cafe_open,
                    );
                    *phase = CafePhase::Playing { slot };
                    info!("Loaded cafe-sim slot {slot}");
                }
                Ok(None) => {
                    warn!("Slot {slot} disappeared between probe and load");
                }
                Err(e) => {
                    error!("Failed to load slot {slot}: {e}");
                }
            },
            (SlotPickerMode::LoadGame, _) => {
                // Empty or incompatible — ignore click.
            }
            (SlotPickerMode::NewGame, _) => {
                saves::apply_new_game(&mut shop, &mut balance, &mut time, &mut cafe_open);
                *phase = CafePhase::Playing { slot };
                info!("Starting new game in slot {slot}");
            }
        }
        mouse.clear_just_pressed(MouseButton::Left);
    }
}
