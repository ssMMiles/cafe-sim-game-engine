//! Brew station — plays a drink's recipe as a run of minigames.
//!
//! Picking a drink in the brew menu starts a [`BrewSession`] and opens
//! [`ActiveMenu::Brew`]: a centred overlay that shows the current step's
//! meters. Each step's score is flashed for a moment, then the next step
//! starts; after the last one the machine gets a ready [`Brewing`] carrying
//! the drink's quality. Closing the menu any other way (Throw away, pause)
//! throws the drink away.
//!
//! While a session runs the machine wears [`MachineInUse`], so staff and the
//! machine click leave it alone. The minigames themselves are pure state in
//! [`crate::minigames`]; this module only feeds them input and draws them.
//! All text is ASCII: bevy's default font is an ASCII subset.

use bevy::{
    picking::hover::Hovered,
    prelude::*,
    ui::{Pressed, RelativeCursorPosition},
};

use crate::coffee_menu::Brewing;
use crate::drinks::{Drink, PERFECT_STEP};
use crate::menus::ActiveMenu;
use crate::minigames::{
    MeterLayout, MeterShape, MeterZone, MinigameInput, MinigameState, Outcome, point_along,
};
use crate::pause_menu::GamePaused;
use crate::spawn::SpawnOwnedCommands;
use crate::staff::spawn_button;

/// How long a step's result shows before the next step starts.
const FLASH_SECS: f32 = 0.8;
const PANEL_WIDTH: f32 = 460.0;
const METER_LONG: f32 = 240.0;
const METER_THICK: f32 = 44.0;
const NEEDLE_PX: f32 = 4.0;
const DISH_SIZE: f32 = 180.0;
const CANVAS_SIZE: f32 = 240.0;
/// Dots drawn along a canvas path, evenly spaced by arc length.
const CANVAS_DOTS: usize = 48;
const PATH_DOT_PX: f32 = 6.0;
const MARKER_PX: f32 = 14.0;
const STEP_DOT_PX: f32 = 10.0;

const GOOD: Color = Color::srgb(0.28, 0.72, 0.40);
const BAD: Color = Color::srgb(0.75, 0.25, 0.25);
const GOLD: Color = Color::srgb(1.0, 0.80, 0.30);

/// Marks a coffee machine whose drink the player is making right now.
#[derive(Component)]
pub struct MachineInUse;

#[derive(Resource, Default)]
pub struct BrewSession(Option<Session>);

struct Session {
    machine: Entity,
    drink: &'static Drink,
    step: usize,
    state: MinigameState,
    scores: Vec<f32>,
    /// Counting down while the last step's result is shown.
    flash: Option<f32>,
}

impl BrewSession {
    /// Start making `drink` at `machine`. `false` (and nothing happens) for a
    /// drink with no steps.
    pub fn start(
        &mut self,
        commands: &mut Commands,
        machine: Entity,
        drink: &'static Drink,
    ) -> bool {
        let Some(first) = drink.steps.first() else {
            warn!("[cafe-sim] {} has no recipe steps", drink.name);
            return false;
        };
        commands.entity(machine).insert(MachineInUse);
        self.0 = Some(Session {
            machine,
            drink,
            step: 0,
            state: MinigameState::new(&first.game, &mut rand::rng()),
            scores: Vec::with_capacity(drink.steps.len()),
            flash: None,
        });
        true
    }
}

// ----------------------------------------------------------------------------
// UI markers
// ----------------------------------------------------------------------------

/// The overlay, tagged with the step it was built for so a new step rebuilds it.
#[derive(Component)]
pub struct BrewOverlayRoot {
    step: usize,
}

/// A node that follows meter `index`'s reading (in [`MinigameState::meters`]
/// order).
#[derive(Component)]
pub struct MeterPart {
    index: usize,
    kind: Part,
}

