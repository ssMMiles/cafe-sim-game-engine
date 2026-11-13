//! Scoop to the line (tea leaves, coffee grounds, cocoa). Hold to fill the
//! scoop, let go to tip it into the cup, press Done when the cup looks right.
//!
//! The scoop has its own bar beside the cup's, at its own scale: a full scoop
//! is only `capacity` of a cup, and that capacity is rolled per attempt. So
//! the scoop bar tells you how full the scoop is, never how far it will lift
//! the cup — the first scoop shows you the ratio, the rest is your judgement.
//! A partial last scoop is how you land in the band. Nothing comes back out.
//!
//! Scoops are limited: as many full ones as this attempt's band needs, plus
//! `spare_scoops`. That leaves room for one top-up, not for creeping up on
//! the line with a dozen tiny ones. The last scoop ends the step.

use rand::RngExt;

use super::{MeterLayout, MeterZone, MinigameInput, Outcome, band_score, shifted};

#[derive(Debug, Clone, Copy)]
pub struct ScoopParams {
    /// A full scoop, in cup-heights, before the per-attempt roll.
    pub capacity: f32,
    /// Each attempt's scoop is `capacity × (1 ± capacity_jitter)`.
    pub capacity_jitter: f32,
    /// Seconds of holding to fill an empty scoop.
    pub fill_secs: f32,
    /// Target amount in the cup, before the per-attempt shift.
    pub band: (f32, f32),
    /// The band moves by up to ± this each attempt.
    pub jitter: f32,
    /// Scoops allowed beyond the full ones the band needs.
    pub spare_scoops: u32,
}

#[derive(Debug, Clone)]
pub struct ScoopState {
    /// In the cup, cup-heights.
    pub amount: f32,
    /// How full the scoop in hand is, 0..1 of *its* capacity.
    pub in_scoop: f32,
    /// This attempt's full scoop, cup-heights.
    capacity: f32,
    /// This attempt's band.
    band: (f32, f32),
    /// Filling the scoop since a fresh press.
    filling: bool,
    /// Scoops tipped in so far, and how many this attempt allows.
    scoops: u32,
    max_scoops: u32,
}

/// Under the band, the score falls to 0 this far below it.
const UNDER_RANGE: f32 = 0.3;
/// Over it falls off faster: you can't take any back out.
const OVER_RANGE: f32 = 0.15;

impl ScoopState {
    pub fn new(p: &ScoopParams, rng: &mut impl rand::Rng) -> Self {
        let roll = if p.capacity_jitter > 0.0 {
            rng.random_range(-p.capacity_jitter..=p.capacity_jitter)
        } else {
            0.0
        };
        let capacity = p.capacity * (1.0 + roll);
        let band = shifted(p.band, p.jitter, rng);
        let full_scoops = (0.5 * (band.0 + band.1) / capacity).ceil() as u32;
        Self {
            amount: 0.0,
            in_scoop: 0.0,
            capacity,
            band,
            filling: false,
            scoops: 0,
            max_scoops: full_scoops + p.spare_scoops,
        }
    }

    pub fn scoops_left(&self) -> u32 {
        self.max_scoops - self.scoops
    }

    pub fn status(&self) -> String {
        match self.scoops_left() {
            1 => "Last scoop!".into(),
            n => format!("Scoops left: {n}"),
        }
    }

    pub fn meters(&self) -> Vec<MeterLayout> {
        vec![
            MeterLayout::bar(true, true, "cup").with_zone(
                self.band.0,
                self.band.1,
                MeterZone::Good,
            ),
            MeterLayout::bar(true, true, "scoop"),
        ]
    }

    pub fn update(
        &mut self,
        p: &ScoopParams,
        input: &MinigameInput,
        _rng: &mut impl rand::Rng,
    ) -> Outcome {
        if input.confirm {
            self.tip();
            return self.done();
        }
        if input.pressed {
            self.filling = true;
        }
        if self.filling {
            if input.held {
                self.in_scoop = (self.in_scoop + input.dt / p.fill_secs).min(1.0);
            } else {
                self.tip();
                if self.scoops_left() == 0 {
                    return self.done();
                }
            }
        }
        Outcome::Running
    }

    fn done(&self) -> Outcome {
        Outcome::Done {
            score: band_score(
                self.amount,
                self.band,
                self.band.0 - UNDER_RANGE,
                self.band.1 + OVER_RANGE,
            ),
        }
    }

