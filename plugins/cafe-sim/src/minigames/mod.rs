//! Drink-making minigames: pure state machines, no ECS or UI.
//!
//! Each game is driven by [`MinigameState::update`] with one frame's
//! [`MinigameInput`] and eventually returns [`Outcome::Done`] with a 0..=1
//! score. The brew station (`drink_station.rs`) owns the UI: it spawns the
//! [`MeterLayout`]s a game asks for once per step and moves them every frame
//! from its [`MeterReading`]s, so every game's scoring can be tested by
//! feeding it scripted input.
//!
//! Targets move: a game's band is shifted by up to its `jitter` each attempt
//! (rolled in [`MinigameState::new`]), so a step can't be done from memory.

mod froth;
mod pour;
mod pressure;
mod scoop;
mod steep;
mod stir;
mod trace;

use bevy::math::Vec2;
use rand::RngExt;

pub use froth::{FrothParams, FrothState};
pub use pour::{PourParams, PourState};
pub use pressure::{PressureParams, PressureState};
pub use scoop::{ScoopParams, ScoopState};
pub use steep::{SteepParams, SteepState};
pub use stir::{StirParams, StirState};
pub use trace::{TraceParams, TraceState, point_along};

/// Which minigame a recipe step plays, with its tuning.
#[derive(Debug, Clone, Copy)]
pub enum Minigame {
    Pressure(PressureParams),
    Scoop(ScoopParams),
    Pour(PourParams),
    Steep(SteepParams),
    Froth(FrothParams),
    Stir(StirParams),
    Trace(TraceParams),
}

impl Minigame {
    /// Every band stays inside 0..1 (and below any cap) at its full jitter —
    /// checked by the drink data test.
    #[cfg(test)]
    pub fn is_well_formed(&self) -> bool {
        let fits = |(lo, hi): (f32, f32), jitter: f32, top: f32| {
            jitter >= 0.0 && 0.0 < lo - jitter && lo < hi && hi + jitter < top
        };
        match self {
            Minigame::Pressure(p) => fits(p.band, p.jitter, p.cap) && p.cap <= 1.0,
            Minigame::Scoop(p) => {
                fits(p.band, p.jitter, 1.0)
                    && (0.0..1.0).contains(&p.capacity_jitter)
                    && p.capacity > 0.0
                    && p.capacity * (1.0 + p.capacity_jitter) < 1.0
                    && p.fill_secs > 0.0
            }
            Minigame::Pour(p) => {
                fits(p.line, p.jitter, 1.0) && p.max_speed > 0.0 && p.settle_secs > 0.0
            }
            Minigame::Steep(p) => fits(p.window, p.jitter, 1.0) && p.duration_secs > 0.0,
            Minigame::Froth(p) => {
                fits(p.band, p.jitter + p.drift, p.scald)
                    && p.drift >= 0.0
                    && p.drift_period_secs > 0.0
                    && p.scald <= 1.0
                    && p.heat_rate > 0.0
                    && p.cool_rate > 0.0
                    && p.foam_secs > 0.0
                    && p.time_limit_secs > p.foam_secs
            }
            Minigame::Stir(p) => {
                p.mix_per_rad > 0.0
                    && p.safe_speed > 0.0
                    && (0.0..1.0).contains(&p.good_from)
                    && p.slosh_gain > 0.0
                    && p.slosh_decay > 0.0
                    && p.max_secs > 0.0
            }
            Minigame::Trace(p) => {
                p.path.len() >= 2
                    && p.path
                        .iter()
                        .all(|v| (0.0..=1.0).contains(&v.x) && (0.0..=1.0).contains(&v.y))
                    && p.tolerance > 0.0
                    && p.max_secs > 0.0
            }
        }
    }
}

/// One frame of player input. "Action" is LMB or Space.
#[derive(Debug, Clone, Copy, Default)]
pub struct MinigameInput {
    pub held: bool,
    pub pressed: bool,
    /// The cursor over the game's cursor surface (the meter whose
    /// [`MeterLayout::cursor`] is set): 0..1 on both axes inside it, y up,
    /// beyond that range outside it. `None` when there is no cursor.
    pub cursor: Option<Vec2>,
    /// The step's Done button (or Enter), for games that offer one
    /// ([`MinigameState::confirm_label`]).
    pub confirm: bool,
    pub dt: f32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Outcome {
    Running,
    Done { score: f32 },
}

// ----------------------------------------------------------------------------
// What the UI draws
// ----------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MeterZone {
    /// Where the player is aiming.
    Good,
    /// Ends the step badly (over-tamped, spilt).
    Bad,
}

