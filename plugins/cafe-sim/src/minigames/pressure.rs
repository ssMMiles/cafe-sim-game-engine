//! Pressure meter (tamping): hold to push the needle up, release in the green.
//! The needle accelerates the longer you hold, so the band takes judgement,
//! not just reflexes. Hitting the cap ends the step with nothing.

use super::{MeterLayout, MeterZone, MinigameInput, Outcome, shifted};

#[derive(Debug, Clone, Copy)]
pub struct PressureParams {
    /// Needle speed when you start holding, meter-lengths per second.
    pub rise_per_sec: f32,
    /// How quickly that speed grows while holding.
    pub accel_per_sec2: f32,
    /// The green band, before the per-attempt shift.
    pub band: (f32, f32),
    /// The band moves by up to ± this each attempt.
    pub jitter: f32,
    /// Over-tamped: the step ends at 0.
    pub cap: f32,
}

#[derive(Debug, Clone)]
pub struct PressureState {
    pub value: f32,
    /// This attempt's band.
    band: (f32, f32),
    held_secs: f32,
    /// Waits for a fresh press, so a button still held from the drink menu
    /// doesn't start tamping.
    started: bool,
}

/// Score at the band's edge; falls to 0 one band-width outside it.
const EDGE_SCORE: f32 = 0.6;

impl PressureState {
    pub fn new(p: &PressureParams, rng: &mut impl rand::Rng) -> Self {
        Self {
            value: 0.0,
            band: shifted(p.band, p.jitter, rng),
            held_secs: 0.0,
            started: false,
        }
    }

    pub fn meter(&self, p: &PressureParams) -> MeterLayout {
        MeterLayout::bar(true, true, "pressure")
            .with_zone(self.band.0, self.band.1, MeterZone::Good)
            .with_zone(p.cap, 1.0, MeterZone::Bad)
    }

    pub fn update(&mut self, p: &PressureParams, input: &MinigameInput) -> Outcome {
        if !self.started {
            if !input.pressed {
                return Outcome::Running;
            }
            self.started = true;
        }
        if input.held {
            self.held_secs += input.dt;
            let speed = p.rise_per_sec + p.accel_per_sec2 * self.held_secs;
            self.value = (self.value + speed * input.dt).min(1.0);
            if self.value >= p.cap {
                return Outcome::Done { score: 0.0 };
            }
            return Outcome::Running;
        }
        // Let go (this frame or a missed release event).
        Outcome::Done {
            score: score(self.band, self.value),
        }
    }
}

fn score(band: (f32, f32), v: f32) -> f32 {
    let (lo, hi) = band;
    let centre = (lo + hi) * 0.5;
    let half = (hi - lo) * 0.5;
    let d = (v - centre).abs();
    if d <= half {
        1.0 - (1.0 - EDGE_SCORE) * d / half
    } else {
        let width = hi - lo;
        (EDGE_SCORE * (1.0 - (d - half) / width)).max(0.0)
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_util::*;
    use super::*;

    const P: PressureParams = PressureParams {
        rise_per_sec: 0.25,
        accel_per_sec2: 0.35,
        band: (0.62, 0.74),
        jitter: 0.0,
        cap: 0.92,
    };

    /// Hold until the needle passes `target`, release, return the score.
    fn tamp_to(target: f32) -> f32 {
        let mut s = PressureState::new(&P, &mut rng());
        assert_eq!(s.update(&P, &press()), Outcome::Running);
        for _ in 0..10_000 {
            if s.value >= target {
                break;
            }
            if let Outcome::Done { score } = s.update(&P, &hold()) {
                return score;
            }
        }
        match s.update(&P, &release()) {
            Outcome::Done { score } => score,
            Outcome::Running => panic!("release must end the step"),
        }
    }

    #[test]
    fn waits_for_a_fresh_press() {
        let mut s = PressureState::new(&P, &mut rng());
        for _ in 0..60 {
            assert_eq!(s.update(&P, &hold()), Outcome::Running);
        }
        assert_eq!(s.value, 0.0);
    }

    #[test]
    fn centre_scores_high_and_misses_score_low() {
        assert!(tamp_to(0.68) >= 0.95);
        assert!(tamp_to(0.3) <= 0.2);
        let edge = tamp_to(0.745);
        assert!((0.4..=0.65).contains(&edge), "{edge}");
    }

    #[test]
    fn over_tamping_ends_at_zero() {
        assert_eq!(tamp_to(2.0), 0.0);
    }
}
