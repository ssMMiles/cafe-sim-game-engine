//! Brew lab: the drink minigames and their overlay, on their own.
//!
//! The overlay (`drink_station.rs`) needs very little of the cafe — a handful
//! of resources, a machine entity to tag, and one frame's input — so this
//! module stands it up in an app of its own. Two ways to run it
//! (`examples/brew_lab.rs`):
//!
//! * **interactive**: a window, real input, play one drink or one step;
//! * **scripted**: no window at all, input from a script file, a fixed
//!   timestep, and PNGs written out every N frames — which is how the UI gets
//!   looked at without a human driving it.
//!
//! ## Why input is synthesised rather than injected as events
//!
//! Bevy only maintains [`RelativeCursorPosition`] for cameras that render to a
//! *window* (`bevy_ui::focus`: "Interactions are only supported for cameras
//! rendering to a window"), and the minigames measure the cursor through
//! exactly that component. Feeding winit or picking events would therefore
//! still need a real window, and a window that macOS can cover or occlude
//! stops presenting, which is no basis for screenshots.
//!
//! So [`apply_lab_input`] writes the three things the overlay actually reads —
//! [`RelativeCursorPosition`], [`Hovered`] and [`Pressed`], plus the two
//! `ButtonInput` resources — straight onto the nodes, hit-testing with the same
//! `ComputedNode` helpers `ui_focus_system` uses. It runs in `Update` ahead of
//! the brew systems, so the `None` that the real focus system writes in
//! `PreUpdate` (no window, no cursor) is overwritten before anything reads it.
//!
//! The one thing a scripted run cannot pin down is the per-attempt jitter:
//! `MinigameState::new` rolls it from the thread RNG, which nothing here can
//! seed. Layout, colour and timing repeat exactly; a target band's position
//! does not.

use std::path::PathBuf;
use std::time::Duration;

use bevy::camera::RenderTarget;
use bevy::picking::hover::Hovered;
use bevy::prelude::*;
use bevy::render::view::screenshot::{Screenshot, save_to_disk};
use bevy::ui::{ComputedNode, Pressed, RelativeCursorPosition, UiGlobalTransform};

use crate::brew_overlay::sync_brew_overlay;
use crate::drink_station::{
    BrewSession, CancelBrewButton, ConfirmStepButton, CursorSurface, end_brew_session_when_closed,
    handle_brew_cancel, tick_brew_session,
};
use crate::drinks::{DRINKS, Drink, Step};
use crate::menus::ActiveMenu;
use crate::minigames::Minigame;
use crate::pause_menu::GamePaused;
use crate::ui::{UiTheme, update_interactive_visuals};

/// Frames the app keeps running after the last screenshot is requested, so the
/// readback and the file write land before `AppExit`.
const FLUSH_FRAMES: u32 = 30;

// ----------------------------------------------------------------------------
// Configuration
// ----------------------------------------------------------------------------

/// What one lab run does. Built by the example from its command line.
#[derive(Resource, Clone, Debug)]
pub struct LabConfig {
    /// The recipe to play. [`game_named`] wraps a single minigame in one.
    pub drink: &'static Drink,
    /// Target size, and the camera's render-target size when headless.
    pub size: UVec2,
    /// `None` runs windowed on real input; `Some` drives the run from a script
    /// with no window.
    pub script: Option<LabScript>,
    /// Where screenshots go. `None` takes none.
    pub shot_dir: Option<PathBuf>,
    /// Screenshot every N frames (0 = only on the script's `shot` commands).
    pub shot_every: u32,
    /// Give up after this many frames (0 = run until the drink finishes).
    pub exit_after: u32,
    /// Seconds per frame fed to the minigames. `None` uses the real clock.
    pub fixed_dt: Option<f32>,
}

impl Default for LabConfig {
    fn default() -> Self {
        Self {
            drink: &DRINKS[0],
            size: UVec2::new(1280, 720),
            script: None,
            shot_dir: None,
            shot_every: 0,
            exit_after: 0,
            fixed_dt: None,
        }
    }
}

impl LabConfig {
    pub fn headless(&self) -> bool {
        self.script.is_some()
    }
}

