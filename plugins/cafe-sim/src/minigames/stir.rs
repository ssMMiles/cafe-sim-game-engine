//! Whisk: circle the cursor round the dish at a steady speed until it's
//! mixed. A speed meter shows how fast you're going against a green band;
//! the score is the share of your stirring spent in it. Too slow just takes
//! longer out of the green. Too fast builds slosh, and slosh reaching the top
//! is a spill: the score is capped and the stir carries on.

use std::f32::consts::PI;

use bevy::math::Vec2;

use super::{MeterLayout, MeterReading, MeterShape, MeterZone, MinigameInput, Outcome};

#[derive(Debug, Clone, Copy)]
pub struct StirParams {
    /// Mixing gained per radian swept.
    pub mix_per_rad: f32,
    /// Top of the green band (rad/s). Faster than this builds slosh.
    pub safe_speed: f32,
    /// Bottom of the green band, as a share of `safe_speed`.
    pub good_from: f32,
    /// Slosh gained per second per (rad/s)² over the safe speed.
    pub slosh_gain: f32,
    /// Slosh lost per second while at or under the safe speed.
    pub slosh_decay: f32,
    /// Give up after this long stirring; the score is scaled by the mix.
    pub max_secs: f32,
}

#[derive(Debug, Clone)]
pub struct StirState {
    pub mixed: f32,
    pub slosh: f32,
    /// Smoothed angular speed, rad/s.
    pub speed: f32,
    spilled: bool,
    /// Time since the first sweep; the clock doesn't run while you read.
    stirring_secs: f32,
    /// Time inside the green band, after the warm-up.
    green_secs: f32,
    last_angle: Option<f32>,
}

/// Closer to the centre than this the angle is too noisy to count.
const DEAD_RADIUS: f32 = 0.12;
/// The dish's rim: stirring outside it does nothing.
const RIM_RADIUS: f32 = 0.5;
/// A single frame can't sweep more than this (cursor jumps aren't stirring).
const MAX_SWEEP_PER_FRAME: f32 = PI / 2.0;
/// Smoothing for the speed meter: mouse input is jittery frame to frame.
const SPEED_SMOOTHING_SECS: f32 = 0.15;
/// Getting up to speed isn't judged.
const WARMUP_SECS: f32 = 0.3;
/// The speed meter's top is this many times the safe speed.
const SPEED_SCALE: f32 = 1.5;
/// The best score after a spill.
const SPILL_CAP: f32 = 0.3;
/// Slosh falls back to here after a spill, so one spill isn't two.
const AFTER_SPILL: f32 = 0.5;

impl StirState {
    pub fn new(_p: &StirParams, _rng: &mut impl rand::Rng) -> Self {
        Self {
            mixed: 0.0,
            slosh: 0.0,
            speed: 0.0,
            spilled: false,
            stirring_secs: 0.0,
            green_secs: 0.0,
            last_angle: None,
        }
    }

    pub fn meters(&self, p: &StirParams) -> Vec<MeterLayout> {
        let (lo, hi) = green_on_meter(p);
        vec![
            MeterLayout {
                shape: MeterShape::Dish,
                zones: Vec::new(),
                cursor: true,
                caption: "whisk",
            },
            MeterLayout::bar(true, true, "speed")
                .with_zone(lo, hi, MeterZone::Good)
                .with_zone(hi, 1.0, MeterZone::Bad),
            MeterLayout::bar(true, true, "mixed"),
            MeterLayout::bar(true, true, "slosh").with_zone(0.85, 1.0, MeterZone::Bad),
        ]
    }

    pub fn readings(&self, p: &StirParams) -> Vec<MeterReading> {
        let speed = self.speed / (SPEED_SCALE * p.safe_speed);
        [0.0, speed, self.mixed, self.slosh]
            .into_iter()
            .map(MeterReading::from)
            .collect()
    }

    /// What to do now, and how it's going.
    pub fn status(&self) -> Option<String> {
        if self.stirring_secs <= 0.0 {
            return Some("Start circling the cursor round the dish".into());
        }
        if self.stirring_secs < WARMUP_SECS {
            return Some("Get up to speed...".into());
        }
        Some(format!(
            "In the green {:.0}% of the time",
            self.green_share() * 100.0
        ))
    }

