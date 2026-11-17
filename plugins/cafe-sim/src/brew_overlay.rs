//! The brew overlay: what a [`BrewSession`] looks like.
//!
//! `drink_station.rs` runs the session; this draws it. The overlay is rebuilt
//! when the step changes and otherwise only its moving parts are touched
//! ([`MeterPart`]s — needles, fills, markers, the trail — and the two text
//! lines), because bevy_ui is retained and every write re-extracts a node.
//!
//! Every meter is dressed as the thing it stands for in the cafe
//! ([`Prop`]: a gauge on the machine, the cup, a thermometer, a milk jug, a
//! bowl, the crema in a cup seen from above) out of plain nodes — borders,
//! radii, gradients and shadows; no images — so the games' 0..1 geometry
//! stays exactly where [`MeterLayout`] says it is. The look is one palette
//! ([`look`]): dark roast panel, caramel borders, cream text.
//!
//! All text is ASCII unless a font is set ([`crate::ui::FONT_PATH`]).

use std::f32::consts::TAU;

use bevy::{
    color::Mix,
    math::Rot2,
    picking::hover::Hovered,
    prelude::*,
    ui::{RelativeCursorPosition, UiTransform, widget::TextShadow},
    ui_widgets::Button,
};

use crate::drink_station::{
    BrewSession, CancelBrewButton, ConfirmStepButton, CursorSurface, Session,
};
use crate::drinks::PERFECT_STEP;
use crate::minigames::{Grade, MeterLayout, MeterShape, MeterZone, Prop, point_along};
use crate::spawn::{SpawnOwnedCommands, WithChildOwned};
use crate::ui::{InteractiveStyle, UiTheme};

const PANEL_WIDTH: f32 = 500.0;
const METER_LONG: f32 = 210.0;
const METER_THICK: f32 = 48.0;
const NEEDLE_PX: f32 = 4.0;
const DISH_SIZE: f32 = 190.0;
/// A cup is wider than it is tall-ish; the bar's 0..1 still spans its height.
const MUG_W: f32 = 104.0;
const MUG_H: f32 = 132.0;
const JUG_W: f32 = 112.0;
const JUG_H: f32 = 190.0;
/// How high the milk sits in the steaming jug before any froth.
const MILK_LEVEL: f32 = 0.42;
/// Foam pips under the steaming jug.
const FOAM_PIPS: usize = 5;
const CANVAS_SIZE: f32 = 230.0;
/// Dots drawn along a canvas path, evenly spaced by arc length.
const CANVAS_DOTS: usize = 48;
const PATH_DOT_PX: f32 = 6.0;
const MARKER_PX: f32 = 16.0;
const STEP_DOT_PX: f32 = 12.0;
/// Dots kept ready on a canvas for the player's trail (`TraceState`'s cap).
const TRAIL_SLOTS: usize = 240;
const TRAIL_DOT_PX: f32 = 6.0;

/// The cafe palette.
mod look {
    use bevy::color::Color;

    /// Panel body: a warm mid brown, lighter than the room behind it.
    pub const ESPRESSO: Color = Color::srgb(0.33, 0.24, 0.17);
    /// Plates and plain tracks inset into the panel.
    pub const ROAST: Color = Color::srgb(0.22, 0.15, 0.10);
    /// Borders and the dim rule lines.
    pub const CARAMEL: Color = Color::srgb(0.80, 0.56, 0.30);
    /// Headline text and ceramic.
    pub const CREAM: Color = Color::srgb(0.97, 0.92, 0.82);
    /// Secondary text.
    pub const LATTE: Color = Color::srgb(0.86, 0.74, 0.58);
    /// Brewed coffee.
    pub const COFFEE: Color = Color::srgb(0.40, 0.24, 0.13);
    /// The crema on top of a shot.
    pub const CREMA: Color = Color::srgb(0.78, 0.57, 0.35);
    /// Milk.
    pub const MILK: Color = Color::srgb(0.98, 0.97, 0.93);
    /// Milk still in the jug, under the froth: creamier than the froth on it.
    pub const MILK_BODY: Color = Color::srgb(0.88, 0.82, 0.68);
    /// Froth that has not been worked yet: thin, grey and watery.
    pub const FROTH_THIN: Color = Color::srgb(0.72, 0.70, 0.66);
    /// A stream of coffee being poured.
    pub const STREAM: Color = Color::srgb(0.52, 0.33, 0.18);
    /// Brushed steel, light and dark.
    pub const STEEL: Color = Color::srgb(0.76, 0.78, 0.80);
    pub const STEEL_DARK: Color = Color::srgb(0.42, 0.44, 0.48);
    /// Target zones.
    pub const GOOD: Color = Color::srgb(0.47, 0.72, 0.38);
    pub const BAD: Color = Color::srgb(0.80, 0.30, 0.24);
    /// Highlights: step dots, the verdict, the start of a trace.
    pub const GOLD: Color = Color::srgb(0.98, 0.78, 0.33);
    /// Shadows and the darkest borders.
    pub const INK: Color = Color::srgb(0.10, 0.06, 0.04);
}
use look::*;

// ----------------------------------------------------------------------------
// Markers
// ----------------------------------------------------------------------------

/// The overlay, tagged with the step it was built for so a new step rebuilds it.
#[derive(Component)]
pub struct BrewOverlayRoot {
    step: usize,
}

/// A node that follows meter `index`'s reading (in
/// [`crate::minigames::MinigameState::meters`] order).
#[derive(Component)]
pub struct MeterPart {
    index: usize,
    kind: Part,
}

