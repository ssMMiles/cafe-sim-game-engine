//! Staff — hire employees and assign them jobs.
//!
//! This is a UI + data scaffold: staff members carry randomly-rolled stats
//! (drink-making, movement speed, friendliness) and a per-day wage, but those
//! stats do not yet affect gameplay. They are displayed in the Shop Overview's
//! Staff section and persisted in the save file.
//!
//! The Shop Overview body (in `shop_overview.rs`) renders the roster and the
//! inline "Assign Job" dropdown; this module owns the data model, the random
//! candidate generation, the marker components, the click handlers, and the
//! separate full-screen Hire menu (an [`ActiveMenu::HireStaff`] modal).

use bevy::{picking::hover::Hovered, prelude::*, ui::Pressed, ui_widgets::Button};
use rand::Rng;
use rand::seq::SliceRandom;
use rkyv::{Archive, Deserialize as RkyvDeserialize, Serialize as RkyvSerialize};

use crate::menus::ActiveMenu;
use crate::spawn::{SpawnOwnedCommands, WithChildOwned};

// ----------------------------------------------------------------------------
// Data model
// ----------------------------------------------------------------------------

/// A job a staff member can hold. rkyv-derived so it serializes in the save.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Archive, RkyvSerialize, RkyvDeserialize)]
pub enum Job {
    Barista,
}

impl Job {
    pub fn title(self) -> &'static str {
        match self {
            Job::Barista => "Barista",
        }
    }
}

/// Every assignable job. Currently just one, but the Assign Job dropdown is
/// built off this list so adding jobs here surfaces them in the UI.
pub const JOBS: &[Job] = &[Job::Barista];

/// One employee. Plain data (not an ECS component), rkyv-derived so a
/// `Vec<StaffMember>` stores directly in [`crate::saves::CafeSaveFile`].
#[derive(Clone, Debug, Archive, RkyvSerialize, RkyvDeserialize)]
pub struct StaffMember {
    pub name: String,
    /// Fractional change to drink-making TIME, `[-0.5, 0.5]`. Negative = faster.
    pub drink_time_mod: f32,
    /// Fractional change to movement speed, `[-0.2, 0.4]`.
    pub move_speed_mod: f32,
    /// `[0.0, 1.0]`.
    pub friendliness: f32,
    /// Per-day wage, derived from the stats with ±20% variance.
    pub pay: i64,
    /// `None` = "Chilling" (no job assigned).
    pub job: Option<Job>,
}

/// The current roster.
#[derive(Resource, Default)]
pub struct Staff {
    pub members: Vec<StaffMember>,
}

/// Candidates shown in the Hire menu; regenerated each time the menu opens.
#[derive(Resource, Default)]
pub struct HireCandidates(pub Vec<StaffMember>);

/// Which hired-staff card (index into [`Staff::members`]) currently has its job
/// dropdown expanded. `None` = all collapsed.
#[derive(Resource, Default)]
pub struct AssignJobOpenFor(pub Option<usize>);

// ----------------------------------------------------------------------------
// Marker components
// ----------------------------------------------------------------------------

/// The "Hire Staff" button in the Shop Overview's Staff section.
#[derive(Component)]
pub struct HireStaffButton;

/// "Assign Job" button on a roster card; carries the staff index it controls.
#[derive(Component)]
pub struct AssignJobButton {
    pub staff: usize,
}

/// A job option in the inline assign dropdown.
#[derive(Component)]
pub struct JobOptionButton {
    pub staff: usize,
    pub job: Job,
}

/// Root of the full-screen Hire menu modal.
#[derive(Component)]
pub struct HireMenuRoot;

/// "Hire" button on a candidate card in the Hire menu.
#[derive(Component)]
pub struct HireCandidateButton {
    pub index: usize,
}

/// "Back" button in the Hire menu (returns to the Shop Overview).
#[derive(Component)]
pub struct HireBackButton;

// ----------------------------------------------------------------------------
// Generation
// ----------------------------------------------------------------------------

const FIRST_NAMES: &[&str] = &[
    "Alex", "Sam", "Jordan", "Riley", "Casey", "Morgan", "Taylor", "Jamie", "Quinn", "Avery",
    "Drew", "Skyler", "Reese", "Rowan", "Charlie", "Finley", "Harper", "Emerson", "Sage", "Blair",
];

