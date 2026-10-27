//! Shop Overview panel — toggled with Tab.
//!
//! Header has the shop name in the top-left with an Edit button next to it.
//! Clicking Edit swaps the label for an inline text input and the button
//! becomes Done; Enter/Escape on the input also commits/cancels. The body of
//! the panel is intentionally empty for now.

use bevy::input::keyboard::{Key, KeyboardInput};
use bevy::prelude::*;

use crate::balance::{Balance, DayStartBalance};
use crate::cafe::CafeOpen;
use crate::menus::ActiveMenu;
use crate::pause_menu::GamePaused;
use crate::spawn::{SpawnOwnedCommands, WithChildOwned};
use crate::staff::{
    AssignJobButton, AssignJobOpenFor, HireStaffButton, JOBS, JobOptionButton, Staff, spawn_button,
    stat_lines,
};
use crate::time::GameTime;

#[derive(Resource, Debug)]
pub struct ShopState {
    pub name: String,
}

impl Default for ShopState {
    fn default() -> Self {
        Self {
            name: "My Shop".to_string(),
        }
    }
}

#[derive(Resource, Default)]
pub struct ShopNameEditing(pub bool);

#[derive(Component)]
pub struct ShopOverviewRoot;

#[derive(Component)]
pub struct ShopNameInput {
    pub value: String,
}

#[derive(Component)]
pub struct ShopNameInputDisplay;

#[derive(Component)]
pub struct ShopNameInputFocus;

#[derive(Component)]
pub struct ShopEditButton;

#[derive(Component)]
pub struct CafeOpenButton;

#[derive(Component)]
pub struct CafeCloseButton;

pub fn toggle_shop_overview(
    input: Res<ButtonInput<KeyCode>>,
    paused: Res<GamePaused>,
    mut active: ResMut<ActiveMenu>,
) {
    if paused.0 {
        return;
    }
    if input.just_pressed(KeyCode::Tab) {
        active.toggle(ActiveMenu::ShopOverview);
    }
}

/// Runs whenever [`ActiveMenu`] changes. When the overview is no longer the
/// active menu, commit any in-progress name edit and exit editing mode. This
/// fires for every close path — Tab, pause, opening another menu.
pub fn cleanup_shop_overview_when_inactive(
    active: Res<ActiveMenu>,
    mut editing: ResMut<ShopNameEditing>,
    mut shop_state: ResMut<ShopState>,
    input_q: Query<&ShopNameInput>,
) {
    if !active.is_changed() || active.is(ActiveMenu::ShopOverview) {
        return;
    }
    if !editing.0 {
        return;
    }
    if let Ok(ti) = input_q.single() {
        shop_state.name = sanitize_name(&ti.value);
    }
    editing.0 = false;
}

pub fn handle_open_cafe_button(
    interactions: Query<&Interaction, (Changed<Interaction>, With<CafeOpenButton>)>,
    mut cafe_open: ResMut<CafeOpen>,
    mut mouse: ResMut<ButtonInput<MouseButton>>,
) {
    for interaction in &interactions {
        if *interaction == Interaction::Pressed {
            cafe_open.0 = true;
            mouse.clear_just_pressed(MouseButton::Left);
        }
    }
}

pub fn handle_close_cafe_button(
    interactions: Query<&Interaction, (Changed<Interaction>, With<CafeCloseButton>)>,
    mut cafe_open: ResMut<CafeOpen>,
    mut game_time: ResMut<GameTime>,
    balance: Res<Balance>,
    mut day_start: ResMut<DayStartBalance>,
    mut mouse: ResMut<ButtonInput<MouseButton>>,
) {
    for interaction in &interactions {
        if *interaction == Interaction::Pressed {
            cafe_open.0 = false;
            game_time.start_next_day();
            // The day's earnings are now banked — the next day starts from here.
            day_start.0 = balance.0;
            mouse.clear_just_pressed(MouseButton::Left);
        }
    }
}

