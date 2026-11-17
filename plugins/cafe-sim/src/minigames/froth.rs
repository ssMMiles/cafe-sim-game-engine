//! Steam the milk: hold to heat, let go and it cools. Foam only builds while
//! the temperature sits in the green, so the player pulses the button to keep
//! the needle there until the foam bar is full. Too hot scalds the milk and
//! ends the step with most of the score gone.
//!
//! Once the milk first reaches the band, the band starts to drift (two
//! sines, so it doesn't settle into a rhythm), so parking the needle in one
//! spot by tapping stops working: you have to follow it.

use std::f32::consts::TAU;

use rand::RngExt;

use super::{MeterLayout, MeterReading, MeterZone, MinigameInput, Outcome, Prop, shifted};

#[derive(Debug, Clone, Copy)]
pub struct FrothParams {
    /// Temperature rise per second while held, meter-heights.
    pub heat_rate: f32,
    /// Temperature fall per second while not held.
    pub cool_rate: f32,
    /// Where the milk froths, before the per-attempt shift.
    pub band: (f32, f32),
    /// The band moves by up to ± this each attempt.
    pub jitter: f32,
    /// Scalded at or above this: the step ends.
    pub scald: f32,
    /// Seconds in the band needed to fill the foam bar.
    pub foam_secs: f32,
    /// Give up after this long: the step ends with the foam so far.
    pub time_limit_secs: f32,
    /// How far the band drifts either way once reached, meter-heights.
    pub drift: f32,
    /// Period of the band's main swing.
    pub drift_period_secs: f32,
}

#[derive(Debug, Clone)]
pub struct FrothState {
    pub temp: f32,
    pub foam: f32,
    /// This attempt's band, before drift.
    band: (f32, f32),
    /// +1 or −1: which way the band sets off.
    drift_dir: f32,
    /// Time since the milk first reached the band.
    drifting_secs: f32,
    elapsed: f32,
    /// Time spent outside the band after first reaching it. Warming the milk
    /// up isn't waste; letting it drift out again is.
    wasted: f32,
    reached: bool,
    /// Waits for a fresh press, so a button still held from the drink menu
    /// doesn't start heating.
    started: bool,
}

/// Score left after scalding the milk.
const SCALD_FACTOR: f32 = 0.3;

impl FrothState {
    pub fn new(p: &FrothParams, rng: &mut impl rand::Rng) -> Self {
        Self {
            temp: 0.0,
            foam: 0.0,
            band: shifted(p.band, p.jitter, rng),
            drift_dir: if rng.random_bool(0.5) { 1.0 } else { -1.0 },
            drifting_secs: 0.0,
            elapsed: 0.0,
            wasted: 0.0,
            reached: false,
            started: false,
        }
    }

    /// One jug, not two bars: the froth rises with the heat, the green ring
    /// is where it should sit, and the rim at `scald` is what it spills over.
    /// The foam made so far shows in how dense the froth draws (plus pips).
    pub fn meters(&self, p: &FrothParams) -> Vec<MeterLayout> {
        vec![
            // The good band moves, so it comes with the reading instead.
            MeterLayout::bar(true, true, "froth")
                .dressed_as(Prop::FrothJug)
                .with_zone(p.scald, 1.0, MeterZone::Bad),
        ]
    }

    pub fn readings(&self, p: &FrothParams) -> Vec<MeterReading> {
        vec![MeterReading {
            value: self.temp,
            zone: Some(self.current_band(p)),
            extra: self.foam,
        }]
    }

    /// Where the band is now: still until the milk first reaches it, then
    /// drifting from where it was.
    pub fn current_band(&self, p: &FrothParams) -> (f32, f32) {
        let phase = TAU * self.drifting_secs / p.drift_period_secs;
        let swing = 0.7 * phase.sin() + 0.3 * (2.7 * phase).sin();
        let offset = self.drift_dir * p.drift * swing;
        (self.band.0 + offset, self.band.1 + offset)
    }

    pub fn update(&mut self, p: &FrothParams, input: &MinigameInput) -> Outcome {
        if !self.started {
            if !input.pressed {
                return Outcome::Running;
            }
            self.started = true;
        }
        self.elapsed += input.dt;
        if input.held {
            self.temp = (self.temp + p.heat_rate * input.dt).min(1.0);
        } else {
            self.temp = (self.temp - p.cool_rate * input.dt).max(0.0);
        }
        if self.temp >= p.scald {
            return Outcome::Done {
                score: self.score(p) * SCALD_FACTOR,
            };
        }
        if self.reached {
            self.drifting_secs += input.dt;
        }
        let band = self.current_band(p);
        if (band.0..=band.1).contains(&self.temp) {
            self.reached = true;
            self.foam = (self.foam + input.dt / p.foam_secs).min(1.0);
        } else if self.reached {
            self.wasted += input.dt;
        }
        if self.foam >= 1.0 || self.elapsed >= p.time_limit_secs {
            return Outcome::Done {
                score: self.score(p),
            };
        }
        Outcome::Running
    }