/// Roll a fresh candidate with random stats and a stats-correlated wage.
pub fn generate_candidate() -> StaffMember {
    let mut rng = rand::thread_rng();

    let name = FIRST_NAMES
        .choose(&mut rng)
        .copied()
        .unwrap_or("Pat")
        .to_string();
    let drink_time_mod = rng.gen_range(-0.5..=0.5);
    let move_speed_mod = rng.gen_range(-0.2..=0.4);
    let friendliness = rng.gen_range(0.0..=1.0);

    // Quality in [0, 1]: faster drinks, higher speed, and higher friendliness
    // all push it up. Each component is normalized to its own range first.
    let drink_q = (0.5 - drink_time_mod) / 1.0; // -0.5 -> 1.0, +0.5 -> 0.0
    let speed_q = (move_speed_mod + 0.2) / 0.6; // -0.2 -> 0.0, +0.4 -> 1.0
    let quality = (drink_q + speed_q + friendliness) / 3.0;

    // Map quality to a base wage, then apply ±20% luck.
    const PAY_MIN: f32 = 8.0;
    const PAY_MAX: f32 = 25.0;
    let base = PAY_MIN + quality * (PAY_MAX - PAY_MIN);
    let variance = rng.gen_range(0.8..=1.2);
    let pay = (base * variance).round().max(1.0) as i64;

    StaffMember {
        name,
        drink_time_mod,
        move_speed_mod,
        friendliness,
        pay,
        job: None,
    }
}

/// Roll 3–5 fresh candidates for the Hire menu.
pub fn generate_candidates() -> Vec<StaffMember> {
    let count = rand::thread_rng().gen_range(3..=5);
    (0..count).map(|_| generate_candidate()).collect()
}

// ----------------------------------------------------------------------------
// Handlers
// ----------------------------------------------------------------------------

/// "Hire Staff" → roll fresh candidates and open the Hire menu.
pub fn handle_hire_staff_button(
    pressed: Query<(), (Added<Pressed>, With<HireStaffButton>)>,
    mut active: ResMut<ActiveMenu>,
    mut candidates: ResMut<HireCandidates>,
    mut mouse: ResMut<ButtonInput<MouseButton>>,
) {
    for _ in &pressed {
        candidates.0 = generate_candidates();
        *active = ActiveMenu::HireStaff;
        mouse.clear_just_pressed(MouseButton::Left);
    }
}

/// Hire a candidate → add to the roster (job `None`) and return to the overview.
pub fn handle_hire_candidate_click(
    pressed: Query<&HireCandidateButton, Added<Pressed>>,
    mut staff: ResMut<Staff>,
    mut candidates: ResMut<HireCandidates>,
    mut active: ResMut<ActiveMenu>,
    mut mouse: ResMut<ButtonInput<MouseButton>>,
) {
    for btn in &pressed {
        if let Some(candidate) = candidates.0.get(btn.index).cloned() {
            staff.members.push(candidate);
        }
        candidates.0.clear();
        *active = ActiveMenu::ShopOverview;
        mouse.clear_just_pressed(MouseButton::Left);
    }
}

/// "Back" in the Hire menu → return to the overview without hiring.
pub fn handle_hire_back_button(
    pressed: Query<(), (Added<Pressed>, With<HireBackButton>)>,
    mut active: ResMut<ActiveMenu>,
    mut mouse: ResMut<ButtonInput<MouseButton>>,
) {
    for _ in &pressed {
        *active = ActiveMenu::ShopOverview;
        mouse.clear_just_pressed(MouseButton::Left);
    }
}

/// "Assign Job" → toggle the inline job dropdown for that card.
pub fn handle_assign_job_button(
    pressed: Query<&AssignJobButton, Added<Pressed>>,
    mut open_for: ResMut<AssignJobOpenFor>,
    mut mouse: ResMut<ButtonInput<MouseButton>>,
) {
    for btn in &pressed {
        open_for.0 = if open_for.0 == Some(btn.staff) {
            None
        } else {
            Some(btn.staff)
        };
        mouse.clear_just_pressed(MouseButton::Left);
    }
}

/// Pick a job from the inline dropdown → assign it and collapse the dropdown.
pub fn handle_job_option_click(
    pressed: Query<&JobOptionButton, Added<Pressed>>,
    mut staff: ResMut<Staff>,
    mut open_for: ResMut<AssignJobOpenFor>,
    mut mouse: ResMut<ButtonInput<MouseButton>>,
) {
    for btn in &pressed {
        if let Some(member) = staff.members.get_mut(btn.staff) {
            member.job = Some(btn.job);
        }
        open_for.0 = None;
        mouse.clear_just_pressed(MouseButton::Left);
    }
}

// ----------------------------------------------------------------------------
// Hire menu UI
// ----------------------------------------------------------------------------

