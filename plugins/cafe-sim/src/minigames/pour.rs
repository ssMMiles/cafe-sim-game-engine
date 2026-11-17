//! Pour to the line: hold to pour, let go to stop. The stream speeds up while
//! you hold and keeps running for a moment after you let go, so the release
//! has to come *before* the level reaches the line. One pour per step; going
//! over the top spills and ends the step with nothing.

use super::{
    MeterLayout, MeterReading, MeterZone, MinigameInput, Outcome, Prop, band_score, shifted,
};

#[derive(Debug, Clone, Copy)]
pub struct PourParams {
    /// How quickly the stream speeds up while held, cup-heights per s².
    pub accel: f32,
    /// Fastest stream, cup-heights per second.
    pub max_speed: f32,
    /// After letting go the stream dies off with this time constant, so it
    /// still adds about `speed × settle_secs`.
    pub settle_secs: f32,
    /// The fill line, before the per-attempt shift.
    pub line: (f32, f32),
    /// The line moves by up to ± this each attempt.
    pub jitter: f32,
}

#[derive(Debug, Clone)]
pub struct PourState {
    pub fill: f32,
    speed: f32,
    /// This attempt's line.
    line: (f32, f32),
    phase: Phase,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// Waits for a fresh press, so a button still held from the drink menu
    /// doesn't start pouring.
    Waiting,
    Pouring,
    Settling,
}

/// Below this the stream has stopped and the step is scored.
const SETTLED_SPEED: f32 = 0.005;
/// Under the line, the score falls to 0 this far below it.
const UNDER_RANGE: f32 = 0.3;
/// Over the line it falls off much faster: a too-full cup is worse.
const OVER_RANGE: f32 = 0.08;

impl PourState {
    pub fn new(p: &PourParams, rng: &mut impl rand::Rng) -> Self {
        Self {
            fill: 0.0,
            speed: 0.0,
            line: shifted(p.line, p.jitter, rng),
            phase: Phase::Waiting,
        }
    }

    pub fn meter(&self) -> MeterLayout {
        MeterLayout::bar(true, true, "cup")
            .dressed_as(Prop::PourCup)
            .with_zone(self.line.0, self.line.1, MeterZone::Good)
    }

    /// The level, plus how hard the stream is running right now — which rises
    /// while held and dies away after, so the overlay's stream does too.
    pub fn reading(&self, p: &PourParams) -> MeterReading {
        MeterReading {
            value: self.fill,
            zone: None,
            extra: (self.speed / p.max_speed).clamp(0.0, 1.0),
        }
    }

    pub fn update(&mut self, p: &PourParams, input: &MinigameInput) -> Outcome {
        match self.phase {
            Phase::Waiting if input.pressed => self.phase = Phase::Pouring,
            Phase::Waiting => return Outcome::Running,
            Phase::Pouring if !input.held => self.phase = Phase::Settling,
            _ => {}
        }
        if self.phase == Phase::Pouring {
            self.speed = (self.speed + p.accel * input.dt).min(p.max_speed);
        } else {
            self.speed *= (-input.dt / p.settle_secs).exp();
        }
        self.fill += self.speed * input.dt;
        if self.fill >= 1.0 {
            self.fill = 1.0;
            return Outcome::Done { score: 0.0 };
        }
        if self.phase == Phase::Settling && self.speed < SETTLED_SPEED {
            return Outcome::Done {
                score: band_score(
                    self.fill,
                    self.line,
                    self.line.0 - UNDER_RANGE,
                    self.line.1 + OVER_RANGE,
                ),
            };
        }
        Outcome::Running
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_util::*;
    use super::*;

    const P: PourParams = PourParams {
        accel: 1.2,
        max_speed: 0.5,
        settle_secs: 0.25,
        line: (0.72, 0.78),
        jitter: 0.0,
    };

    /// Pour until the level passes `release_at`, let go, return the score and
    /// where the level settled.
    fn pour_to(release_at: f32) -> (f32, f32) {
        let mut s = PourState::new(&P, &mut rng());
        assert_eq!(s.update(&P, &press()), Outcome::Running);
        while s.fill < release_at {
            if let Outcome::Done { score } = s.update(&P, &hold()) {
                return (score, s.fill);
            }
        }
        for _ in 0..10_000 {
            if let Outcome::Done { score } = s.update(&P, &release()) {
                return (score, s.fill);
            }
        }
        panic!("the stream must settle");
    }

    #[test]
    fn waits_for_a_fresh_press() {
        let mut s = PourState::new(&P, &mut rng());
        for _ in 0..60 {
            assert_eq!(s.update(&P, &hold()), Outcome::Running);
        }
        assert_eq!(s.fill, 0.0);
    }

    #[test]
    fn the_stream_carries_on_after_letting_go() {
        // Letting go at the line overshoots it: you have to stop early.
        let (score, settled) = pour_to(0.75);
        assert!(settled > 0.75 + 0.05, "{settled}");
        assert!(score < 0.5, "{score}");
    }

    #[test]
    fn stopping_early_enough_lands_on_the_line() {
        // Full speed adds ≈ max_speed × settle_secs = 0.125 after release.
        let (score, settled) = pour_to(0.75 - 0.125);
        assert!(score >= 0.95, "{score} at {settled}");
    }

    #[test]
    fn overshooting_costs_more_than_undershooting() {
        let (under, under_at) = pour_to(0.5);
        let (over, over_at) = pour_to(0.72);
        let (under_by, over_by) = (P.line.0 - under_at, over_at - P.line.1);
        // Over by less than under, and still scored lower.
        assert!(
            over_by < under_by && over < under,
            "{over}@{over_by} {under}@{under_by}"
        );
    }

    #[test]
    fn overflowing_spills() {
        let (score, fill) = pour_to(2.0);
        assert_eq!((score, fill), (0.0, 1.0));
    }
}
