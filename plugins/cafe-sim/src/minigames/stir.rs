//! Whisk: circle the cursor round the dish. Angle swept mixes the drink;
//! sweeping faster than the safe speed builds slosh, which settles again when
//! you slow down. Slosh reaching the top is a spill — the score is capped and
//! the stir carries on. Steady beats frantic: the score is pace × calmness.

use std::f32::consts::PI;

use bevy::math::Vec2;

use super::{MeterLayout, MeterShape, MeterZone, MinigameInput, Outcome};

#[derive(Debug, Clone, Copy)]
pub struct StirParams {
    /// Mixing gained per radian swept.
    pub mix_per_rad: f32,
    /// Sweeping faster than this (rad/s) builds slosh.
    pub safe_speed: f32,
    /// Slosh gained per second per (rad/s)² over the safe speed.
    pub slosh_gain: f32,
    /// Slosh lost per second while under the safe speed.
    pub slosh_decay: f32,
    /// Finishing within this is full pace marks.
    pub ideal_secs: f32,
    /// At this long the pace mark is 0.
    pub max_secs: f32,
}

#[derive(Debug, Clone)]
pub struct StirState {
    pub mixed: f32,
    pub slosh: f32,
    slosh_peak: f32,
    spilled: bool,
    elapsed: f32,
    last_angle: Option<f32>,
}

/// Closer to the centre than this the angle is too noisy to count.
const DEAD_RADIUS: f32 = 0.12;
/// The dish's rim: stirring outside it does nothing.
const RIM_RADIUS: f32 = 0.5;
/// A single frame can't sweep more than this (cursor jumps aren't stirring).
const MAX_SWEEP_PER_FRAME: f32 = PI / 2.0;
/// The best score after a spill.
const SPILL_CAP: f32 = 0.3;
/// Slosh falls back to here after a spill, so one spill isn't two.
const AFTER_SPILL: f32 = 0.5;

impl StirState {
    pub fn new(_p: &StirParams, _rng: &mut impl rand::Rng) -> Self {
        Self {
            mixed: 0.0,
            slosh: 0.0,
            slosh_peak: 0.0,
            spilled: false,
            elapsed: 0.0,
            last_angle: None,
        }
    }

    pub fn meters(&self) -> Vec<MeterLayout> {
        vec![
            MeterLayout {
                shape: MeterShape::Dish,
                zones: Vec::new(),
                cursor: true,
                caption: "whisk",
            },
            MeterLayout::bar(true, true, "mixed"),
            MeterLayout::bar(true, true, "slosh").with_zone(0.85, 1.0, MeterZone::Bad),
        ]
    }

    pub fn update(&mut self, p: &StirParams, input: &MinigameInput) -> Outcome {
        self.elapsed += input.dt;
        let swept = self.sweep(input.cursor);
        if input.dt <= 0.0 {
            return Outcome::Running;
        }
        let speed = swept / input.dt;
        self.mixed = (self.mixed + p.mix_per_rad * swept).min(1.0);
        let over = speed - p.safe_speed;
        if over > 0.0 {
            self.slosh += p.slosh_gain * over * over * input.dt;
        } else {
            self.slosh -= p.slosh_decay * input.dt;
        }
        self.slosh = self.slosh.clamp(0.0, 1.0);
        self.slosh_peak = self.slosh_peak.max(self.slosh);
        if self.slosh >= 1.0 {
            self.spilled = true;
            self.slosh = AFTER_SPILL;
        }
        if self.mixed >= 1.0 {
            return Outcome::Done {
                score: self.score(p),
            };
        }
        Outcome::Running
    }

    /// Unsigned angle swept around the dish's centre since last frame.
    fn sweep(&mut self, cursor: Option<Vec2>) -> f32 {
        let Some(c) = cursor else {
            self.last_angle = None;
            return 0.0;
        };
        let d = c - Vec2::splat(0.5);
        let r = d.length();
        if !(DEAD_RADIUS..=RIM_RADIUS).contains(&r) {
            self.last_angle = None;
            return 0.0;
        }
        let angle = d.y.atan2(d.x);
        let swept = match self.last_angle {
            Some(last) => {
                let mut delta = angle - last;
                if delta > PI {
                    delta -= 2.0 * PI;
                } else if delta < -PI {
                    delta += 2.0 * PI;
                }
                delta.abs().min(MAX_SWEEP_PER_FRAME)
            }
            None => 0.0,
        };
        self.last_angle = Some(angle);
        swept
    }

    fn score(&self, p: &StirParams) -> f32 {
        let pace = ((p.max_secs - self.elapsed) / (p.max_secs - p.ideal_secs)).clamp(0.0, 1.0);
        let calm = 1.0 - self.slosh_peak;
        let score = pace * calm;
        if self.spilled {
            score.min(SPILL_CAP)
        } else {
            score
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_util::*;
    use super::*;

    const P: StirParams = StirParams {
        mix_per_rad: 1.0 / (6.0 * 2.0 * PI), // six turns
        safe_speed: 2.0 * PI * 1.5,          // 1.5 turns a second
        slosh_gain: 0.01,
        slosh_decay: 0.4,
        ideal_secs: 4.5,
        max_secs: 12.0,
    };

    /// Circle at `turns_per_sec` until mixed; returns the score and the
    /// slosh peak reached.
    fn stir(turns_per_sec: f32) -> (f32, f32) {
        let mut s = StirState::new(&P, &mut rng());
        let mut angle = 0.0_f32;
        for _ in 0..100_000 {
            angle += 2.0 * PI * turns_per_sec * DT;
            let c = Vec2::splat(0.5) + 0.35 * Vec2::new(angle.cos(), angle.sin());
            if let Outcome::Done { score } = s.update(&P, &at(hold(), c)) {
                return (score, s.slosh_peak);
            }
        }
        panic!("the stir must finish");
    }

    #[test]
    fn a_steady_safe_stir_scores_high() {
        let (score, peak) = stir(1.45);
        assert!(peak < 1e-3);
        assert!(score >= 0.95, "{score}");
    }

    #[test]
    fn slosh_grows_with_speed_and_a_frantic_stir_spills() {
        let (_, p1) = stir(1.8);
        let (_, p2) = stir(2.0);
        let (score, p4) = stir(4.0);
        assert!(p1 < p2 && p2 < p4, "{p1} {p2} {p4}");
        assert!(score <= SPILL_CAP, "{score}");
    }

    #[test]
    fn a_slow_stir_loses_pace() {
        let (score, _) = stir(0.5); // 12 s: at the limit
        assert!(score <= 0.05, "{score}");
    }

    #[test]
    fn jumping_across_the_dish_counts_as_frantic() {
        let mut s = StirState::new(&P, &mut rng());
        for i in 0..600 {
            let x = if i % 2 == 0 { 0.15 } else { 0.85 };
            if let Outcome::Done { score } = s.update(&P, &at(hold(), Vec2::new(x, 0.5))) {
                assert!(s.spilled && score <= SPILL_CAP, "{score}");
                return;
            }
        }
        panic!("the stir must finish");
    }

    #[test]
    fn outside_the_dish_does_nothing() {
        let mut s = StirState::new(&P, &mut rng());
        for i in 0..100 {
            let a = i as f32 * 0.3;
            let c = Vec2::splat(0.5) + 0.7 * Vec2::new(a.cos(), a.sin());
            s.update(&P, &at(hold(), c));
        }
        assert_eq!(s.mixed, 0.0);
    }
}