/// Every minigame the lab can run on its own, in catalog order.
pub const GAMES: &[&str] = &[
    "pressure", "scoop", "pour", "steep", "froth", "stir", "trace",
];

/// Look a recipe up by name, e.g. `Latte` (case-insensitive).
pub fn drink_named(name: &str) -> Option<&'static Drink> {
    DRINKS.iter().find(|d| d.name.eq_ignore_ascii_case(name))
}

/// Every drink the catalog has, for `--help`.
pub fn drink_names() -> Vec<&'static str> {
    DRINKS.iter().map(|d| d.name).collect()
}

/// A one-step recipe playing `game` ([`GAMES`]), tuned exactly as the first
/// real recipe that uses it — so the lab shows the shipping numbers rather
/// than a second set that could drift away from them.
pub fn game_named(game: &str) -> Option<&'static Drink> {
    let matches = |g: &Minigame| {
        matches!(
            (game, g),
            ("pressure", Minigame::Pressure(_))
                | ("scoop", Minigame::Scoop(_))
                | ("pour", Minigame::Pour(_))
                | ("steep", Minigame::Steep(_))
                | ("froth", Minigame::Froth(_))
                | ("stir", Minigame::Stir(_))
                | ("trace", Minigame::Trace(_))
        )
    };
    let step = DRINKS
        .iter()
        .flat_map(|d| d.steps.iter())
        .find(|s| matches(&s.game))?;
    // `Drink::steps` is `&'static` (the catalog is a `const`), so a recipe
    // assembled at runtime has to be leaked. One per process, by construction.
    let steps: &'static [Step] = Box::leak(Box::new([Step {
        label: step.label,
        hint: step.hint,
        game: step.game,
        weight: 1.0,
    }]));
    Some(Box::leak(Box::new(Drink {
        name: step.label,
        price: 0,
        brew_secs: 1.0,
        difficulty: 0.0,
        steps,
    })))
}

// ----------------------------------------------------------------------------
// Scripts
// ----------------------------------------------------------------------------

/// One scripted action, keyed to the frame it happens on.
#[derive(Clone, Debug, PartialEq)]
pub enum LabCmd {
    /// Put the cursor at 0..1 of the cursor surface (y up), or take it away.
    Cursor(Option<Vec2>),
    /// Slide the cursor to a point over N frames — how a trace is drawn.
    CursorTo {
        to: Vec2,
        frames: u32,
    },
    /// Put the cursor on one of the overlay's buttons. They sit outside the
    /// cursor surface, so surface coordinates cannot reach them.
    Point(Widget),
    /// Mouse down, and back up after N frames (0 = stay down).
    Press {
        frames: u32,
    },
    Release,
    /// Tap a key for one frame (`space`, `enter`).
    Key(KeyCode),
    /// Screenshot now, under an optional name.
    Shot(Option<String>),
    Quit,
}

/// A widget a script can aim at by name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Widget {
    /// The step's "Done" button, on the games that have one.
    Confirm,
    /// "Throw away" — ends the session.
    Cancel,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct LabScript(pub Vec<(u32, LabCmd)>);