#[derive(Clone, Copy)]
enum Part {
    /// Positioned at the reading along a bar.
    Needle { vertical: bool },
    /// Sized from 0 up to the reading.
    Fill { vertical: bool },
    /// Positioned at the reading's fraction along a canvas path.
    Marker { path: &'static [Vec2] },
}

/// The meter the game measures the cursor over ([`MeterLayout::cursor`]).
#[derive(Component)]
pub struct CursorSurface;

#[derive(Component)]
pub struct StepResultText;

#[derive(Component)]
pub struct CancelBrewButton;

/// The step's Done button ([`MinigameState::confirm_label`]).
#[derive(Component)]
pub struct ConfirmStepButton;

// ----------------------------------------------------------------------------
// Systems
// ----------------------------------------------------------------------------

pub fn handle_brew_cancel(
    pressed: Query<(), (With<CancelBrewButton>, Added<Pressed>)>,
    mut active: ResMut<ActiveMenu>,
    mut mouse: ResMut<ButtonInput<MouseButton>>,
) {
    if !pressed.is_empty() && active.is(ActiveMenu::Brew) {
        active.close();
        mouse.clear_just_pressed(MouseButton::Left);
    }
}

/// Feed this frame's input to the current minigame and advance the session.
pub fn tick_brew_session(
    mut commands: Commands,
    time: Res<Time>,
    paused: Res<GamePaused>,
    mouse: Res<ButtonInput<MouseButton>>,
    keys: Res<ButtonInput<KeyCode>>,
    mut active: ResMut<ActiveMenu>,
    mut session: ResMut<BrewSession>,
    buttons_hover: Query<&Hovered, Or<(With<CancelBrewButton>, With<ConfirmStepButton>)>>,
    confirm_pressed: Query<(), (With<ConfirmStepButton>, Added<Pressed>)>,
    surface: Query<&RelativeCursorPosition, With<CursorSurface>>,
) {
    if paused.0 || !active.is(ActiveMenu::Brew) {
        return;
    }
    let Some(s) = session.0.as_mut() else {
        return;
    };
    let dt = time.delta_secs();

    if let Some(remaining) = s.flash.as_mut() {
        *remaining -= dt;
        if *remaining > 0.0 {
            return;
        }
        s.flash = None;
        s.step += 1;
        if let Some(next) = s.drink.steps.get(s.step) {
            s.state = MinigameState::new(&next.game, &mut rand::rng());
            return;
        }
        let (quality, perfect) = s.drink.quality(&s.scores);
        commands
            .entity(s.machine)
            .try_remove::<MachineInUse>()
            .try_insert(Brewing::finished(s.drink.name, quality, perfect));
        session.0 = None;
        active.close();
        return;
    }

    // Clicks aimed at the overlay's buttons aren't minigame input.
    let over_button = buttons_hover.iter().any(|h| h.get());
    let action = [MouseButton::Left];
    // bevy measures from the node's centre, y down; games want 0..1, y up.
    let cursor = surface
        .iter()
        .find_map(|c| c.normalized)
        .map(|n| Vec2::new(n.x + 0.5, 0.5 - n.y));
    let confirm = !confirm_pressed.is_empty()
        || (s.state.confirm_label().is_some() && keys.just_pressed(KeyCode::Enter));
    let input = if over_button {
        MinigameInput {
            confirm,
            dt,
            ..default()
        }
    } else {
        MinigameInput {
            held: mouse.any_pressed(action) || keys.pressed(KeyCode::Space),
            pressed: mouse.any_just_pressed(action) || keys.just_pressed(KeyCode::Space),
            cursor,
            confirm,
            dt,
        }
    };

    let game = &s.drink.steps[s.step].game;
    if let Outcome::Done { score } = s.state.update(game, &input, &mut rand::rng()) {
        s.scores.push(score);
        s.flash = Some(FLASH_SECS);
    }
}

/// The menu closed without the drink finishing: free the machine, drop it.
pub fn end_brew_session_when_closed(
    mut commands: Commands,
    active: Res<ActiveMenu>,
    mut session: ResMut<BrewSession>,
) {
    if active.is(ActiveMenu::Brew) {
        return;
    }
    if let Some(s) = session.0.take() {
        commands.entity(s.machine).try_remove::<MachineInUse>();
    }
}

/// Build the overlay for the current step (rebuilding when the step changes),
/// tear it down when the session ends, and move the meters every frame.
pub fn sync_brew_overlay(
    mut commands: Commands,
    session: Res<BrewSession>,
    roots: Query<(Entity, &BrewOverlayRoot)>,
    mut parts: Query<(&mut Node, &MeterPart)>,
    mut result_text: Query<&mut Text, With<StepResultText>>,
) {
    let Some(s) = session.0.as_ref() else {
        for (e, _) in &roots {
            commands.entity(e).despawn();
        }
        return;
    };

    if !roots.iter().any(|(_, r)| r.step == s.step) {
        for (e, _) in &roots {
            commands.entity(e).despawn();
        }
        spawn_overlay(&mut commands, s);
        return; // new nodes are queryable next frame
    }

    let readings = s.state.readings();
    let pct = |v: f32| Val::Percent(v.clamp(0.0, 1.0) * 100.0);
    for (mut node, part) in &mut parts {
        let Some(&value) = readings.get(part.index) else {
            continue;
        };
        match part.kind {
            Part::Needle { vertical: true } => node.bottom = pct(value),
            Part::Needle { vertical: false } => node.left = pct(value),
            Part::Fill { vertical: true } => node.height = pct(value),
            Part::Fill { vertical: false } => node.width = pct(value),
            Part::Marker { path } => {
                let at = point_along(path, value);
                node.left = pct(at.x);
                node.bottom = pct(at.y);
            }
        }
    }
    let result = match s.flash {
        Some(_) => s.scores.last().map(|&v| verdict(v)).unwrap_or_default(),
        None => String::new(),
    };
    for mut text in &mut result_text {
        if **text != result {
            **text = result.clone();
        }
    }
}

fn verdict(score: f32) -> String {
    let word = match score {
        s if s >= PERFECT_STEP => "Perfect!",
        s if s >= 0.7 => "Good",
        s if s >= 0.4 => "Okay",
        _ => "Miss",
    };
    format!("{word}  {:.0}%", score * 100.0)
}

// ----------------------------------------------------------------------------
// Overlay
// ----------------------------------------------------------------------------

fn spawn_overlay(commands: &mut Commands, s: &Session) {
    let step = &s.drink.steps[s.step];
    let text = Color::srgb(0.95, 0.95, 0.95);
    let dim = Color::srgb(0.65, 0.70, 0.78);

    let root = commands
        .spawn_owned((
            BrewOverlayRoot { step: s.step },
            Node {
                position_type: PositionType::Absolute,
                width: Val::Px(PANEL_WIDTH),
                left: Val::Percent(50.0),
                top: Val::Percent(50.0),
                margin: UiRect {
                    left: Val::Px(-PANEL_WIDTH * 0.5),
                    top: Val::Px(-240.0),
                    ..default()
                },
                padding: UiRect::all(Val::Px(18.0)),
                border: UiRect::all(Val::Px(2.0)),
                border_radius: BorderRadius::all(Val::Px(8.0)),
                flex_direction: FlexDirection::Column,
                align_items: AlignItems::Center,
                row_gap: Val::Px(8.0),
                ..default()
            },
            BorderColor::all(Color::srgb(0.25, 0.30, 0.38)),
            BackgroundColor(Color::srgba(0.06, 0.07, 0.10, 0.94)),
            GlobalZIndex(100),
        ))
        .id();

    let title = label(commands, s.drink.name.to_string(), 24.0, text);
    let dots = step_dots(commands, s.step, s.drink.steps.len());
    let subtitle = label(
        commands,
        format!(
            "Step {}/{}: {}",
            s.step + 1,
            s.drink.steps.len(),
            step.label
        ),
        16.0,
        Color::srgb(1.0, 0.85, 0.55),
    );
    let hint = label(commands, step.hint.to_string(), 14.0, dim);

    let meters = commands
        .spawn_owned(Node {
            flex_direction: FlexDirection::Row,
            align_items: AlignItems::End,
            column_gap: Val::Px(28.0),
            margin: UiRect::vertical(Val::Px(10.0)),
            ..default()
        })
        .id();
    for (index, layout) in s.state.meters(&step.game).iter().enumerate() {
        let meter = spawn_meter(commands, index, layout, dim);
        commands.entity(meters).add_child(meter);
    }

    let result = commands
        .spawn_owned((
            StepResultText,
            Text::new(""),
            TextFont {
                font_size: FontSize::Px(20.0),
                ..default()
            },
            TextColor(GOLD),
            Node {
                min_height: Val::Px(26.0),
                ..default()
            },
        ))
        .id();

    let buttons = commands
        .spawn_owned(Node {
            flex_direction: FlexDirection::Row,
            column_gap: Val::Px(12.0),
            ..default()
        })
        .id();
    if let Some(confirm) = s.state.confirm_label() {
        let done = spawn_button(
            commands,
            ConfirmStepButton,
            &format!("{confirm} (Enter)"),
            Color::srgb(0.15, 0.32, 0.20),
            Color::srgb(0.28, 0.60, 0.36),
            text,
        );
        commands.entity(buttons).add_child(done);
    }
    let cancel = spawn_button(
        commands,
        CancelBrewButton,
        "Throw away",
        Color::srgb(0.35, 0.15, 0.15),
        Color::srgb(0.6, 0.25, 0.25),
        text,
    );
    commands.entity(buttons).add_child(cancel);

    commands
        .entity(root)
        .add_children(&[title, dots, subtitle, hint, meters, result, buttons]);
}

/// A row of round dots: filled for steps done or current, hollow for the rest.
fn step_dots(commands: &mut Commands, current: usize, total: usize) -> Entity {
    let row = commands
        .spawn_owned(Node {
            flex_direction: FlexDirection::Row,
            column_gap: Val::Px(6.0),
            ..default()
        })
        .id();
    for i in 0..total {
        let fill = if i <= current { GOLD } else { Color::NONE };
        let dot = commands
            .spawn_owned((
                Node {
                    width: Val::Px(STEP_DOT_PX),
                    height: Val::Px(STEP_DOT_PX),
                    border: UiRect::all(Val::Px(2.0)),
                    border_radius: BorderRadius::MAX,
                    ..default()
                },
                BorderColor::all(GOLD),
                BackgroundColor(fill),
            ))
            .id();
        commands.entity(row).add_child(dot);
    }
    row
}

fn label(commands: &mut Commands, s: String, size: f32, color: Color) -> Entity {
    commands
        .spawn_owned((
            Text::new(s),
            TextFont {
                font_size: FontSize::Px(size),
                ..default()
            },
            TextColor(color),
        ))
        .id()
}

/// A meter of any [`MeterShape`], with its caption underneath.
fn spawn_meter(commands: &mut Commands, index: usize, layout: &MeterLayout, dim: Color) -> Entity {
    let (w, h, radius) = match layout.shape {
        MeterShape::Bar { vertical: true, .. } => (METER_THICK, METER_LONG, 4.0),
        MeterShape::Bar {
            vertical: false, ..
        } => (METER_LONG, METER_THICK, 4.0),
        MeterShape::Dish => (DISH_SIZE, DISH_SIZE, DISH_SIZE * 0.5),
        MeterShape::Canvas { .. } => (CANVAS_SIZE, CANVAS_SIZE, 8.0),
    };
    let track = commands
        .spawn_owned((
            Node {
                width: Val::Px(w),
                height: Val::Px(h),
                border: UiRect::all(Val::Px(2.0)),
                border_radius: BorderRadius::all(Val::Px(radius)),
                overflow: Overflow::clip(),
                ..default()
            },
            BorderColor::all(Color::srgb(0.35, 0.40, 0.48)),
            BackgroundColor(Color::srgb(0.13, 0.15, 0.19)),
        ))
        .id();
    if layout.cursor {
        commands
            .entity(track)
            .insert((CursorSurface, RelativeCursorPosition::default()));
    }

    let mut children = Vec::new();
    match layout.shape {
        MeterShape::Bar { vertical, fill } => {
            bar_parts(commands, index, layout, vertical, fill, &mut children)
        }
        // Nothing moves on a dish; the OS cursor is the whisk.
        MeterShape::Dish => {}
        MeterShape::Canvas { path } => canvas_parts(commands, index, path, &mut children),
    }
    commands.entity(track).add_children(&children);

    let caption = label(commands, layout.caption.to_string(), 13.0, dim);
    commands
        .spawn_owned(Node {
            flex_direction: FlexDirection::Column,
            align_items: AlignItems::Center,
            row_gap: Val::Px(4.0),
            ..default()
        })
        .add_children(&[track, caption])
        .id()
}

/// Zones, then the fill, then the needle on top (children draw in order).
fn bar_parts(
    commands: &mut Commands,
    index: usize,
    layout: &MeterLayout,
    vertical: bool,
    fill: bool,
    out: &mut Vec<Entity>,
) {
    // A span along the bar's axis.
    let span = |from: f32, len: Val| {
        let mut node = Node {
            position_type: PositionType::Absolute,
            ..default()
        };
        if vertical {
            node.width = Val::Percent(100.0);
            node.bottom = Val::Percent(from * 100.0);
            node.height = len;
        } else {
            node.height = Val::Percent(100.0);
            node.left = Val::Percent(from * 100.0);
            node.width = len;
        }
        node
    };

    for &(from, to, kind) in &layout.zones {
        let color = match kind {
            MeterZone::Good => GOOD,
            MeterZone::Bad => BAD,
        };
        let node = span(from, Val::Percent((to - from) * 100.0));
        out.push(commands.spawn_owned((node, BackgroundColor(color))).id());
    }
    if fill {
        out.push(
            commands
                .spawn_owned((
                    span(0.0, Val::Px(0.0)),
                    BackgroundColor(Color::srgba(0.75, 0.82, 0.95, 0.35)),
                    MeterPart {
                        index,
                        kind: Part::Fill { vertical },
                    },
                ))
                .id(),
        );
    }
    let mut needle = span(0.0, Val::Px(NEEDLE_PX));
    // Centre the needle on its value.
    if vertical {
        needle.margin.bottom = Val::Px(-NEEDLE_PX * 0.5);
    } else {
        needle.margin.left = Val::Px(-NEEDLE_PX * 0.5);
    }
    out.push(
        commands
            .spawn_owned((
                needle,
                BackgroundColor(Color::WHITE),
                MeterPart {
                    index,
                    kind: Part::Needle { vertical },
                },
            ))
            .id(),
    );
}

/// Dots along the path (the first one gold: start here), then the marker.
fn canvas_parts(
    commands: &mut Commands,
    index: usize,
    path: &'static [Vec2],
    out: &mut Vec<Entity>,
) {
    let dot = |at: Vec2, size: f32| Node {
        position_type: PositionType::Absolute,
        left: Val::Percent(at.x * 100.0),
        bottom: Val::Percent(at.y * 100.0),
        width: Val::Px(size),
        height: Val::Px(size),
        margin: UiRect {
            left: Val::Px(-size * 0.5),
            bottom: Val::Px(-size * 0.5),
            ..default()
        },
        border_radius: BorderRadius::MAX,
        ..default()
    };
    for i in 0..=CANVAS_DOTS {
        let at = point_along(path, i as f32 / CANVAS_DOTS as f32);
        let (size, color) = if i == 0 {
            (MARKER_PX, GOLD)
        } else {
            (PATH_DOT_PX, Color::srgba(0.9, 0.85, 0.75, 0.45))
        };
        out.push(
            commands
                .spawn_owned((dot(at, size), BackgroundColor(color)))
                .id(),
        );
    }
    out.push(
        commands
            .spawn_owned((
                // Smaller than the start dot, so a gold ring shows round it.
                dot(path[0], MARKER_PX - 6.0),
                BackgroundColor(Color::WHITE),
                MeterPart {
                    index,
                    kind: Part::Marker { path },
                },
            ))
            .id(),
    );
}
