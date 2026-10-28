//! Brew menu — pops up when the player left-clicks on a placed coffee machine.
//!
//! Mirrors the build shop's panel layout but renders a horizontal carousel of
//! drinks pulled from [`crate::drinks::DRINKS`]. Selecting a drink closes the
//! menu and attaches a [`Brewing`] component to the machine that was clicked.
//! Acts as a modal, so placement and cursor-lock systems all gate on the active
//! menu.
//!
//! Once brewing, the machine sprouts a floating screen-space popup showing the
//! drink name and a progress bar, then "Ready!" when done — the same pattern
//! customer popups use. While a machine is brewing, clicking it does not
//! re-open the menu.

use bevy::window::PrimaryWindow;
use bevy::{picking::hover::Hovered, prelude::*, ui::Pressed, ui_widgets::Button};
use engine_api::PlayerCamera;
use std::collections::HashSet;

use crate::drinks::DRINKS;
use crate::menus::ActiveMenu;
use crate::pause_menu::GamePaused;
use crate::placement::{CoffeeMachine, PlacementSelection};
use crate::spawn::{SpawnOwnedCommands, WithChildOwned};

const CARD_W: f32 = 140.0;
const CARD_H: f32 = 140.0;
const CARD_GAP: f32 = 8.0;

const POPUP_OFFSET_Y: f32 = 1.1;
const POPUP_WIDTH: f32 = 140.0;
const POPUP_FONT_SIZE: f32 = 14.0;
const PROGRESS_BAR_HEIGHT: f32 = 6.0;

#[derive(Component)]
pub struct CoffeeMenuRoot;

#[derive(Component)]
pub struct DrinkCard {
    pub index: usize,
}

/// Machine the brew menu is currently bound to. Set when `handle_machine_click`
/// opens the menu, cleared when the menu transitions away from Coffee, read by
/// `handle_drink_click` to know which machine to start brewing.
#[derive(Resource, Default)]
pub struct ActiveCoffeeMachine(pub Option<Entity>);

/// Attached to a [`CoffeeMachine`] entity while it's producing a drink. Once
/// `elapsed >= total_secs` the popup reads "Ready!" but the component stays
/// (no pickup mechanic yet).
#[derive(Component)]
pub struct Brewing {
    pub drink: &'static str,
    pub total_secs: f32,
    pub elapsed: f32,
}

impl Brewing {
    pub fn ready(&self) -> bool {
        self.elapsed >= self.total_secs
    }

    pub fn progress(&self) -> f32 {
        if self.total_secs <= 0.0 {
            return 1.0;
        }
        (self.elapsed / self.total_secs).clamp(0.0, 1.0)
    }
}

/// Screen-space label + progress bar parented to no machine but tracking one.
#[derive(Component)]
pub struct BrewPopup {
    pub machine: Entity,
}

#[derive(Component)]
pub struct BrewPopupText;

#[derive(Component)]
pub struct BrewPopupBar;

pub fn handle_machine_click(
    mouse: Res<ButtonInput<MouseButton>>,
    paused: Res<GamePaused>,
    selection: Res<PlacementSelection>,
    mut active: ResMut<ActiveMenu>,
    mut active_machine: ResMut<ActiveCoffeeMachine>,
    cam_q: Query<&GlobalTransform, With<PlayerCamera>>,
    machine_q: Query<Has<Brewing>, With<CoffeeMachine>>,
    parent_q: Query<&ChildOf>,
    mut ray_cast: MeshRayCast,
) {
    if paused.0 {
        return;
    }
    if active.is_open() && !active.is(ActiveMenu::Coffee) {
        return;
    }
    if selection.piece.is_some() {
        return;
    }
    if !mouse.just_pressed(MouseButton::Left) {
        return;
    }
    let Ok(cam) = cam_q.single() else {
        return;
    };
    let ray = Ray3d::new(cam.translation(), cam.forward());
    let hits = ray_cast.cast_ray(ray, &MeshRayCastSettings::default());
    let Some(&(hit_entity, _)) = hits.first() else {
        return;
    };

    // Walk up the hierarchy from the hit mesh until we find a CoffeeMachine
    // ancestor (the hit lands on a deep mesh child of the scene root).
    let mut entity = hit_entity;
    let (machine, is_brewing) = loop {
        if let Ok(is_brewing) = machine_q.get(entity) {
            break (entity, is_brewing);
        }
        match parent_q.get(entity) {
            Ok(child_of) => entity = child_of.parent(),
            Err(_) => return,
        }
    };
    if is_brewing {
        return;
    }

    if active.is(ActiveMenu::Coffee) && active_machine.0 == Some(machine) {
        active.close();
        active_machine.0 = None;
    } else {
        *active = ActiveMenu::Coffee;
        active_machine.0 = Some(machine);
    }
}

