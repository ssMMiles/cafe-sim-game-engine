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
//! [`crate::minigames`]; this module only feeds them input. Drawing is
//! `brew_overlay.rs`, which reads the [`Session`] and owns every node but the
//! three the input loop needs to find ([`CursorSurface`] and the two buttons).

use bevy::{picking::hover::Hovered, prelude::*, ui::Pressed, ui::RelativeCursorPosition};

use crate::coffee_menu::Brewing;
use crate::drinks::Drink;
use crate::menus::ActiveMenu;
use crate::minigames::{MinigameInput, MinigameState, Outcome};
use crate::pause_menu::GamePaused;

/// How long a step's result shows before the next step starts.
pub(crate) const FLASH_SECS: f32 = 0.8;

/// Marks a coffee machine whose drink the player is making right now.
#[derive(Component)]
pub struct MachineInUse;

#[derive(Resource, Default)]
pub struct BrewSession(Option<Session>);

pub(crate) struct Session {
    pub(crate) machine: Entity,
    pub(crate) drink: &'static Drink,
    pub(crate) step: usize,
    pub(crate) state: MinigameState,
    pub(crate) scores: Vec<f32>,
    /// Counting down while the last step's result is shown.
    pub(crate) flash: Option<f32>,
}

impl BrewSession {
    /// No drink is being made right now. The brew lab's exit condition.
    #[cfg_attr(not(feature = "lab"), allow(dead_code))]
    pub fn is_idle(&self) -> bool {
        self.0.is_none()
    }

    pub(crate) fn current(&self) -> Option<&Session> {
        self.0.as_ref()
    }

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
// The overlay nodes the input loop reads
// ----------------------------------------------------------------------------

/// The meter the game measures the cursor over
/// ([`crate::minigames::MeterLayout::cursor`]). The overlay puts a
/// [`RelativeCursorPosition`] next to it.
#[derive(Component)]
pub struct CursorSurface;

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