#[derive(Clone, Copy)]
enum Part {
    /// Tilts with the reading's `extra` — the pouring jug.
    Tilt { degrees: f32 },
    /// A stream of liquid: thickness and opacity from `extra`, with a wobble
    /// of its own so a steady pour still looks alive. Hidden at no flow.
    Stream { base_px: f32 },
    /// Turns at a rate set by the reading — the liquid going round the bowl.
    Swirl { turns_per_sec: f32, angle: f32 },
    /// The froth standing above the milk: `value` is its top, `extra` how
    /// dense (and so how white) it has been worked.
    Froth { from: f32 },
    /// One pip of an `extra`-driven row, lit once the reading passes it.
    Pip { index: usize, of: usize },
    /// Froth going over the rim, once `value` reaches `at`.
    Spill { at: f32 },
    /// Positioned at the reading along a bar.
    Needle { vertical: bool },
    /// Sized from 0 up to the reading.
    Fill { vertical: bool },
    /// Positioned at the reading's fraction along a canvas path.
    Marker { path: &'static [Vec2] },
    /// The reading's moving good zone along a bar; empty when it has none.
    Zone { vertical: bool },
    /// One dot of what the player has drawn
    /// ([`crate::minigames::MinigameState::trail`]); hidden until the trail
    /// reaches it.
    Trail { slot: usize },
}

#[derive(Component)]
pub struct StepResultText;

/// The game's live feedback line ([`crate::minigames::MinigameState::status`]).
#[derive(Component)]
pub struct StepStatusText;

// ----------------------------------------------------------------------------
// System
// ----------------------------------------------------------------------------

/// Build the overlay for the current step (rebuilding when the step changes),
/// tear it down when the session ends, and move the meters every frame.
pub fn sync_brew_overlay(
    mut commands: Commands,
    session: Res<BrewSession>,
    theme: Res<UiTheme>,
    time: Res<Time>,
    roots: Query<(Entity, &BrewOverlayRoot)>,
    mut parts: Query<(
        &mut Node,
        &mut BackgroundColor,
        &mut MeterPart,
        Option<&mut UiTransform>,
    )>,
    mut result_text: Query<&mut Text, (With<StepResultText>, Without<StepStatusText>)>,
    mut status_text: Query<&mut Text, With<StepStatusText>>,
) {
    let Some(s) = session.current() else {
        for (e, _) in &roots {
            commands.entity(e).despawn();
        }
        return;
    };

    if !roots.iter().any(|(_, r)| r.step == s.step) {
        for (e, _) in &roots {
            commands.entity(e).despawn();
        }
        spawn_overlay(&mut commands, &theme, s);
        return; // new nodes are queryable next frame
    }

    let game = &s.drink.steps[s.step].game;
    let readings = s.state.readings(game);
    let trail = s.state.trail();
    let pct = |v: f32| Val::Percent(v.clamp(0.0, 1.0) * 100.0);
    let (now, dt) = (time.elapsed_secs(), time.delta_secs());
    // Two detuned sines: a pour that never repeats itself exactly.
    let wobble = 0.6 * (now * 11.0).sin() + 0.4 * (now * 4.3).sin();
    for (mut node, mut color, mut part, mut transform) in &mut parts {
        if let Part::Trail { slot } = part.kind {
            match trail.get(slot) {
                Some(point) => {
                    node.display = Display::Flex;
                    node.left = pct(point.at.x);
                    node.bottom = pct(point.at.y);
                    color.0 = milk_drop(point.grade);
                }
                None => node.display = Display::None,
            }
            continue;
        }
        let Some(reading) = readings.get(part.index) else {
            continue;
        };
        let value = reading.value;
        let tint = |color: &mut BackgroundColor, want: Color| {
            if color.0 != want {
                color.0 = want;
            }
        };
        match &mut part.kind {
            Part::Needle { vertical: true } => node.bottom = pct(value),
            Part::Needle { vertical: false } => node.left = pct(value),
            Part::Fill { vertical: true } => node.height = pct(value),
            Part::Fill { vertical: false } => node.width = pct(value),
            Part::Tilt { degrees } => {
                if let Some(transform) = transform.as_mut() {
                    transform.rotation = Rot2::degrees(*degrees * reading.extra);
                }
            }
            Part::Stream { base_px } => {
                let flow = reading.extra;
                if flow <= 0.02 {
                    node.display = Display::None;
                } else {
                    // Thickens as the stream builds, never perfectly steady.
                    let width = *base_px * (0.45 + 0.55 * flow) * (1.0 + 0.16 * wobble);
                    node.display = Display::Flex;
                    node.width = Val::Px(width);
                    node.margin.left = Val::Px(-width * 0.5 + 2.0 * wobble);
                    tint(&mut color, STREAM.with_alpha((0.45 + 0.55 * flow).min(1.0)));
                }
            }
            Part::Swirl {
                turns_per_sec,
                angle,
            } => {
                *angle += value * *turns_per_sec * TAU * dt;
                if let Some(transform) = transform.as_mut() {
                    transform.rotation = Rot2::radians(*angle);
                }
            }
            Part::Froth { from } => {
                node.height = pct(value - *from);
                // Thin and translucent at first, stiff and white when worked.
                tint(&mut color, FROTH_THIN.mix(&MILK, reading.extra));
            }
            Part::Pip { index, of } => {
                let lit = reading.extra * *of as f32 >= *index as f32 + 1.0;
                tint(&mut color, if lit { GOLD } else { Color::NONE });
            }
            Part::Spill { at } => {
                let over = (value - *at + 0.06) / 0.06;
                if over <= 0.0 {
                    node.display = Display::None;
                } else {
                    node.display = Display::Flex;
                    node.height = Val::Px(6.0 + 16.0 * over.min(1.0));
                }
            }
            Part::Marker { path } => {
                let path = *path;
                let at = point_along(path, value);
                node.left = pct(at.x);
                node.bottom = pct(at.y);
            }
            Part::Zone { vertical } => {
                let vertical = *vertical;
                let (start, extent) = match reading.zone {
                    Some((from, to)) => (pct(from), pct(to - from)),
                    None => (Val::Percent(0.0), Val::Percent(0.0)),
                };
                if vertical {
                    node.bottom = start;
                    node.height = extent;
                } else {
                    node.left = start;
                    node.width = extent;
                }
            }
            Part::Trail { .. } => unreachable!("handled above"),
        }
    }
    let status = s.state.status(game).unwrap_or_default();
    for mut text in &mut status_text {
        if **text != status {
            **text = status.clone();
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

/// A poured drop of milk on the crema: white on the line, off-white near it,
/// a brown smear where it went wrong.
fn milk_drop(grade: Grade) -> Color {
    match grade {
        Grade::Good => MILK,
        Grade::Off => Color::srgb(0.90, 0.82, 0.66),
        Grade::Bad => Color::srgb(0.50, 0.24, 0.16),
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
// Panel
// ----------------------------------------------------------------------------

fn spawn_overlay(commands: &mut Commands, theme: &UiTheme, s: &Session) {
    let step = &s.drink.steps[s.step];

    // A full-screen flex box centres the panel whatever its height.
    let root = commands
        .spawn_owned((
            BrewOverlayRoot { step: s.step },
            Node {
                position_type: PositionType::Absolute,
                width: Val::Percent(100.0),
                height: Val::Percent(100.0),
                justify_content: JustifyContent::Center,
                align_items: AlignItems::Center,
                ..default()
            },
            GlobalZIndex(100),
        ))
        .id();
    let panel = commands
        .spawn_owned((
            Node {
                width: Val::Px(PANEL_WIDTH),
                padding: UiRect::axes(Val::Px(18.0), Val::Px(16.0)),
                border: UiRect::all(Val::Px(4.0)),
                border_radius: BorderRadius::all(Val::Px(18.0)),
                flex_direction: FlexDirection::Column,
                align_items: AlignItems::Center,
                row_gap: Val::Px(10.0),
                ..default()
            },
            BorderColor::all(CARAMEL),
            BackgroundColor(ESPRESSO),
            BoxShadow::new(
                INK.with_alpha(0.65),
                Val::Px(0.0),
                Val::Px(10.0),
                Val::Px(0.0),
                Val::Px(28.0),
            ),
        ))
        .id();
    commands.entity(root).add_child(panel);

    let header = header_plate(commands, theme, s);
    let hint = commands
        .spawn_owned((
            Text::new(step.hint),
            theme.text(14.0),
            TextColor(LATTE),
            TextLayout::new(Justify::Center, LineBreak::WordBoundary),
            Node {
                max_width: Val::Px(PANEL_WIDTH - 60.0),
                ..default()
            },
        ))
        .id();

    let meters = commands
        .spawn_owned(Node {
            flex_direction: FlexDirection::Row,
            align_items: AlignItems::End,
            column_gap: Val::Px(30.0),
            margin: UiRect::vertical(Val::Px(8.0)),
            ..default()
        })
        .id();
    for (index, layout) in s.state.meters(&step.game).iter().enumerate() {
        let meter = spawn_meter(commands, theme, index, layout);
        commands.entity(meters).add_child(meter);
    }

    let status = commands
        .spawn_owned((
            StepStatusText,
            Text::new(""),
            theme.text(15.0),
            TextColor(CREAM),
            Node {
                min_height: Val::Px(20.0),
                ..default()
            },
        ))
        .id();

    let result = commands
        .spawn_owned((
            StepResultText,
            Text::new(""),
            theme.text(24.0),
            TextColor(GOLD),
            TextShadow {
                offset: Vec2::splat(2.0),
                color: INK,
            },
            Node {
                min_height: Val::Px(30.0),
                ..default()
            },
        ))
        .id();

    let buttons = commands
        .spawn_owned(Node {
            flex_direction: FlexDirection::Row,
            column_gap: Val::Px(14.0),
            margin: UiRect::top(Val::Px(4.0)),
            ..default()
        })
        .id();
    if let Some(confirm) = s.state.confirm_label() {
        let done = chunky_button(
            commands,
            theme,
            ConfirmStepButton,
            &format!("{confirm} (Enter)"),
            Color::srgb(0.22, 0.40, 0.22),
            Color::srgb(0.32, 0.56, 0.30),
        );
        commands.entity(buttons).add_child(done);
    }
    let cancel = chunky_button(
        commands,
        theme,
        CancelBrewButton,
        "Throw away",
        Color::srgb(0.42, 0.18, 0.14),
        Color::srgb(0.60, 0.26, 0.20),
    );
    commands.entity(buttons).add_child(cancel);

    commands
        .entity(panel)
        .add_children(&[header, hint, meters, status, result, buttons]);
}

/// The menu-board strip at the top: drink name, step dots, step title.
fn header_plate(commands: &mut Commands, theme: &UiTheme, s: &Session) -> Entity {
    let step = &s.drink.steps[s.step];
    let plate = commands
        .spawn_owned((
            Node {
                width: Val::Percent(100.0),
                padding: UiRect::axes(Val::Px(14.0), Val::Px(10.0)),
                border: UiRect::all(Val::Px(2.0)),
                border_radius: BorderRadius::all(Val::Px(10.0)),
                flex_direction: FlexDirection::Column,
                align_items: AlignItems::Center,
                row_gap: Val::Px(6.0),
                ..default()
            },
            BorderColor::all(CARAMEL.with_alpha(0.45)),
            BackgroundColor(ROAST),
        ))
        .id();
    let title = commands
        .spawn_owned((
            Text::new(s.drink.name),
            theme.text(30.0),
            TextColor(CREAM),
            TextShadow {
                offset: Vec2::splat(2.0),
                color: INK,
            },
        ))
        .id();
    let dots = step_dots(commands, s.step, s.drink.steps.len());
    let subtitle = commands
        .spawn_owned((
            Text::new(format!(
                "Step {}/{}  -  {}",
                s.step + 1,
                s.drink.steps.len(),
                step.label
            )),
            theme.text(16.0),
            TextColor(GOLD),
        ))
        .id();
    commands
        .entity(plate)
        .add_children(&[title, dots, subtitle]);
    plate
}

/// A row of beans: filled for steps done or current, hollow for the rest.
fn step_dots(commands: &mut Commands, current: usize, total: usize) -> Entity {
    let row = commands
        .spawn_owned(Node {
            flex_direction: FlexDirection::Row,
            column_gap: Val::Px(7.0),
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

/// A big-bordered button in the panel's language. Hover and press colours
/// come from [`InteractiveStyle`], which `update_interactive_visuals` applies.
fn chunky_button(
    commands: &mut Commands,
    theme: &UiTheme,
    marker: impl Bundle,
    label: &str,
    bg: Color,
    hover_bg: Color,
) -> Entity {
    let style = InteractiveStyle {
        idle_bg: bg,
        hover_bg,
        pressed_bg: bg.darker(0.05),
        idle_border: CARAMEL,
        hover_border: CREAM,
        pressed_border: GOLD,
    };
    commands
        .spawn_owned((
            marker,
            Button,
            Hovered::default(),
            style,
            Node {
                padding: UiRect::axes(Val::Px(18.0), Val::Px(10.0)),
                border: UiRect::all(Val::Px(3.0)),
                border_radius: BorderRadius::all(Val::Px(10.0)),
                justify_content: JustifyContent::Center,
                align_items: AlignItems::Center,
                ..default()
            },
            BorderColor::all(style.idle_border),
            BackgroundColor(style.idle_bg),
            BoxShadow::new(
                INK.with_alpha(0.5),
                Val::Px(0.0),
                Val::Px(3.0),
                Val::Px(0.0),
                Val::Px(6.0),
            ),
        ))
        .with_child_owned((
            Text::new(label.to_string()),
            theme.text(16.0),
            TextColor(CREAM),
            TextShadow {
                offset: Vec2::splat(1.5),
                color: INK,
            },
        ))
        .id()
}

// ----------------------------------------------------------------------------
// Meters
// ----------------------------------------------------------------------------

/// How a [`Prop`] is dressed: the track's own look plus the colours of what
/// moves inside it. Geometry (where 0 and 1 are) is the track node itself,
/// so decorations hang outside it and never shift a reading.
struct Dress {
    border_px: f32,
    border: Color,
    /// Flat background, drawn under `gradient` if both are set.
    background: Color,
    gradient: Option<Gradient>,
    radius: BorderRadius,
    fill: Color,
    /// A second fill colour makes the fill a gradient (`fill` at the far end).
    fill_from: Option<Color>,
    needle: Color,
}

fn dress(prop: Prop, shape: MeterShape) -> Dress {
    let vertical = !matches!(
        shape,
        MeterShape::Bar {
            vertical: false,
            ..
        }
    );
    // Along the bar: the direction a fill grows.
    let along = if vertical {
        LinearGradient::TO_TOP
    } else {
        LinearGradient::TO_RIGHT
    };
    // Across the bar: light catching a cylinder.
    let across = if vertical {
        LinearGradient::TO_RIGHT
    } else {
        LinearGradient::TO_BOTTOM
    };
    let sheen = |dark: Color, light: Color| {
        Some(Gradient::Linear(LinearGradient::new(
            across,
            vec![
                ColorStop::new(dark, Val::Percent(0.0)),
                ColorStop::new(light, Val::Percent(35.0)),
                ColorStop::new(dark, Val::Percent(100.0)),
            ],
        )))
    };
    let plain = Dress {
        border_px: 3.0,
        border: INK,
        background: ROAST,
        gradient: None,
        radius: BorderRadius::all(Val::Px(8.0)),
        fill: CREMA,
        fill_from: None,
        needle: CREAM,
    };
    match prop {
        Prop::Plain => plain,
        Prop::Gauge => Dress {
            border_px: 4.0,
            border: STEEL_DARK,
            background: STEEL,
            gradient: sheen(STEEL_DARK, STEEL),
            radius: BorderRadius::all(Val::Px(12.0)),
            fill: GOLD.with_alpha(0.55),
            fill_from: Some(CREMA.with_alpha(0.35)),
            needle: BAD,
        },
        Prop::Mug => Dress {
            border_px: 4.0,
            background: CREAM,
            radius: BorderRadius::px(6.0, 6.0, 30.0, 30.0),
            fill: CREMA,
            fill_from: Some(COFFEE),
            needle: COFFEE,
            ..plain
        },
        Prop::Scoop => Dress {
            border: STEEL_DARK,
            background: STEEL,
            gradient: sheen(STEEL_DARK, STEEL),
            radius: BorderRadius::px(6.0, 6.0, 24.0, 24.0),
            fill: Color::srgb(0.30, 0.18, 0.10),
            needle: Color::NONE,
            ..plain
        },
        // The cup being poured into: a mug, with the jug above it.
        Prop::PourCup => Dress {
            border_px: 4.0,
            background: CREAM,
            radius: BorderRadius::px(6.0, 6.0, 30.0, 30.0),
            fill: CREMA,
            fill_from: Some(COFFEE),
            needle: COFFEE,
            ..plain
        },
        // The steaming jug: an empty steel body the milk and froth sit in.
        Prop::FrothJug => Dress {
            border_px: 4.0,
            border: STEEL_DARK,
            background: Color::srgb(0.28, 0.30, 0.33),
            gradient: sheen(Color::srgb(0.24, 0.26, 0.29), Color::srgb(0.38, 0.41, 0.45)),
            radius: BorderRadius::px(6.0, 6.0, 18.0, 18.0),
            fill: MILK,
            fill_from: None,
            needle: MILK,
        },
        Prop::Jug => Dress {
            border: STEEL_DARK,
            background: STEEL,
            gradient: sheen(STEEL_DARK, STEEL),
            radius: if vertical {
                BorderRadius::px(4.0, 4.0, 12.0, 12.0)
            } else {
                BorderRadius::all(Val::Px(10.0))
            },
            fill: MILK,
            needle: STEEL_DARK,
            ..plain
        },
        Prop::Strip => Dress {
            background: COFFEE,
            gradient: Some(Gradient::Linear(LinearGradient::new(
                along,
                vec![
                    ColorStop::new(Color::srgb(0.95, 0.86, 0.66), Val::Percent(0.0)),
                    ColorStop::new(CREMA, Val::Percent(50.0)),
                    ColorStop::new(COFFEE, Val::Percent(100.0)),
                ],
            ))),
            radius: BorderRadius::all(Val::Px(10.0)),
            needle: CREAM,
            ..plain
        },
        Prop::Bowl => Dress {
            border_px: 7.0,
            border: CREAM,
            background: COFFEE,
            gradient: Some(Gradient::Radial(RadialGradient::new(
                UiPosition::CENTER,
                RadialGradientShape::ClosestSide,
                vec![
                    ColorStop::new(Color::srgb(0.58, 0.36, 0.20), Val::Percent(0.0)),
                    ColorStop::new(Color::srgb(0.40, 0.23, 0.12), Val::Percent(70.0)),
                    ColorStop::new(Color::srgb(0.26, 0.14, 0.08), Val::Percent(100.0)),
                ],
            ))),
            radius: BorderRadius::MAX,
            ..plain
        },
        Prop::LatteCup => Dress {
            border_px: 9.0,
            border: CREAM,
            background: CREMA,
            gradient: Some(Gradient::Radial(RadialGradient::new(
                UiPosition::CENTER,
                RadialGradientShape::ClosestSide,
                vec![
                    ColorStop::new(Color::srgb(0.86, 0.66, 0.42), Val::Percent(0.0)),
                    ColorStop::new(CREMA, Val::Percent(60.0)),
                    ColorStop::new(Color::srgb(0.58, 0.38, 0.22), Val::Percent(100.0)),
                ],
            ))),
            radius: BorderRadius::MAX,
            fill: plain.fill,
            fill_from: None,
            needle: plain.needle,
        },
    }
}

/// A meter of any shape and prop, with its caption underneath.
fn spawn_meter(
    commands: &mut Commands,
    theme: &UiTheme,
    index: usize,
    layout: &MeterLayout,
) -> Entity {
    let (w, h) = match (layout.shape, layout.prop) {
        (MeterShape::Bar { vertical: true, .. }, Prop::Scoop) => (METER_THICK, 120.0),
        (MeterShape::Bar { vertical: true, .. }, Prop::Mug | Prop::PourCup) => (MUG_W, MUG_H),
        (MeterShape::Bar { vertical: true, .. }, Prop::FrothJug) => (JUG_W, JUG_H),
        (MeterShape::Bar { vertical: true, .. }, _) => (METER_THICK, METER_LONG),
        (
            MeterShape::Bar {
                vertical: false, ..
            },
            _,
        ) => (METER_LONG, METER_THICK),
        (MeterShape::Dish, _) => (DISH_SIZE, DISH_SIZE),
        (MeterShape::Canvas { .. }, _) => (CANVAS_SIZE, CANVAS_SIZE),
    };
    let dress = dress(layout.prop, layout.shape);

    let mut track_node = Node {
        width: Val::Px(w),
        height: Val::Px(h),
        border: UiRect::all(Val::Px(dress.border_px)),
        border_radius: dress.radius,
        overflow: Overflow::clip(),
        ..default()
    };
    // A dish or cup is round: its contents are clipped to the circle too.
    if matches!(layout.shape, MeterShape::Dish | MeterShape::Canvas { .. }) {
        track_node.overflow = Overflow::clip();
    }
    let track = commands
        .spawn_owned((
            track_node,
            BorderColor::all(dress.border),
            BackgroundColor(dress.background),
            BoxShadow::new(
                INK.with_alpha(0.45),
                Val::Px(0.0),
                Val::Px(4.0),
                Val::Px(0.0),
                Val::Px(8.0),
            ),
        ))
        .id();
    if let Some(gradient) = dress.gradient.clone() {
        commands
            .entity(track)
            .insert(BackgroundGradient(vec![gradient]));
    }
    if layout.cursor {
        commands
            .entity(track)
            .insert((CursorSurface, RelativeCursorPosition::default()));
    }

    let mut children = Vec::new();
    match (layout.shape, layout.prop) {
        (MeterShape::Bar { .. }, Prop::FrothJug) => {
            froth_parts(commands, index, layout, &mut children)
        }
        (MeterShape::Bar { vertical, fill }, _) => bar_parts(
            commands,
            index,
            layout,
            &dress,
            vertical,
            fill,
            &mut children,
        ),
        // The cursor is the whisk; what moves in the bowl is the liquid.
        (MeterShape::Dish, _) => bowl_parts(commands, index, &mut children),
        (MeterShape::Canvas { path }, _) => canvas_parts(commands, index, path, &mut children),
    }
    commands.entity(track).add_children(&children);

    // Decorations sit outside the track, so they can't move a reading.
    let frame = prop_frame(commands, index, layout, w, h, track);

    let caption = commands
        .spawn_owned((
            Text::new(layout.caption.to_uppercase()),
            theme.text(12.0),
            TextColor(LATTE),
            TextShadow {
                offset: Vec2::splat(1.0),
                color: INK,
            },
        ))
        .id();
    commands
        .spawn_owned(Node {
            flex_direction: FlexDirection::Column,
            align_items: AlignItems::Center,
            row_gap: Val::Px(6.0),
            ..default()
        })
        .add_children(&[frame, caption])
        .id()
}

/// The track plus whatever hangs off it: a mug's handle, a scoop's handle,
/// the pouring jug and its stream, a cup's saucer, the foam pips. Returns the
/// frame holding them all.
///
/// Anything here is *outside* the track, so it can be animated freely without
/// moving where a reading lands.
fn prop_frame(
    commands: &mut Commands,
    index: usize,
    layout: &MeterLayout,
    w: f32,
    h: f32,
    track: Entity,
) -> Entity {
    let prop = layout.prop;
    let mut frame_node = Node {
        width: Val::Px(w),
        height: Val::Px(h),
        ..default()
    };
    // Behind the track...
    let mut behind = Vec::new();
    // ...and in front of it: a stream falls past the cup's lip.
    let mut front = Vec::new();

    if matches!(prop, Prop::Mug | Prop::PourCup) {
        // Handle: a thick ring on the right, drawn as a border-only box.
        behind.push(
            commands
                .spawn_owned((
                    Node {
                        position_type: PositionType::Absolute,
                        right: Val::Px(-24.0),
                        top: Val::Percent(18.0),
                        width: Val::Px(38.0),
                        height: Val::Px(70.0),
                        border: UiRect::all(Val::Px(11.0)),
                        border_radius: BorderRadius::all(Val::Px(20.0)),
                        ..default()
                    },
                    BorderColor::all(CREAM),
                    BackgroundColor(Color::NONE),
                ))
                .id(),
        );
        frame_node.margin.right = Val::Px(18.0);
    }

    match prop {
        Prop::PourCup => {
            // The jug tips as the pour builds and rights itself as it dies.
            let jug = commands
                .spawn_owned((
                    Node {
                        position_type: PositionType::Absolute,
                        left: Val::Percent(50.0),
                        top: Val::Px(-88.0),
                        margin: UiRect::left(Val::Px(-12.0)),
                        width: Val::Px(66.0),
                        height: Val::Px(50.0),
                        border: UiRect::all(Val::Px(4.0)),
                        border_radius: BorderRadius::px(10.0, 10.0, 18.0, 18.0),
                        ..default()
                    },
                    BorderColor::all(STEEL_DARK),
                    BackgroundColor(STEEL),
                    UiTransform::default(),
                    MeterPart {
                        index,
                        kind: Part::Tilt { degrees: -38.0 },
                    },
                ))
                .id();
            // Spout, on the side the jug tips towards.
            commands.entity(jug).with_child_owned((
                Node {
                    position_type: PositionType::Absolute,
                    left: Val::Px(-16.0),
                    bottom: Val::Px(2.0),
                    width: Val::Px(20.0),
                    height: Val::Px(14.0),
                    border_radius: BorderRadius::px(9.0, 2.0, 9.0, 2.0),
                    ..default()
                },
                BackgroundColor(STEEL_DARK),
            ));
            front.push(jug);
            front.push(
                commands
                    .spawn_owned((
                        Node {
                            position_type: PositionType::Absolute,
                            left: Val::Percent(50.0),
                            top: Val::Px(-44.0),
                            margin: UiRect::left(Val::Px(-16.0)),
                            width: Val::Px(8.0),
                            height: Val::Px(58.0),
                            border_radius: BorderRadius::all(Val::Px(4.0)),
                            display: Display::None,
                            ..default()
                        },
                        BackgroundColor(STREAM),
                        MeterPart {
                            index,
                            kind: Part::Stream { base_px: 10.0 },
                        },
                    ))
                    .id(),
            );
            frame_node.margin.top = Val::Px(88.0);
        }
        Prop::FrothJug => {
            behind.push(
                commands
                    .spawn_owned((
                        Node {
                            position_type: PositionType::Absolute,
                            right: Val::Px(-26.0),
                            top: Val::Percent(28.0),
                            width: Val::Px(40.0),
                            height: Val::Px(78.0),
                            border: UiRect::all(Val::Px(11.0)),
                            border_radius: BorderRadius::all(Val::Px(20.0)),
                            ..default()
                        },
                        BorderColor::all(STEEL_DARK),
                        BackgroundColor(Color::NONE),
                    ))
                    .id(),
            );
            // Froth going over the rim: outside the jug, so nothing clips it.
            let scald = scald_line(layout);
            for side in [false, true] {
                let mut node = Node {
                    position_type: PositionType::Absolute,
                    // Hanging from the rim, running down the outside.
                    top: Val::Percent((1.0 - scald) * 100.0),
                    width: Val::Px(20.0),
                    height: Val::Px(6.0),
                    border_radius: BorderRadius::px(4.0, 4.0, 10.0, 10.0),
                    display: Display::None,
                    ..default()
                };
                if side {
                    node.right = Val::Px(-12.0);
                } else {
                    node.left = Val::Px(-12.0);
                }
                front.push(
                    commands
                        .spawn_owned((
                            node,
                            BackgroundColor(MILK),
                            MeterPart {
                                index,
                                kind: Part::Spill { at: scald },
                            },
                        ))
                        .id(),
                );
            }
            frame_node.margin.left = Val::Px(14.0);
            // Foam made so far, as pips rather than a second bar.
            let pips = commands
                .spawn_owned(Node {
                    position_type: PositionType::Absolute,
                    left: Val::Px(0.0),
                    right: Val::Px(0.0),
                    bottom: Val::Px(-26.0),
                    flex_direction: FlexDirection::Row,
                    justify_content: JustifyContent::Center,
                    column_gap: Val::Px(7.0),
                    ..default()
                })
                .id();
            for i in 0..FOAM_PIPS {
                let pip = commands
                    .spawn_owned((
                        Node {
                            width: Val::Px(11.0),
                            height: Val::Px(11.0),
                            border: UiRect::all(Val::Px(2.0)),
                            border_radius: BorderRadius::MAX,
                            ..default()
                        },
                        BorderColor::all(GOLD),
                        BackgroundColor(Color::NONE),
                        MeterPart {
                            index,
                            kind: Part::Pip {
                                index: i,
                                of: FOAM_PIPS,
                            },
                        },
                    ))
                    .id();
                commands.entity(pips).add_child(pip);
            }
            front.push(pips);
            frame_node.margin.bottom = Val::Px(30.0);
            frame_node.margin.right = Val::Px(22.0);
        }
        Prop::Scoop => {
            // Handle: a steel rod standing up from the bowl.
            behind.push(
                commands
                    .spawn_owned((
                        Node {
                            position_type: PositionType::Absolute,
                            left: Val::Percent(50.0),
                            top: Val::Px(-78.0),
                            margin: UiRect::left(Val::Px(-5.0)),
                            width: Val::Px(10.0),
                            height: Val::Px(84.0),
                            border: UiRect::all(Val::Px(2.0)),
                            border_radius: BorderRadius::all(Val::Px(5.0)),
                            ..default()
                        },
                        BorderColor::all(STEEL_DARK),
                        BackgroundColor(STEEL),
                    ))
                    .id(),
            );
            frame_node.margin.top = Val::Px(78.0);
        }
        Prop::LatteCup => {
            // Saucer: a wider ring behind the cup.
            let grow = 18.0;
            behind.push(
                commands
                    .spawn_owned((
                        Node {
                            position_type: PositionType::Absolute,
                            left: Val::Px(-grow),
                            top: Val::Px(-grow),
                            width: Val::Px(w + grow * 2.0),
                            height: Val::Px(h + grow * 2.0),
                            border: UiRect::all(Val::Px(6.0)),
                            border_radius: BorderRadius::MAX,
                            ..default()
                        },
                        BorderColor::all(CREAM.with_alpha(0.35)),
                        BackgroundColor(ROAST),
                    ))
                    .id(),
            );
            frame_node.margin = UiRect::all(Val::Px(grow));
        }
        Prop::Mug | Prop::Plain | Prop::Gauge | Prop::Jug | Prop::Strip | Prop::Bowl => {}
    }

    let frame = commands.spawn_owned(frame_node).id();
    // Siblings paint in order, so: decorations, the track, then anything that
    // belongs in front of it. (A `GlobalZIndex` would instead lift these out
    // of the panel's stacking context and bury them under it.)
    commands.entity(frame).add_children(&behind);
    commands.entity(frame).add_child(track);
    commands.entity(frame).add_children(&front);
    frame
}

/// Where the jug's rim sits: the bottom of the meter's Bad zone.
fn scald_line(layout: &MeterLayout) -> f32 {
    layout
        .zones
        .iter()
        .find(|(_, _, kind)| *kind == MeterZone::Bad)
        .map_or(1.0, |&(from, _, _)| from)
}

/// The steaming jug: milk sitting in the bottom, froth standing on it up to
/// the reading, the drifting band it should sit in, and the rim it spills
/// over. The froth whitens as it is worked ([`MeterReading::extra`]).
fn froth_parts(commands: &mut Commands, index: usize, layout: &MeterLayout, out: &mut Vec<Entity>) {
    let band = |from: Val, height: Val| Node {
        position_type: PositionType::Absolute,
        width: Val::Percent(100.0),
        bottom: from,
        height,
        ..default()
    };
    let scald = scald_line(layout);

    // Froth first: the milk below covers its foot, so it reads as standing on
    // the milk rather than starting at the jug's base.
    out.push(
        commands
            .spawn_owned((
                band(Val::Percent(MILK_LEVEL * 100.0), Val::Px(0.0)),
                BackgroundColor(FROTH_THIN),
                MeterPart {
                    index,
                    kind: Part::Froth { from: MILK_LEVEL },
                },
            ))
            .id(),
    );
    out.push(
        commands
            .spawn_owned((
                band(Val::Px(0.0), Val::Percent(MILK_LEVEL * 100.0)),
                BackgroundColor(MILK_BODY),
            ))
            .id(),
    );
    // Over the rim is where it scalds and spills.
    out.push(
        commands
            .spawn_owned((
                band(
                    Val::Percent(scald * 100.0),
                    Val::Percent((1.0 - scald) * 100.0),
                ),
                BackgroundColor(BAD.with_alpha(0.35)),
            ))
            .id(),
    );
    // The band to keep the froth in, drifting with the reading.
    out.push(
        commands
            .spawn_owned((
                band(Val::Px(0.0), Val::Px(0.0)),
                BackgroundColor(GOOD.with_alpha(0.55)),
                MeterPart {
                    index,
                    kind: Part::Zone { vertical: true },
                },
            ))
            .id(),
    );
    // The top of the froth, and the rim it must stay under.
    out.push(
        commands
            .spawn_owned((
                {
                    let mut node = band(Val::Px(0.0), Val::Px(5.0));
                    node.margin.bottom = Val::Px(-2.5);
                    node.border_radius = BorderRadius::all(Val::Px(3.0));
                    node
                },
                BackgroundColor(MILK),
                MeterPart {
                    index,
                    kind: Part::Needle { vertical: true },
                },
            ))
            .id(),
    );
    out.push(
        commands
            .spawn_owned((
                band(Val::Percent(scald * 100.0), Val::Px(4.0)),
                BackgroundColor(CREAM.with_alpha(0.8)),
            ))
            .id(),
    );
}

/// The bowl's liquid: a conic swirl that turns at the stirring speed.
fn bowl_parts(commands: &mut Commands, index: usize, out: &mut Vec<Entity>) {
    let stops = |light: Color, dark: Color| {
        vec![
            AngularColorStop::new(dark, 0.0),
            AngularColorStop::new(light, TAU * 0.18),
            AngularColorStop::new(dark, TAU * 0.5),
            AngularColorStop::new(light, TAU * 0.68),
            AngularColorStop::new(dark, TAU),
        ]
    };
    out.push(
        commands
            .spawn_owned((
                Node {
                    position_type: PositionType::Absolute,
                    left: Val::Px(0.0),
                    top: Val::Px(0.0),
                    width: Val::Percent(100.0),
                    height: Val::Percent(100.0),
                    border_radius: BorderRadius::MAX,
                    ..default()
                },
                BackgroundColor(Color::NONE),
                // Translucent, so the bowl's own shading still reads under
                // the streaks going round.
                BackgroundGradient(vec![Gradient::Conic(ConicGradient::new(
                    UiPosition::CENTER,
                    stops(
                        Color::srgba(0.86, 0.68, 0.44, 0.55),
                        Color::srgba(0.20, 0.10, 0.05, 0.35),
                    ),
                ))]),
                UiTransform::default(),
                MeterPart {
                    index,
                    kind: Part::Swirl {
                        turns_per_sec: 0.42,
                        angle: 0.0,
                    },
                },
            ))
            .id(),
    );
}

/// Zones, then the fill, then the needle on top (children draw in order).
fn bar_parts(
    commands: &mut Commands,
    index: usize,
    layout: &MeterLayout,
    dress: &Dress,
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
            MeterZone::Good => GOOD.with_alpha(0.75),
            MeterZone::Bad => BAD.with_alpha(0.75),
        };
        let node = span(from, Val::Percent((to - from) * 100.0));
        out.push(commands.spawn_owned((node, BackgroundColor(color))).id());
    }
    out.push(
        commands
            .spawn_owned((
                span(0.0, Val::Px(0.0)),
                BackgroundColor(GOOD.with_alpha(0.75)),
                MeterPart {
                    index,
                    kind: Part::Zone { vertical },
                },
            ))
            .id(),
    );
    if fill {
        let mut node = span(0.0, Val::Px(0.0));
        node.overflow = Overflow::clip();
        let mut bundle = commands.spawn_owned((
            node,
            BackgroundColor(dress.fill),
            MeterPart {
                index,
                kind: Part::Fill { vertical },
            },
        ));
        if let Some(from) = dress.fill_from {
            // Darker at the bottom (or start), the fill colour at the surface.
            let angle = if vertical {
                LinearGradient::TO_TOP
            } else {
                LinearGradient::TO_RIGHT
            };
            bundle.insert(BackgroundGradient(vec![Gradient::Linear(
                LinearGradient::new(
                    angle,
                    vec![
                        ColorStop::new(from, Val::Percent(0.0)),
                        ColorStop::new(dress.fill, Val::Percent(100.0)),
                    ],
                ),
            )]));
        }
        out.push(bundle.id());
    }
    if dress.needle.alpha() > 0.0 {
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
                    BackgroundColor(dress.needle),
                    MeterPart {
                        index,
                        kind: Part::Needle { vertical },
                    },
                ))
                .id(),
        );
    }
}

/// Dots along the path (the first one gold: start here), the trail, then the
/// marker — a drop of milk being poured.
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
            (PATH_DOT_PX, INK.with_alpha(0.45))
        };
        out.push(
            commands
                .spawn_owned((dot(at, size), BackgroundColor(color)))
                .id(),
        );
    }
    for slot in 0..TRAIL_SLOTS {
        let mut node = dot(Vec2::ZERO, TRAIL_DOT_PX);
        node.display = Display::None;
        out.push(
            commands
                .spawn_owned((
                    node,
                    BackgroundColor(MILK),
                    MeterPart {
                        index,
                        kind: Part::Trail { slot },
                    },
                ))
                .id(),
        );
    }
    out.push(
        commands
            .spawn_owned((
                // Smaller than the start dot, so a gold ring shows round it.
                dot(path[0], MARKER_PX - 6.0),
                BackgroundColor(MILK),
                MeterPart {
                    index,
                    kind: Part::Marker { path },
                },
            ))
            .id(),
    );
}