    pub fn update(&mut self, p: &StirParams, input: &MinigameInput) -> Outcome {
        let swept = self.sweep(input.cursor);
        if input.dt <= 0.0 || (self.stirring_secs <= 0.0 && swept <= 0.0) {
            return Outcome::Running;
        }
        self.stirring_secs += input.dt;

        let raw = swept / input.dt;
        let k = 1.0 - (-input.dt / SPEED_SMOOTHING_SECS).exp();
        self.speed += (raw - self.speed) * k;
        if self.stirring_secs > WARMUP_SECS
            && (p.good_from * p.safe_speed..=p.safe_speed).contains(&self.speed)
        {
            self.green_secs += input.dt;
        }

        self.mixed = (self.mixed + p.mix_per_rad * swept).min(1.0);
        let over = self.speed - p.safe_speed;
        if over > 0.0 {
            self.slosh += p.slosh_gain * over * over * input.dt;
        } else {
            self.slosh -= p.slosh_decay * input.dt;
        }
        self.slosh = self.slosh.clamp(0.0, 1.0);
        if self.slosh >= 1.0 {
            self.spilled = true;
            self.slosh = AFTER_SPILL;
        }

        if self.mixed >= 1.0 {
            return Outcome::Done {
                score: self.score(),
            };
        }
        if self.stirring_secs >= p.max_secs {
            return Outcome::Done {
                score: self.score() * self.mixed,
            };
        }
        Outcome::Running
    }

    /// Share of the stirring (after warm-up) spent in the green band.
    fn green_share(&self) -> f32 {
        let judged = self.stirring_secs - WARMUP_SECS;
        if judged > 0.0 {
            (self.green_secs / judged).clamp(0.0, 1.0)
        } else {
            1.0
        }
    }

    fn score(&self) -> f32 {
        let score = self.green_share();
        if self.spilled {
            score.min(SPILL_CAP)
        } else {
            score
        }
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
}

/// The green band on the speed meter, 0..1.
fn green_on_meter(p: &StirParams) -> (f32, f32) {
    (p.good_from / SPEED_SCALE, 1.0 / SPEED_SCALE)
}

#[cfg(test)]
mod tests {
    use super::super::test_util::*;
    use super::*;

    const P: StirParams = StirParams {
        mix_per_rad: 1.0 / (6.0 * 2.0 * PI), // six turns
        safe_speed: 2.0 * PI * 1.5,          // 1.5 turns a second
        good_from: 0.6,
        slosh_gain: 0.01,
        slosh_decay: 0.4,
        max_secs: 15.0,
    };

    /// Circle at `turns_per_sec` until the step ends; the score and state.
    fn stir(turns_per_sec: f32) -> (f32, StirState) {
        let mut s = StirState::new(&P, &mut rng());
        let mut angle = 0.0_f32;
        for _ in 0..100_000 {
            angle += 2.0 * PI * turns_per_sec * DT;
            let c = Vec2::splat(0.5) + 0.35 * Vec2::new(angle.cos(), angle.sin());
            if let Outcome::Done { score } = s.update(&P, &at(hold(), c)) {
                return (score, s);
            }
        }
        panic!("the stir must finish");
    }

    #[test]
    fn a_steady_stir_in_the_green_scores_high() {
        let (score, s) = stir(1.2); // 0.8 of the safe speed
        assert!(score >= 0.95, "{score}");
        assert!(!s.spilled && s.slosh < 1e-3);
        // ...and the meter needle sits inside the drawn green band.
        let (lo, hi) = green_on_meter(&P);
        let needle = s.readings(&P)[1].value;
        assert!((lo..=hi).contains(&needle), "{needle} not in {lo}..{hi}");
    }

    #[test]
    fn too_slow_is_out_of_the_green() {
        let (score, s) = stir(0.3); // well under the green: times out part mixed
        assert!(s.mixed < 1.0);
        assert!(score <= 0.05, "{score}");
    }

    #[test]
    fn a_bit_fast_builds_slosh_and_frantic_spills() {
        let (_, calm) = stir(1.2);
        let (_, quick) = stir(1.8);
        assert!(calm.slosh < 1e-3 && quick.slosh > 0.0);
        let (score, frantic) = stir(4.0);
        assert!(frantic.spilled && score <= SPILL_CAP, "{score}");
    }

    #[test]
    fn the_clock_waits_for_the_first_sweep() {
        let mut s = StirState::new(&P, &mut rng());
        for _ in 0..600 {
            s.update(&P, &at(idle(), Vec2::new(0.85, 0.5)));
        }
        assert_eq!(s.stirring_secs, 0.0);
        assert!(s.status().unwrap().starts_with("Start"));
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