impl LabScript {
    /// Parse the script format: `<frame>: <command>` a line, `#` comments and
    /// blank lines ignored. Frames need not be in order.
    ///
    /// ```text
    /// 10: cursor 0.5 0.2      # 0..1 across the cursor surface, y up
    /// 20: cursor_to 0.5 0.8 over 60
    /// 30: press 15            # held for 15 frames
    /// 50: key enter
    /// 55: point confirm       # the Done button, then `press` to click it
    /// 60: shot settled        # -> settled.png
    /// 90: quit
    /// ```
    pub fn parse(text: &str) -> Result<Self, String> {
        let mut out = Vec::new();
        for (n, line) in text.lines().enumerate() {
            let line = line.split('#').next().unwrap_or("").trim();
            if line.is_empty() {
                continue;
            }
            let fail = |m: String| format!("line {}: {m}", n + 1);
            let (frame, rest) = line
                .split_once(':')
                .ok_or_else(|| fail("expected `<frame>: <command>`".into()))?;
            let frame: u32 = frame
                .trim()
                .parse()
                .map_err(|_| fail(format!("`{}` is not a frame number", frame.trim())))?;
            let words: Vec<&str> = rest.split_whitespace().collect();
            let num = |i: usize| -> Result<f32, String> {
                words
                    .get(i)
                    .ok_or_else(|| fail("missing a number".into()))?
                    .parse()
                    .map_err(|_| fail(format!("`{}` is not a number", words[i])))
            };
            let cmd = match words.first().copied() {
                Some("cursor") if words.get(1) == Some(&"off") => LabCmd::Cursor(None),
                Some("cursor") => LabCmd::Cursor(Some(Vec2::new(num(1)?, num(2)?))),
                Some("cursor_to") => LabCmd::CursorTo {
                    to: Vec2::new(num(1)?, num(2)?),
                    // `cursor_to x y over N`
                    frames: num(4)?.max(1.0) as u32,
                },
                Some("press") => LabCmd::Press {
                    frames: words.get(1).map_or(Ok(0.0), |_| num(1))? as u32,
                },
                Some("release") => LabCmd::Release,
                Some("key") => match words.get(1).copied() {
                    Some("enter") => LabCmd::Key(KeyCode::Enter),
                    Some("space") => LabCmd::Key(KeyCode::Space),
                    other => return Err(fail(format!("unknown key `{}`", other.unwrap_or("")))),
                },
                Some("point") => match words.get(1).copied() {
                    Some("confirm") => LabCmd::Point(Widget::Confirm),
                    Some("cancel") => LabCmd::Point(Widget::Cancel),
                    other => {
                        return Err(fail(format!("unknown widget `{}`", other.unwrap_or(""))));
                    }
                },
                Some("shot") => LabCmd::Shot(words.get(1).map(|s| s.to_string())),
                Some("quit") => LabCmd::Quit,
                other => return Err(fail(format!("unknown command `{}`", other.unwrap_or("")))),
            };
            out.push((frame, cmd));
        }
        out.sort_by_key(|(f, _)| *f);
        Ok(Self(out))
    }
}

// ----------------------------------------------------------------------------
// Runtime state
// ----------------------------------------------------------------------------

#[derive(Resource, Default)]
struct LabState {
    frame: u32,
    next_cmd: usize,
    /// What the cursor is on.
    aim: Aim,
    glide: Option<Glide>,
    mouse_down: bool,
    /// Frame the current press is released on.
    release_at: Option<u32>,
    key: Option<KeyCode>,
    /// The lab's own clock, advanced a fixed step a frame and copied over the
    /// real one. Kept here rather than advancing `Time` in place, which
    /// panics the moment the real clock has run ahead of the fixed one.
    clock: Time,
    shots: u32,
    /// The frame the last screenshot was asked for: one render target can
    /// only be captured once a frame, so a scripted `shot` and the periodic
    /// one landing together must not both fire.
    last_shot: Option<u32>,
    /// Frame after which the app exits, once everything has flushed.
    done_at: Option<u32>,
}

/// Where the pointer is. Surface coordinates cover the games; the buttons are
/// addressed by name because they lie outside the surface.
#[derive(Clone, Copy, Default, PartialEq)]
enum Aim {
    #[default]
    Nowhere,
    /// 0..1 of the cursor surface, y up.
    Surface(Vec2),
    Widget(Widget),
}

struct Glide {
    from: Vec2,
    to: Vec2,
    start: u32,
    frames: u32,
}

/// The camera every overlay root is pinned to (there is no primary window to
/// fall back on when headless).
#[derive(Resource)]
struct LabCamera(Entity);

/// The image the camera renders to when headless, and screenshots copy from.
#[derive(Resource)]
struct LabTarget(Handle<Image>);

// ----------------------------------------------------------------------------
// Plugin
// ----------------------------------------------------------------------------

pub struct BrewLabPlugin(pub LabConfig);