/// Rebuild the Hire menu whenever [`ActiveMenu`] changes. Mirrors
/// `sync_coffee_menu_ui`: despawn the old root, and if `HireStaff` is active,
/// render a candidate carousel. Candidates are rolled by
/// `handle_hire_staff_button` before this runs.
pub fn build_hire_menu(
    mut commands: Commands,
    active: Res<ActiveMenu>,
    candidates: Res<HireCandidates>,
    existing: Query<Entity, With<HireMenuRoot>>,
) {
    if !active.is_changed() {
        return;
    }

    for e in &existing {
        commands.entity(e).despawn();
    }

    if !active.is(ActiveMenu::HireStaff) {
        return;
    }

    let panel_bg = Color::srgba(0.06, 0.07, 0.10, 0.92);
    let card_bg = Color::srgba(0.18, 0.22, 0.28, 1.0);
    let primary_bg = Color::srgb(0.16, 0.40, 0.52);
    let back_bg = Color::srgb(0.30, 0.34, 0.40);
    let text_color = Color::srgb(0.95, 0.95, 0.95);
    let dim_color = Color::srgb(0.65, 0.70, 0.78);
    let border = Color::srgb(0.22, 0.27, 0.34);

    let root = commands
        .spawn_owned((
            HireMenuRoot,
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

    let title = commands
        .spawn_owned((
            Text::new("Hire Staff"),
            TextFont {
                font_size: FontSize::Px(28.0),
                ..default()
            },
            TextColor(text_color),
        ))
        .id();
    commands.entity(root).add_child(title);

    let row = commands
        .spawn_owned((Node {
            flex_direction: FlexDirection::Row,
            column_gap: Val::Px(12.0),
            flex_wrap: FlexWrap::Wrap,
            ..default()
        },))
        .id();
    commands.entity(root).add_child(row);

    for (idx, c) in candidates.0.iter().enumerate() {
        let card = commands
            .spawn_owned((
                Node {
                    width: Val::Px(180.0),
                    padding: UiRect::all(Val::Px(12.0)),
                    border: UiRect::all(Val::Px(2.0)),
                    border_radius: BorderRadius::all(Val::Px(6.0)),
                    flex_direction: FlexDirection::Column,
                    row_gap: Val::Px(6.0),
                    ..default()
                },
                BackgroundColor(card_bg),
                BorderColor::all(border),
            ))
            .id();
        commands.entity(row).add_child(card);

        let name = commands
            .spawn_owned((
                Text::new(c.name.clone()),
                TextFont {
                    font_size: FontSize::Px(20.0),
                    ..default()
                },
                TextColor(text_color),
            ))
            .id();
        commands.entity(card).add_child(name);

        for line in stat_lines(c) {
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

        let pay = commands
            .spawn_owned((
                Text::new(format!("${}/day", c.pay)),
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
        commands.entity(card).add_child(pay);

        let hire_btn = spawn_button(
            &mut commands,
            HireCandidateButton { index: idx },
            "Hire",
            primary_bg,
            border,
            text_color,
        );
        commands.entity(card).add_child(hire_btn);
    }

    let back_btn = spawn_button(
        &mut commands,
        HireBackButton,
        "Back",
        back_bg,
        border,
        text_color,
    );
    commands.entity(root).add_child(back_btn);
}

// ----------------------------------------------------------------------------
// Shared formatting / button helpers (also used by shop_overview's roster)
// ----------------------------------------------------------------------------

/// The three stat lines for a staff/candidate card.
pub fn stat_lines(m: &StaffMember) -> [String; 3] {
    [
        format!("Drink Time: {:+.0}%", m.drink_time_mod * 100.0),
        format!("Move Speed: {:+.0}%", m.move_speed_mod * 100.0),
        format!("Friendliness: {:.0}%", m.friendliness * 100.0),
    ]
}

/// Minimal labeled action button. Shared by the Hire menu and the roster cards
/// so both keep the same look without re-deriving the node layout.
pub fn spawn_button(
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
            Hovered::default(),
            Node {
                padding: UiRect::axes(Val::Px(14.0), Val::Px(8.0)),
                border: UiRect::all(Val::Px(2.0)),
                border_radius: BorderRadius::all(Val::Px(4.0)),
                justify_content: JustifyContent::Center,
                align_items: AlignItems::Center,
                margin: UiRect::top(Val::Px(4.0)),
                ..default()
            },
            BorderColor::all(border),
            BackgroundColor(bg),
        ))
        .with_child_owned((
            Text::new(label.to_string()),
            TextFont {
                font_size: FontSize::Px(15.0),
                ..default()
            },
            TextColor(text),
        ))
        .id()
}