/// What a meter looks like and what its reading moves.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MeterShape {
    /// A bar with a needle at the reading, optionally shaded up to it.
    Bar { vertical: bool, fill: bool },
    /// A round dish: a cursor surface with no moving parts (the OS cursor is
    /// the whisk). Its reading is ignored.
    Dish,
    /// A square with a dotted path; a marker sits at the reading's fraction
    /// of the way along it. Points are 0..1, y up.
    Canvas { path: &'static [Vec2] },
}

/// One meter the overlay spawns for a step. Nothing but the reading moves.
pub struct MeterLayout {
    pub shape: MeterShape,
    /// `(from, to, kind)` along a bar, 0..1. Ignored by other shapes.
    pub zones: Vec<(f32, f32, MeterZone)>,
    /// This meter is where [`MinigameInput::cursor`] is measured.
    pub cursor: bool,
    /// Short label under the meter.
    pub caption: &'static str,
}

/// One meter's moving parts this frame, in [`MinigameState::meters`] order.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct MeterReading {
    pub value: f32,
    /// A good zone that moves during the step (Froth's drifting band),
    /// `(from, to)` along a bar. Static zones live in [`MeterLayout::zones`].
    pub zone: Option<(f32, f32)>,
}

impl From<f32> for MeterReading {
    fn from(value: f32) -> Self {
        Self { value, zone: None }
    }
}

/// How close a drawn point came to the path it was following.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Grade {
    Good,
    Off,
    Bad,
}

/// A point the player actually drew on a canvas.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TrailPoint {
    pub at: Vec2,
    pub grade: Grade,
}

impl MeterLayout {
    pub fn bar(vertical: bool, fill: bool, caption: &'static str) -> Self {
        Self {
            shape: MeterShape::Bar { vertical, fill },
            zones: Vec::new(),
            cursor: false,
            caption,
        }
    }

    pub fn with_zone(mut self, from: f32, to: f32, kind: MeterZone) -> Self {
        self.zones.push((from, to, kind));
        self
    }
}

// ----------------------------------------------------------------------------
// State + dispatch
// ----------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub enum MinigameState {
    Pressure(PressureState),
    Scoop(ScoopState),
    Pour(PourState),
    Steep(SteepState),
    Froth(FrothState),
    Stir(StirState),
    Trace(TraceState),
}

impl MinigameState {
    pub fn new(game: &Minigame, rng: &mut impl rand::Rng) -> Self {
        match game {
            Minigame::Pressure(p) => Self::Pressure(PressureState::new(p, rng)),
            Minigame::Scoop(p) => Self::Scoop(ScoopState::new(p, rng)),
            Minigame::Pour(p) => Self::Pour(PourState::new(p, rng)),
            Minigame::Steep(p) => Self::Steep(SteepState::new(p, rng)),
            Minigame::Froth(p) => Self::Froth(FrothState::new(p, rng)),
            Minigame::Stir(p) => Self::Stir(StirState::new(p, rng)),
            Minigame::Trace(p) => Self::Trace(TraceState::new(p, rng)),
        }
    }

    pub fn update(
        &mut self,
        game: &Minigame,
        input: &MinigameInput,
        rng: &mut impl rand::Rng,
    ) -> Outcome {
        match (self, game) {
            (Self::Pressure(s), Minigame::Pressure(p)) => s.update(p, input),
            (Self::Scoop(s), Minigame::Scoop(p)) => s.update(p, input, rng),
            (Self::Trace(s), Minigame::Trace(p)) => s.update(p, input),
            (Self::Pour(s), Minigame::Pour(p)) => s.update(p, input),
            (Self::Steep(s), Minigame::Steep(p)) => s.update(p, input),
            (Self::Froth(s), Minigame::Froth(p)) => s.update(p, input),
            (Self::Stir(s), Minigame::Stir(p)) => s.update(p, input),
            _ => unreachable!("minigame state built from a different game"),
        }
    }