impl Plugin for BrewLabPlugin {
    fn build(&self, app: &mut App) {
        let config = self.0.clone();
        let theme = UiTheme::load(app.world().resource::<AssetServer>());
        app.insert_resource(ActiveMenu::Brew)
            .insert_resource(theme)
            .insert_resource(BrewSession::default())
            .insert_resource(GamePaused(false))
            .init_resource::<LabState>()
            .insert_resource(config.clone())
            .add_systems(Startup, setup)
            .add_systems(
                Update,
                (
                    (drive_clock, run_script, apply_lab_input)
                        .chain()
                        .run_if(|c: Res<LabConfig>| c.script.is_some()),
                    (
                        handle_brew_cancel,
                        tick_brew_session,
                        end_brew_session_when_closed,
                        sync_brew_overlay,
                        update_interactive_visuals,
                    )
                        .chain(),
                    (target_overlay_at_camera, take_shots, finish).chain(),
                )
                    .chain(),
            );
    }
}

fn setup(mut commands: Commands, config: Res<LabConfig>, mut images: ResMut<Assets<Image>>) {
    let camera = if config.headless() {
        let image = images.add(lab_render_target(config.size));
        commands.insert_resource(LabTarget(image.clone()));
        commands
            .spawn((
                Camera2d,
                RenderTarget::Image(image.into()),
                Camera {
                    // A warm wall, roughly what the cafe shows behind the overlay.
                    clear_color: ClearColorConfig::Custom(Color::srgb(0.20, 0.15, 0.11)),
                    ..default()
                },
            ))
            .id()
    } else {
        commands.spawn(Camera2d).id()
    };
    commands.insert_resource(LabCamera(camera));

    // A machine to own the session. Nothing in the overlay touches it beyond
    // tagging it, and the `Brewing` it ends up with is never read here.
    let machine = commands.spawn(Name::new("lab machine")).id();
    commands.queue(start_session(machine));
}

fn start_session(machine: Entity) -> impl Command {
    move |world: &mut World| {
        let drink = world.resource::<LabConfig>().drink;
        world.resource_scope(|world, mut session: Mut<BrewSession>| {
            let mut commands = world.commands();
            if !session.start(&mut commands, machine, drink) {
                error!("[lab] {} has no steps", drink.name);
            }
        });
    }
}

fn lab_render_target(size: UVec2) -> Image {
    use bevy::asset::RenderAssetUsages;
    use bevy::image::Image;
    use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat, TextureUsages};

    let mut image = Image::new_fill(
        Extent3d {
            width: size.x,
            height: size.y,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        &[0, 0, 0, 255],
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::default(),
    );
    image.texture_descriptor.usage = TextureUsages::TEXTURE_BINDING
        | TextureUsages::COPY_SRC
        | TextureUsages::COPY_DST
        | TextureUsages::RENDER_ATTACHMENT;
    image
}

/// UI picks its camera from the primary window unless told otherwise, and a
/// headless run has none — so every root node gets pinned to ours.
fn target_overlay_at_camera(
    mut commands: Commands,
    camera: Res<LabCamera>,
    roots: Query<Entity, (With<Node>, Without<ChildOf>, Without<UiTargetCamera>)>,
) {
    for root in &roots {
        commands.entity(root).insert(UiTargetCamera(camera.0));
    }
}

// ----------------------------------------------------------------------------
// Scripted input
// ----------------------------------------------------------------------------

/// Pin the clock to a fixed step, so a scripted run plays the same every time
/// regardless of how fast the frames come.
fn drive_clock(config: Res<LabConfig>, mut state: ResMut<LabState>, mut time: ResMut<Time>) {
    let Some(dt) = config.fixed_dt else {
        return;
    };
    state.clock.advance_by(Duration::from_secs_f32(dt));
    *time = state.clock;
}