pub fn handle_edit_button(
    interactions: Query<&Interaction, (Changed<Interaction>, With<ShopEditButton>)>,
    mut editing: ResMut<ShopNameEditing>,
    mut shop_state: ResMut<ShopState>,
    input_q: Query<&ShopNameInput>,
    mut mouse: ResMut<ButtonInput<MouseButton>>,
) {
    let mut clicked = false;
    for interaction in &interactions {
        if *interaction == Interaction::Pressed {
            clicked = true;
        }
    }
    if !clicked {
        return;
    }
    if editing.0 {
        if let Ok(ti) = input_q.single() {
            shop_state.name = sanitize_name(&ti.value);
        }
        editing.0 = false;
    } else {
        editing.0 = true;
    }
    mouse.clear_just_pressed(MouseButton::Left);
}

pub fn focus_shop_name_input(
    mut commands: Commands,
    interactions: Query<(Entity, &Interaction), (Changed<Interaction>, With<ShopNameInput>)>,
    focused: Query<Entity, With<ShopNameInputFocus>>,
) {
    for (entity, interaction) in &interactions {
        if *interaction != Interaction::Pressed {
            continue;
        }
        for old in &focused {
            commands.entity(old).remove::<ShopNameInputFocus>();
        }
        commands.entity(entity).insert(ShopNameInputFocus);
    }
}

pub fn apply_shop_name_keystrokes(
    mut commands: Commands,
    mut events: MessageReader<KeyboardInput>,
    mut focused: Query<(Entity, &mut ShopNameInput), With<ShopNameInputFocus>>,
    mut editing: ResMut<ShopNameEditing>,
    mut shop_state: ResMut<ShopState>,
) {
    let Ok((entity, mut input)) = focused.single_mut() else {
        events.clear();
        return;
    };
    for ev in events.read() {
        if !ev.state.is_pressed() {
            continue;
        }
        match &ev.logical_key {
            Key::Character(s) => {
                for ch in s.chars() {
                    if !ch.is_control() {
                        input.value.push(ch);
                    }
                }
            }
            Key::Space => input.value.push(' '),
            Key::Backspace => {
                input.value.pop();
            }
            Key::Enter => {
                shop_state.name = sanitize_name(&input.value);
                editing.0 = false;
                commands.entity(entity).remove::<ShopNameInputFocus>();
            }
            Key::Escape => {
                editing.0 = false;
                commands.entity(entity).remove::<ShopNameInputFocus>();
            }
            _ => {}
        }
    }
}

pub fn sync_shop_name_display(
    inputs: Query<(&ShopNameInput, &Children), Changed<ShopNameInput>>,
    mut texts: Query<&mut Text, With<ShopNameInputDisplay>>,
) {
    for (input, children) in &inputs {
        for child in children.iter() {
            if let Ok(mut text) = texts.get_mut(child) {
                *text = Text::new(input.value.clone());
            }
        }
    }
}