    /// The bars this attempt draws (zones already shifted by the jitter).
    pub fn meters(&self, game: &Minigame) -> Vec<MeterLayout> {
        match (self, game) {
            (Self::Pressure(s), Minigame::Pressure(p)) => vec![s.meter(p)],
            (Self::Scoop(s), Minigame::Scoop(_)) => s.meters(),
            (Self::Pour(s), Minigame::Pour(_)) => vec![s.meter()],
            (Self::Steep(s), Minigame::Steep(_)) => vec![s.meter()],
            (Self::Froth(s), Minigame::Froth(p)) => s.meters(p),
            (Self::Stir(s), Minigame::Stir(p)) => s.meters(p),
            (Self::Trace(s), Minigame::Trace(p)) => s.meters(p),
            _ => unreachable!("minigame state built from a different game"),
        }
    }

    /// Each meter's reading this frame, in [`Self::meters`] order.
    pub fn readings(&self, game: &Minigame) -> Vec<MeterReading> {
        let plain = |values: &[f32]| values.iter().map(|&v| v.into()).collect();
        match (self, game) {
            (Self::Pressure(s), _) => plain(&[s.value]),
            (Self::Scoop(s), _) => plain(&[s.amount, s.in_scoop]),
            (Self::Pour(s), _) => plain(&[s.fill]),
            (Self::Steep(s), _) => plain(&[s.progress]),
            (Self::Froth(s), Minigame::Froth(p)) => s.readings(p),
            (Self::Stir(s), Minigame::Stir(p)) => s.readings(p),
            (Self::Trace(s), Minigame::Trace(p)) => plain(&[s.fraction(), s.time_left(p)]),
            _ => unreachable!("minigame state built from a different game"),
        }
    }

    /// One line of live feedback under the meters, if the game has any.
    pub fn status(&self, game: &Minigame) -> Option<String> {
        match (self, game) {
            (Self::Scoop(s), _) => Some(s.status()),
            (Self::Stir(s), _) => s.status(),
            (Self::Trace(s), Minigame::Trace(p)) => Some(s.status(p)),
            _ => None,
        }
    }

    /// What the player has drawn so far, for a game with a canvas.
    pub fn trail(&self) -> &[TrailPoint] {
        match self {
            Self::Trace(s) => s.trail(),
            _ => &[],
        }
    }

    /// The label of this game's Done button, if it ends on the player's word.
    pub fn confirm_label(&self) -> Option<&'static str> {
        match self {
            Self::Scoop(_) => Some("Done scooping"),
            _ => None,
        }
    }
}

/// A band moved by a uniform roll in `±jitter`.
pub(crate) fn shifted(band: (f32, f32), jitter: f32, rng: &mut impl rand::Rng) -> (f32, f32) {
    let d = if jitter > 0.0 {
        rng.random_range(-jitter..=jitter)
    } else {
        0.0
    };
    (band.0 + d, band.1 + d)
}

/// 1 inside `band`, falling linearly to 0 at `floor` below and `ceil` above.
/// Shared falloff for "stop in the zone" games.
pub(crate) fn band_score(v: f32, band: (f32, f32), floor: f32, ceil: f32) -> f32 {
    if v < band.0 {
        ((v - floor) / (band.0 - floor)).clamp(0.0, 1.0)
    } else if v > band.1 {
        ((ceil - v) / (ceil - band.1)).clamp(0.0, 1.0)
    } else {
        1.0
    }
}

#[cfg(test)]
pub(crate) mod test_util {
    use super::*;
    use rand::SeedableRng;

    pub const DT: f32 = 1.0 / 60.0;

    pub fn rng() -> rand::rngs::StdRng {
        rand::rngs::StdRng::seed_from_u64(7)
    }

    pub fn idle() -> MinigameInput {
        MinigameInput {
            dt: DT,
            ..Default::default()
        }
    }
    pub fn press() -> MinigameInput {
        MinigameInput {
            held: true,
            pressed: true,
            dt: DT,
            ..Default::default()
        }
    }
    pub fn hold() -> MinigameInput {
        MinigameInput {
            held: true,
            dt: DT,
            ..Default::default()
        }
    }
    /// Nothing held after holding = let go.
    pub fn release() -> MinigameInput {
        idle()
    }
    /// `input` with the cursor at `at`.
    pub fn at(input: MinigameInput, at: Vec2) -> MinigameInput {
        MinigameInput {
            cursor: Some(at),
            ..input
        }
    }
}