/// Clear the active-machine pointer whenever the menu leaves the Coffee state
/// (Escape, pause, opening a different menu).
pub fn sync_active_coffee_machine(
    active: Res<ActiveMenu>,
    mut active_machine: ResMut<ActiveCoffeeMachine>,
) {
    if active.is_changed() && !active.is(ActiveMenu::Coffee) {
        active_machine.0 = None;
    }
}

pub fn sync_coffee_menu_ui(
    mut commands: Commands,
    active: Res<ActiveMenu>,
    existing: Query<Entity, With<CoffeeMenuRoot>>,
) {
    if !active.is_changed() {
        return;
    }

    for e in &existing {
        commands.entity(e).despawn();
    }

    if !active.is(ActiveMenu::Coffee) {
        return;
    }

    let panel_bg = Color::srgba(0.06, 0.07, 0.10, 0.92);
    let card_bg = Color::srgba(0.18, 0.22, 0.28, 1.0);
    let text_color = Color::srgb(0.95, 0.95, 0.95);
    let dim_color = Color::srgb(0.65, 0.70, 0.78);

    let root = commands
        .spawn_owned((
            CoffeeMenuRoot,
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
            Text::new("Drinks"),
            TextFont {
                font_size: FontSize::Px(28.0),
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
    commands.entity(root).add_child(row);

    for (idx, drink) in DRINKS.iter().enumerate() {
        let card = commands
            .spawn_owned((
                DrinkCard { index: idx },
                Button,
                Hovered::default(),
                Node {
                    width: Val::Px(CARD_W),
                    height: Val::Px(CARD_H),
                    flex_shrink: 0.0,
                    flex_direction: FlexDirection::Column,
                    justify_content: JustifyContent::Center,
                    align_items: AlignItems::Center,
                    overflow: Overflow::clip(),
                    row_gap: Val::Px(6.0),
                    ..default()
                },
                BackgroundColor(card_bg),
            ))
            .with_child_owned((
                Text::new(drink.name),
                TextFont {
                    font_size: FontSize::Px(16.0),
                    ..default()
                },
                TextColor(text_color),
            ))
            .with_child_owned((
                Text::new(format!("{:.0}s", drink.brew_secs)),
                TextFont {
                    font_size: FontSize::Px(12.0),
                    ..default()
                },
                TextColor(dim_color),
            ))
            .id();
        commands.entity(row).add_child(card);
    }
}

pub fn handle_drink_click(
    mut commands: Commands,
    pressed: Query<&DrinkCard, Added<Pressed>>,
    mut active: ResMut<ActiveMenu>,
    mut active_machine: ResMut<ActiveCoffeeMachine>,
    mut mouse: ResMut<ButtonInput<MouseButton>>,
    machine_q: Query<(), (With<CoffeeMachine>, Without<Brewing>)>,
) {
    for card in &pressed {
        let Some(drink) = DRINKS.get(card.index) else {
            continue;
        };
        if let Some(machine) = active_machine.0
            && machine_q.get(machine).is_ok()
        {
            commands.entity(machine).insert(Brewing {
                drink: drink.name,
                total_secs: drink.brew_secs,
                elapsed: 0.0,
            });
        }
        active.close();
        active_machine.0 = None;
        // Swallow the click so the same press doesn't re-toggle the menu via
        // handle_machine_click or trigger placement.
        mouse.clear_just_pressed(MouseButton::Left);
    }
}

pub fn tick_brewing(time: Res<Time>, paused: Res<GamePaused>, mut q: Query<&mut Brewing>) {
    if paused.0 {
        return;
    }
    let dt = time.delta_secs();
    for mut brewing in &mut q {
        if brewing.elapsed < brewing.total_secs {
            brewing.elapsed = (brewing.elapsed + dt).min(brewing.total_secs);
        }
    }
}

/// Spawn a popup node for any brewing machine that doesn't have one yet, and
/// despawn popups whose machine entity is gone or no longer brewing.
pub fn manage_brew_popups(
    mut commands: Commands,
    machines_q: Query<Entity, With<Brewing>>,
    popups_q: Query<(Entity, &BrewPopup)>,
) {
    let already_has_popup: HashSet<Entity> = popups_q.iter().map(|(_, p)| p.machine).collect();
    let brewing: HashSet<Entity> = machines_q.iter().collect();

    for (popup_entity, popup) in popups_q.iter() {
        if !brewing.contains(&popup.machine) {
            commands.entity(popup_entity).despawn();
        }
    }

    for machine in machines_q.iter() {
        if already_has_popup.contains(&machine) {
            continue;
        }

        let bar_track = Color::srgba(0.15, 0.18, 0.22, 1.0);
        let bar_fill = Color::srgb(0.45, 0.85, 0.55);

        let root = commands
            .spawn_owned((
                BrewPopup { machine },
                Node {
                    position_type: PositionType::Absolute,
                    width: Val::Px(POPUP_WIDTH),
                    padding: UiRect::axes(Val::Px(8.0), Val::Px(6.0)),
                    border_radius: BorderRadius::all(Val::Px(4.0)),
                    flex_direction: FlexDirection::Column,
                    align_items: AlignItems::Center,
                    row_gap: Val::Px(4.0),
                    ..default()
                },
                BackgroundColor(Color::srgba(0.0, 0.0, 0.0, 0.7)),
                Visibility::Hidden,
                GlobalZIndex(85),
            ))
            .id();

        let label = commands
            .spawn_owned((
                BrewPopupText,
                Text::new(""),
                TextFont {
                    font_size: FontSize::Px(POPUP_FONT_SIZE),
                    ..default()
                },
                TextColor(Color::srgb(1.0, 1.0, 1.0)),
            ))
            .id();
        commands.entity(root).add_child(label);

        let bar_bg = commands
            .spawn_owned((
                Node {
                    width: Val::Percent(100.0),
                    height: Val::Px(PROGRESS_BAR_HEIGHT),
                    border_radius: BorderRadius::all(Val::Px(2.0)),
                    overflow: Overflow::clip(),
                    ..default()
                },
                BackgroundColor(bar_track),
            ))
            .id();
        let bar_fg = commands
            .spawn_owned((
                BrewPopupBar,
                Node {
                    width: Val::Percent(0.0),
                    height: Val::Percent(100.0),
                    ..default()
                },
                BackgroundColor(bar_fill),
            ))
            .id();
        commands.entity(bar_bg).add_child(bar_fg);
        commands.entity(root).add_child(bar_bg);
    }
}

/// Write the current drink name (or "Ready!") and progress percentage into
/// each popup. Walks `Children` to find the popup's text + bar nodes — the
/// popup is shallow so the search is cheap.
pub fn update_brew_popups(
    machines_q: Query<&Brewing>,
    popups_q: Query<(&BrewPopup, &Children)>,
    children_q: Query<&Children>,
    mut text_q: Query<&mut Text, With<BrewPopupText>>,
    mut bar_q: Query<&mut Node, With<BrewPopupBar>>,
) {
    for (popup, root_children) in popups_q.iter() {
        let Ok(brewing) = machines_q.get(popup.machine) else {
            continue;
        };

        for child in root_children.iter() {
            if let Ok(mut text) = text_q.get_mut(child) {
                **text = if brewing.ready() {
                    format!("{} - Ready!", brewing.drink)
                } else {
                    brewing.drink.to_string()
                };
            }
            if let Ok(grand_children) = children_q.get(child) {
                for gc in grand_children.iter() {
                    if let Ok(mut node) = bar_q.get_mut(gc) {
                        node.width = Val::Percent(brewing.progress() * 100.0);
                    }
                }
            }
        }
    }
}

/// Project each brewing machine's world position through the camera and write
/// the result back to its popup's `top`/`left`. Mirrors `position_customer_popups`.
pub fn position_brew_popups(
    machines_q: Query<&GlobalTransform, With<Brewing>>,
    cam_q: Query<(&Camera, &GlobalTransform), With<PlayerCamera>>,
    window_q: Query<&Window, With<PrimaryWindow>>,
    mut popups_q: Query<(&BrewPopup, &mut Node, &mut Visibility)>,
) {
    let Ok((cam, cam_transform)) = cam_q.single() else {
        return;
    };
    let Ok(window) = window_q.single() else {
        return;
    };
    let Some(viewport_size) = cam.logical_viewport_size() else {
        return;
    };
    if viewport_size.x <= 0.0 || viewport_size.y <= 0.0 {
        return;
    }
    let scale = Vec2::new(window.width(), window.height()) / viewport_size;

    for (popup, mut node, mut visibility) in popups_q.iter_mut() {
        let Ok(machine_transform) = machines_q.get(popup.machine) else {
            *visibility = Visibility::Hidden;
            continue;
        };
        let head = machine_transform.translation() + Vec3::Y * POPUP_OFFSET_Y;
        let Ok(viewport_pos) = cam.world_to_viewport(cam_transform, head) else {
            *visibility = Visibility::Hidden;
            continue;
        };
        let screen = viewport_pos * scale;
        node.left = Val::Px(screen.x - POPUP_WIDTH * 0.5);
        node.top = Val::Px(screen.y - 40.0);
        *visibility = Visibility::Visible;
    }
}
