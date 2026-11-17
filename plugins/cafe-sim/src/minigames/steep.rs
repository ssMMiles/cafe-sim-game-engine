//! Extraction window (steeping tea, pulling a shot): a bar fills on its own,
//! click to stop it inside the green. Letting it run out over-extracts.

use super::{MeterLayout, MeterZone, MinigameInput, Outcome, Prop, band_score, shifted};

#[derive(Debug, Clone, Copy)]
pub struct SteepParams {
    /// Time for the bar to fill.
    pub duration_secs: f32,
    /// The sweet spot along the bar, before the per-attempt shift.
    pub window: (f32, f32),
    /// The window moves by up to ± this each attempt.
    pub jitter: f32,
}

#[derive(Debug, Clone)]
pub struct SteepState {
    pub progress: f32,
    /// This attempt's window.
    window: (f32, f32),
}

impl SteepState {
    pub fn new(p: &SteepParams, rng: &mut impl rand::Rng) -> Self {
        Self {
            progress: 0.0,
            window: shifted(p.window, p.jitter, rng),
        }
    }

    pub fn meter(&self) -> MeterLayout {
        MeterLayout::bar(false, false, "time")
            .dressed_as(Prop::Strip)
            .with_zone(self.window.0, self.window.1, MeterZone::Good)
    }

    pub fn update(&mut self, p: &SteepParams, input: &MinigameInput) -> Outcome {
        if input.pressed {
            return Outcome::Done {
                score: band_score(self.progress, self.window, 0.0, 1.0),
            };
        }
        self.progress = (self.progress + input.dt / p.duration_secs).min(1.0);
        if self.progress >= 1.0 {
            return Outcome::Done { score: 0.0 };
        }
        Outcome::Running
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_util::*;
    use super::*;

    const P: SteepParams = SteepParams {
        duration_secs: 3.0,
        window: (0.55, 0.72),
        jitter: 0.0,
    };

    fn stop_at(target: f32) -> f32 {
        let mut s = SteepState::new(&P, &mut rng());
        while s.progress < target {
            if let Outcome::Done { score } = s.update(&P, &idle()) {
                return score;
            }
        }
        match s.update(&P, &press()) {
            Outcome::Done { score } => score,
            Outcome::Running => panic!("a click must end the step"),
        }
    }

    #[test]
    fn inside_the_window_is_full_marks() {
        assert_eq!(stop_at(0.6), 1.0);
        assert!(stop_at(0.2) < 0.4);
        assert!(stop_at(0.9) < 0.4);
    }

    #[test]
    fn running_out_scores_zero() {
        assert_eq!(stop_at(2.0), 0.0);
    }
}