fn run_script(config: Res<LabConfig>, mut state: ResMut<LabState>, mut commands: Commands) {
    let Some(script) = config.script.as_ref() else {
        return;
    };
    let frame = state.frame;

    while let Some((at, cmd)) = script.0.get(state.next_cmd) {
        if *at > frame {
            break;
        }
        state.next_cmd += 1;
        match cmd.clone() {
            LabCmd::Cursor(at) => {
                state.glide = None;
                state.aim = at.map_or(Aim::Nowhere, Aim::Surface);
            }
            LabCmd::Point(widget) => {
                state.glide = None;
                state.aim = Aim::Widget(widget);
            }
            LabCmd::CursorTo { to, frames } => {
                let from = match state.aim {
                    Aim::Surface(at) => at,
                    _ => to,
                };
                state.glide = Some(Glide {
                    from,
                    to,
                    start: frame,
                    frames,
                });
            }
            LabCmd::Press { frames } => {
                state.mouse_down = true;
                state.release_at = (frames > 0).then_some(frame + frames);
            }
            LabCmd::Release => {
                state.mouse_down = false;
                state.release_at = None;
            }
            LabCmd::Key(key) => state.key = Some(key),
            LabCmd::Shot(name) => {
                let name = name.unwrap_or_else(|| format!("frame_{frame:05}"));
                shoot(&mut commands, &config, &mut state, &name);
            }
            LabCmd::Quit => state.done_at = Some(frame),
        }
    }

    if state.release_at == Some(frame) {
        state.mouse_down = false;
        state.release_at = None;
    }
    if let Some(glide) = state.glide.as_ref() {
        let t = ((frame - glide.start) as f32 / glide.frames as f32).clamp(0.0, 1.0);
        let at = glide.from.lerp(glide.to, t);
        state.aim = Aim::Surface(at);
        if t >= 1.0 {
            state.glide = None;
        }
    }
}

/// Everything the overlay reads about the pointer, written directly.
///
/// `ui_focus_system` has already set each [`RelativeCursorPosition`] to "no
/// cursor" this frame (`PreUpdate`, and there is no window to read one from);
/// this runs in `Update` before the brew systems, so these values are what they
/// see.
fn apply_lab_input(
    mut commands: Commands,
    state: Res<LabState>,
    mut mouse: ResMut<ButtonInput<MouseButton>>,
    mut keys: ResMut<ButtonInput<KeyCode>>,
    surface: Query<(&ComputedNode, &UiGlobalTransform), With<CursorSurface>>,
    confirm: Query<&UiGlobalTransform, With<ConfirmStepButton>>,
    cancel: Query<&UiGlobalTransform, With<CancelBrewButton>>,
    mut relative: Query<(
        &ComputedNode,
        &UiGlobalTransform,
        &mut RelativeCursorPosition,
    )>,
    hoverable: Query<(Entity, &ComputedNode, &UiGlobalTransform, &Hovered)>,
    pressed: Query<Entity, With<Pressed>>,
) {
    // The script speaks in cursor-surface coordinates (0..1, y up). Without a
    // surface on screen this step, there is nothing to measure against and the
    // games that use one would read garbage, so the cursor simply goes away.
    let point = match state.aim {
        Aim::Nowhere => None,
        Aim::Surface(at) => surface.single().ok().map(|(node, transform)| {
            let size = node.size();
            transform.translation + Vec2::new((at.x - 0.5) * size.x, (0.5 - at.y) * size.y)
        }),
        Aim::Widget(Widget::Confirm) => confirm.single().ok().map(|t| t.translation),
        Aim::Widget(Widget::Cancel) => cancel.single().ok().map(|t| t.translation),
    };

    for (node, transform, mut rel) in &mut relative {
        let over = point.is_some_and(|p| node.contains_point(*transform, p));
        let normalized = point.and_then(|p| node.normalize_point(*transform, p));
        rel.set_if_neq(RelativeCursorPosition {
            cursor_over: over,
            normalized,
        });
    }

    // `Hovered` is immutable (that is how `Changed<Hovered>` stays meaningful
    // for the widget styling), so a change is an insert — and only ever on a
    // change, or every button would re-style every frame.
    let mut over_button = None;
    for (entity, node, transform, hovered) in &hoverable {
        let over = point.is_some_and(|p| node.contains_point(*transform, p));
        if over {
            over_button = Some(entity);
        }
        if hovered.0 != over {
            commands.entity(entity).insert(Hovered(over));
        }
    }

    // `Pressed` is what `ui_widgets::Button` would insert on pointer-down; the
    // overlay watches for it with `Added<Pressed>`, so it has to arrive fresh.
    if state.mouse_down {
        mouse.press(MouseButton::Left);
        if let Some(button) = over_button
            && pressed.get(button).is_err()
        {
            commands.entity(button).insert(Pressed);
        }
    } else {
        mouse.release(MouseButton::Left);
        for button in &pressed {
            commands.entity(button).remove::<Pressed>();
        }
    }

    keys.release_all();
    if let Some(key) = state.key {
        keys.press(key);
    }
}