pub fn build_shop_overview(
    mut commands: Commands,
    active: Res<ActiveMenu>,
    editing: Res<ShopNameEditing>,
    shop_state: Res<ShopState>,
    cafe_open: Res<CafeOpen>,
    staff: Res<Staff>,
    assign_open: Res<AssignJobOpenFor>,
    existing: Query<Entity, With<ShopOverviewRoot>>,
) {
    if !active.is_changed()
        && !editing.is_changed()
        && !cafe_open.is_changed()
        && !staff.is_changed()
        && !assign_open.is_changed()
    {
        return;
    }

    for e in &existing {
        commands.entity(e).despawn();
    }

    if !active.is(ActiveMenu::ShopOverview) {
        return;
    }

    let panel_bg = Color::srgba(0.06, 0.07, 0.10, 0.92);
    let surface_bg = Color::srgba(0.18, 0.22, 0.28, 1.0);
    let primary_bg = Color::srgb(0.16, 0.40, 0.52);
    let open_bg = Color::srgb(0.20, 0.52, 0.30);
    let close_bg = Color::srgb(0.60, 0.32, 0.20);
    let text_color = Color::srgb(0.95, 0.95, 0.95);
    let border = Color::srgb(0.22, 0.27, 0.34);

    let root = commands
        .spawn_owned((
            ShopOverviewRoot,
            Node {
                position_type: PositionType::Absolute,
                left: Val::Percent(10.0),
                right: Val::Percent(10.0),
                top: Val::Percent(10.0),
                bottom: Val::Percent(10.0),
                padding: UiRect::all(Val::Px(16.0)),
                flex_direction: FlexDirection::Column,
                row_gap: Val::Px(16.0),
                overflow: Overflow::clip(),
                ..default()
            },
            BackgroundColor(panel_bg),
            GlobalZIndex(100),
        ))
        .id();

    let header = commands
        .spawn_owned((Node {
            flex_direction: FlexDirection::Row,
            align_items: AlignItems::Center,
            column_gap: Val::Px(12.0),
            ..default()
        },))
        .id();
    commands.entity(root).add_child(header);

    if editing.0 {
        let input = commands
            .spawn_owned((
                Button,
                Node {
                    width: Val::Px(320.0),
                    height: Val::Px(48.0),
                    border: UiRect::all(Val::Px(2.0)),
                    border_radius: BorderRadius::all(Val::Px(4.0)),
                    align_items: AlignItems::Center,
                    padding: UiRect::horizontal(Val::Px(12.0)),
                    ..default()
                },
                BorderColor::all(border),
                BackgroundColor(surface_bg),
                ShopNameInput {
                    value: shop_state.name.clone(),
                },
                ShopNameInputFocus,
            ))
            .id();
        let display = commands
            .spawn_owned((
                ShopNameInputDisplay,
                Text::new(shop_state.name.clone()),
                TextFont {
                    font_size: FontSize::Px(20.0),
                    ..default()
                },
                TextColor(text_color),
            ))
            .id();
        commands.entity(input).add_child(display);
        commands.entity(header).add_child(input);

        let done_btn = spawn_action_button(
            &mut commands,
            ShopEditButton,
            "Done",
            primary_bg,
            border,
            text_color,
        );
        commands.entity(header).add_child(done_btn);
    } else {
        let name = commands
            .spawn_owned((
                Text::new(shop_state.name.clone()),
                TextFont {
                    font_size: FontSize::Px(36.0),
                    ..default()
                },
                TextColor(text_color),
            ))
            .id();
        commands.entity(header).add_child(name);

        let edit_btn = spawn_action_button(
            &mut commands,
            ShopEditButton,
            "Edit",
            primary_bg,
            border,
            text_color,
        );
        commands.entity(header).add_child(edit_btn);

        let spacer = commands
            .spawn_owned((Node {
                flex_grow: 1.0,
                ..default()
            },))
            .id();
        commands.entity(header).add_child(spacer);

        if cafe_open.0 {
            let close_btn = spawn_action_button(
                &mut commands,
                CafeCloseButton,
                "Close Early",
                close_bg,
                border,
                text_color,
            );
            commands.entity(header).add_child(close_btn);
        } else {
            let open_btn = spawn_action_button(
                &mut commands,
                CafeOpenButton,
                "Open Shop",
                open_bg,
                border,
                text_color,
            );
            commands.entity(header).add_child(open_btn);
        }
    }

    // ---- Staff section ----
    let dim_color = Color::srgb(0.65, 0.70, 0.78);
    let assign_bg = Color::srgb(0.30, 0.34, 0.40);

    let section = commands
        .spawn_owned((Node {
            flex_direction: FlexDirection::Column,
            row_gap: Val::Px(10.0),
            ..default()
        },))
        .id();
    commands.entity(root).add_child(section);

    let staff_title = commands
        .spawn_owned((
            Text::new("Staff"),
            TextFont {
                font_size: FontSize::Px(24.0),
                ..default()
            },
            TextColor(text_color),
        ))
        .id();
    commands.entity(section).add_child(staff_title);

    if staff.members.is_empty() {
        let empty = commands
            .spawn_owned((
                Text::new("No staff yet."),
                TextFont {
                    font_size: FontSize::Px(14.0),
                    ..default()
                },
                TextColor(dim_color),
            ))
            .id();
        commands.entity(section).add_child(empty);
    }

    let cards_row = commands
        .spawn_owned((Node {
            flex_direction: FlexDirection::Row,
            column_gap: Val::Px(12.0),
            flex_wrap: FlexWrap::Wrap,
            ..default()
        },))
        .id();
    commands.entity(section).add_child(cards_row);

    for (i, member) in staff.members.iter().enumerate() {
        let card = commands
            .spawn_owned((
                Node {
                    width: Val::Px(190.0),
                    padding: UiRect::all(Val::Px(12.0)),
                    border: UiRect::all(Val::Px(2.0)),
                    border_radius: BorderRadius::all(Val::Px(6.0)),
                    flex_direction: FlexDirection::Column,
                    row_gap: Val::Px(6.0),
                    ..default()
                },
                BackgroundColor(surface_bg),
                BorderColor::all(border),
            ))
            .id();
        commands.entity(cards_row).add_child(card);

        let name = commands
            .spawn_owned((
                Text::new(member.name.clone()),
                TextFont {
                    font_size: FontSize::Px(20.0),
                    ..default()
                },
                TextColor(text_color),
            ))
            .id();
        commands.entity(card).add_child(name);

        for line in stat_lines(member) {
            let stat = commands
                .spawn_owned((
                    Text::new(line),
                    TextFont {
                        font_size: FontSize::Px(13.0),
                        ..default()
                    },
                    TextColor(dim_color),
                ))
                .id();
            commands.entity(card).add_child(stat);
        }

        let job_label = member.job.map(|j| j.title()).unwrap_or("Chilling");
        let job = commands
            .spawn_owned((
                Text::new(format!("Job: {job_label}")),
                TextFont {
                    font_size: FontSize::Px(15.0),
                    ..default()
                },
                TextColor(text_color),
                Node {
                    margin: UiRect::top(Val::Px(4.0)),
                    ..default()
                },
            ))
            .id();
        commands.entity(card).add_child(job);

        let assign_btn = spawn_button(
            &mut commands,
            AssignJobButton { staff: i },
            "Assign Job",
            assign_bg,
            border,
            text_color,
        );
        commands.entity(card).add_child(assign_btn);

        if assign_open.0 == Some(i) {
            for &job in JOBS {
                let opt = spawn_button(
                    &mut commands,
                    JobOptionButton { staff: i, job },
                    job.title(),
                    primary_bg,
                    border,
                    text_color,
                );
                commands.entity(card).add_child(opt);
            }
        }
    }

    let hire_btn = spawn_button(
        &mut commands,
        HireStaffButton,
        "Hire Staff",
        primary_bg,
        border,
        text_color,
    );
    commands.entity(section).add_child(hire_btn);
}

fn spawn_action_button(
    commands: &mut Commands,
    marker: impl Bundle,
    label: &str,
    bg: Color,
    border: Color,
    text: Color,
) -> Entity {
    commands
        .spawn_owned((
            marker,
            Button,
            Node {
                padding: UiRect::axes(Val::Px(16.0), Val::Px(8.0)),
                border: UiRect::all(Val::Px(2.0)),
                border_radius: BorderRadius::all(Val::Px(4.0)),
                justify_content: JustifyContent::Center,
                align_items: AlignItems::Center,
                ..default()
            },
            BorderColor::all(border),
            BackgroundColor(bg),
        ))
        .with_child_owned((
            Text::new(label.to_string()),
            TextFont {
                font_size: FontSize::Px(16.0),
                ..default()
            },
            TextColor(text),
        ))
        .id()
}

fn sanitize_name(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        "My Shop".to_string()
    } else {
        trimmed.to_string()
    }
}