    /// Empty the scoop into the cup. An empty scoop doesn't count.
    fn tip(&mut self) {
        if self.in_scoop > 0.0 {
            self.amount = (self.amount + self.in_scoop * self.capacity).min(1.0);
            self.scoops += 1;
        }
        self.in_scoop = 0.0;
        self.filling = false;
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_util::*;
    use super::*;

    const P: ScoopParams = ScoopParams {
        capacity: 0.2,
        capacity_jitter: 0.0,
        fill_secs: 1.0,
        band: (0.55, 0.6),
        jitter: 0.0,
        spare_scoops: 1,
    };

    fn confirm() -> MinigameInput {
        MinigameInput {
            confirm: true,
            ..idle()
        }
    }

    /// One scoop filled for `secs`, then tipped.
    fn scoop(s: &mut ScoopState, secs: f32) {
        s.update(&P, &press(), &mut rng());
        let frames = (secs / DT).round() as usize;
        for _ in 1..frames {
            s.update(&P, &hold(), &mut rng());
        }
        s.update(&P, &release(), &mut rng());
    }

    fn done(s: &mut ScoopState) -> f32 {
        match s.update(&P, &confirm(), &mut rng()) {
            Outcome::Done { score } => score,
            Outcome::Running => panic!("Done must end the step"),
        }
    }

    #[test]
    fn holding_fills_the_scoop_and_letting_go_tips_it() {
        let mut s = ScoopState::new(&P, &mut rng());
        s.update(&P, &press(), &mut rng());
        for _ in 0..29 {
            s.update(&P, &hold(), &mut rng());
        }
        assert!((s.in_scoop - 0.5).abs() < 0.02, "{}", s.in_scoop);
        assert_eq!(s.amount, 0.0);
        s.update(&P, &release(), &mut rng());
        assert_eq!(s.in_scoop, 0.0);
        assert!((s.amount - 0.1).abs() < 0.005, "{}", s.amount);
    }

    #[test]
    fn a_full_scoop_stops_at_its_capacity() {
        let mut s = ScoopState::new(&P, &mut rng());
        scoop(&mut s, 3.0);
        assert!((s.amount - P.capacity).abs() < 1e-5);
    }

    #[test]
    fn full_scoops_plus_a_partial_one_lands_in_the_band() {
        // 2 × 0.2 + 0.8 × 0.2 = 0.56
        let mut s = ScoopState::new(&P, &mut rng());
        scoop(&mut s, 1.5);
        scoop(&mut s, 1.5);
        scoop(&mut s, 0.8);
        assert!(done(&mut s) >= 0.95);
    }

    #[test]
    fn whole_scoops_alone_miss() {
        // 0.4 under, 0.6 at the band's top edge, 0.8 well over.
        let mut s = ScoopState::new(&P, &mut rng());
        scoop(&mut s, 1.5);
        scoop(&mut s, 1.5);
        assert!(done(&mut s) <= 0.55);

        let mut s = ScoopState::new(&P, &mut rng());
        for _ in 0..4 {
            scoop(&mut s, 1.5);
        }
        assert!(done(&mut s) <= 0.2);
    }

    #[test]
    fn the_scoop_budget_is_the_full_scoops_needed_plus_spares() {
        // Band centre 0.575 / capacity 0.2 → 3 full scoops, + 1 spare.
        let s = ScoopState::new(&P, &mut rng());
        assert_eq!(s.scoops_left(), 4);
    }

    #[test]
    fn the_last_scoop_ends_the_step() {
        let mut s = ScoopState::new(&P, &mut rng());
        for _ in 0..3 {
            scoop(&mut s, 0.3);
        }
        s.update(&P, &press(), &mut rng());
        for _ in 0..10 {
            s.update(&P, &hold(), &mut rng());
        }
        // Tiny scoops add up to far too little, and the fourth one ends it.
        match s.update(&P, &release(), &mut rng()) {
            Outcome::Done { score } => assert!(score <= 0.2, "{score}"),
            Outcome::Running => panic!("the last scoop must end the step"),
        }
    }

    #[test]
    fn done_while_filling_tips_the_scoop_first() {
        let mut s = ScoopState::new(&P, &mut rng());
        s.update(&P, &press(), &mut rng());
        for _ in 0..59 {
            s.update(&P, &hold(), &mut rng());
        }
        done(&mut s);
        assert!((s.amount - P.capacity).abs() < 0.005);
    }

    #[test]
    fn capacity_is_rolled_per_attempt() {
        let p = ScoopParams {
            capacity_jitter: 0.3,
            ..P
        };
        let mut rng = rng();
        let caps: Vec<f32> = (0..20)
            .map(|_| ScoopState::new(&p, &mut rng).capacity)
            .collect();
        assert!(caps.iter().all(|c| (0.14..=0.26).contains(c)));
        assert!(caps.iter().any(|&c| (c - caps[0]).abs() > 0.01));
    }
}