// ----------------------------------------------------------------------------
// Screenshots and exit
// ----------------------------------------------------------------------------

fn take_shots(mut commands: Commands, config: Res<LabConfig>, mut state: ResMut<LabState>) {
    let frame = state.frame;
    // Nothing worth capturing once the run is over, and a capture in flight
    // when the app exits loses its channel.
    if state.done_at.is_some() {
        return;
    }
    if config.shot_every > 0 && frame.is_multiple_of(config.shot_every) {
        let name = format!("frame_{frame:05}");
        shoot(&mut commands, &config, &mut state, &name);
    }
}

fn shoot(commands: &mut Commands, config: &LabConfig, state: &mut LabState, name: &str) {
    let Some(dir) = config.shot_dir.as_ref() else {
        return;
    };
    if state.last_shot == Some(state.frame) {
        return;
    }
    state.last_shot = Some(state.frame);
    let path = dir.join(format!("{name}.png"));
    state.shots += 1;
    commands.queue(move |world: &mut World| {
        let Some(image) = world.get_resource::<LabTarget>().map(|t| t.0.clone()) else {
            return;
        };
        world
            .spawn(Screenshot::image(image))
            .observe(save_to_disk(path));
    });
}

/// End the run: the drink finished, the script said so, or the frame budget ran
/// out — then keep ticking long enough for the screenshots to reach disk.
fn finish(
    config: Res<LabConfig>,
    session: Res<BrewSession>,
    active: Res<ActiveMenu>,
    mut state: ResMut<LabState>,
    mut exit: MessageWriter<AppExit>,
) {
    let frame = state.frame;
    state.frame += 1;

    if state.done_at.is_none() {
        let budget_spent = config.exit_after > 0 && frame >= config.exit_after;
        let finished = config.script.is_some() && !active.is_open() && session.is_idle();
        if budget_spent || finished {
            state.done_at = Some(frame);
        }
    }

    if let Some(done) = state.done_at
        && frame >= done + FLUSH_FRAMES
    {
        exit.write(AppExit::Success);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_script() {
        let script = LabScript::parse(
            "# a comment\n\
             \n\
             10: cursor 0.5 0.25\n\
             40: cursor_to 0.5 0.75 over 30\n\
             5: press 12\n\
             80: key enter\n\
             90: shot settled\n\
             99: quit\n",
        )
        .expect("parses");
        assert_eq!(
            script.0,
            vec![
                (5, LabCmd::Press { frames: 12 }),
                (10, LabCmd::Cursor(Some(Vec2::new(0.5, 0.25)))),
                (
                    40,
                    LabCmd::CursorTo {
                        to: Vec2::new(0.5, 0.75),
                        frames: 30
                    }
                ),
                (80, LabCmd::Key(KeyCode::Enter)),
                (90, LabCmd::Shot(Some("settled".into()))),
                (99, LabCmd::Quit),
            ]
        );
    }

    #[test]
    fn rejects_nonsense_with_a_line_number() {
        let err = LabScript::parse("1: cursor 0.5 0.5\n2: wiggle").unwrap_err();
        assert!(err.starts_with("line 2:"), "{err}");
    }

    #[test]
    fn every_named_game_resolves_to_a_one_step_recipe() {
        for game in GAMES {
            let drink = game_named(game)
                .unwrap_or_else(|| panic!("no recipe in the catalog plays `{game}`"));
            assert_eq!(drink.steps.len(), 1, "{game}");
        }
        assert!(game_named("wiggle").is_none());
    }

    #[test]
    fn drinks_are_found_by_name_ignoring_case() {
        assert!(drink_named("latte").is_some());
        assert!(drink_named("Flat White").is_none());
    }
}