    /// Foam made × how much of the time since reaching the band went into
    /// making it.
    fn score(&self, p: &FrothParams) -> f32 {
        let useful = self.foam * p.foam_secs;
        let efficiency = useful / (useful + self.wasted).max(1e-6);
        self.foam * (0.5 + 0.5 * efficiency)
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_util::*;
    use super::*;

    const P: FrothParams = FrothParams {
        heat_rate: 0.5,
        cool_rate: 0.25,
        band: (0.6, 0.72),
        jitter: 0.0,
        scald: 0.9,
        foam_secs: 3.0,
        time_limit_secs: 12.0,
        drift: 0.0,
        drift_period_secs: 4.0,
    };

    /// Pulse the button to hold the temperature near `target` until done.
    fn hold_at(target: f32) -> f32 {
        let mut s = FrothState::new(&P, &mut rng());
        assert_eq!(s.update(&P, &press()), Outcome::Running);
        for _ in 0..100_000 {
            let input = if s.temp < target { hold() } else { release() };
            if let Outcome::Done { score } = s.update(&P, &input) {
                return score;
            }
        }
        panic!("the step must end");
    }

    #[test]
    fn holding_the_band_makes_foam_quickly_and_scores_high() {
        assert!(hold_at(0.66) >= 0.95);
    }

    #[test]
    fn hovering_under_the_band_times_out_with_nothing() {
        assert_eq!(hold_at(0.4), 0.0);
    }

    #[test]
    fn scalding_ends_the_step_cheaply() {
        let mut s = FrothState::new(&P, &mut rng());
        s.update(&P, &press());
        let mut result = None;
        for _ in 0..10_000 {
            if let Outcome::Done { score } = s.update(&P, &hold()) {
                result = Some(score);
                break;
            }
        }
        let score = result.expect("scald must end the step");
        assert!(s.temp >= P.scald);
        assert!(score <= SCALD_FACTOR, "{score}");
    }

    #[test]
    fn dithering_in_and_out_of_the_band_costs_efficiency() {
        // Oscillate across the whole band and well below it.
        let mut s = FrothState::new(&P, &mut rng());
        s.update(&P, &press());
        let mut heating = true;
        for _ in 0..100_000 {
            if s.temp > 0.72 {
                heating = false;
            } else if s.temp < 0.3 {
                heating = true;
            }
            let input = if heating { hold() } else { release() };
            if let Outcome::Done { score } = s.update(&P, &input) {
                assert!((0.5..0.9).contains(&score), "{score}");
                return;
            }
        }
        panic!("the step must end");
    }

    const DRIFTING: FrothParams = FrothParams { drift: 0.12, ..P };

    /// Pulse the heat towards `target(state)` until the step ends.
    fn steam(p: &FrothParams, target: impl Fn(&FrothState) -> f32) -> f32 {
        let mut s = FrothState::new(p, &mut rng());
        s.update(p, &press());
        for _ in 0..100_000 {
            let input = if s.temp < target(&s) {
                hold()
            } else {
                release()
            };
            if let Outcome::Done { score } = s.update(p, &input) {
                return score;
            }
        }
        panic!("the step must end");
    }

    #[test]
    fn the_band_drifts_once_reached() {
        let mut s = FrothState::new(&DRIFTING, &mut rng());
        let start = s.current_band(&DRIFTING);
        s.update(&DRIFTING, &press());
        for _ in 0..120 {
            s.update(&DRIFTING, &hold());
            if s.reached {
                break;
            }
        }
        assert_eq!(s.current_band(&DRIFTING), start, "still until reached");
        for _ in 0..30 {
            s.update(&DRIFTING, &release());
        }
        assert_ne!(s.current_band(&DRIFTING), start);
    }

    #[test]
    fn parking_in_the_old_spot_loses_to_following_the_band() {
        let parked = steam(&DRIFTING, |_| 0.66);
        let followed = steam(&DRIFTING, |s| {
            let (lo, hi) = s.current_band(&DRIFTING);
            0.5 * (lo + hi)
        });
        assert!(followed >= 0.9, "{followed}");
        assert!(
            parked < followed - 0.15,
            "parked {parked} vs followed {followed}"
        );
    }
}
