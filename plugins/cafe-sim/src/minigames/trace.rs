//! Trace a path (latte art, cocoa dusting): press on the start dot and,
//! holding, follow the dotted path to its end. Letting go lifts the jug and
//! ends the step with what you've drawn so far.
//!
//! Progress only ever moves forward, and only to path within a short reach
//! ahead of where you are, so cutting across the shape skips nothing. The
//! score is how much of the path you drew × how close to it you stayed.
//!
//! What the player actually drew is kept as a trail of graded points the
//! overlay draws over the path, and a draining bar shows the jug running dry.

use bevy::math::Vec2;

use super::{Grade, MeterLayout, MeterShape, MinigameInput, Outcome, Prop, TrailPoint};

#[derive(Debug, Clone, Copy)]
pub struct TraceParams {
    /// The shape, 0..1 on both axes, y up. Drawn from the first point.
    pub path: &'static [Vec2],
    /// Mean distance from the path at which accuracy reaches 0, in canvas
    /// widths.
    pub tolerance: f32,
    /// Time from the first press until the jug runs dry.
    pub max_secs: f32,
}

#[derive(Debug, Clone)]
pub struct TraceState {
    /// Arc length drawn so far, canvas widths.
    progress: f32,
    total: f32,
    /// ∫ distance-from-path d(arc length) over what's been drawn.
    deviation: f32,
    elapsed: f32,
    started: bool,
    /// Where the cursor went while drawing, at most [`TRAIL_MAX`] points.
    trail: Vec<TrailPoint>,
}

/// How near the first point a press must land to start drawing.
const START_RADIUS: f32 = 0.08;
/// How far ahead along the path the cursor can pull the progress per frame.
const REACH: f32 = 0.12;
/// A drawn point is recorded once the cursor is this far from the last one.
const TRAIL_SPACING: f32 = 0.012;
const TRAIL_MAX: usize = 240;
/// Drawn within this share of the tolerance: on the line.
const GOOD_SHARE: f32 = 0.5;

impl TraceState {
    pub fn new(p: &TraceParams, _rng: &mut impl rand::Rng) -> Self {
        Self {
            progress: 0.0,
            total: length(p.path),
            deviation: 0.0,
            elapsed: 0.0,
            started: false,
            trail: Vec::new(),
        }
    }

    pub fn meters(&self, p: &TraceParams) -> Vec<MeterLayout> {
        vec![
            MeterLayout {
                shape: MeterShape::Canvas { path: p.path },
                zones: Vec::new(),
                cursor: true,
                caption: "pour",
                prop: Prop::LatteCup,
            },
            MeterLayout::bar(true, true, "milk").dressed_as(Prop::Jug),
        ]
    }

    /// Share of the jug left, 1 until the first press.
    pub fn time_left(&self, p: &TraceParams) -> f32 {
        (1.0 - self.elapsed / p.max_secs).clamp(0.0, 1.0)
    }

    pub fn trail(&self) -> &[TrailPoint] {
        &self.trail
    }

    pub fn status(&self, p: &TraceParams) -> String {
        if !self.started {
            return "Press on the gold dot to start pouring".into();
        }
        format!(
            "On the line {:.0}%   -   {:.1} s left",
            self.accuracy(p) * 100.0,
            (p.max_secs - self.elapsed).max(0.0),
        )
    }

    /// How far along the path the drawing has got, 0..1.
    pub fn fraction(&self) -> f32 {
        if self.total > 0.0 {
            self.progress / self.total
        } else {
            0.0
        }
    }

    pub fn update(&mut self, p: &TraceParams, input: &MinigameInput) -> Outcome {
        if !self.started {
            let on_start = input
                .cursor
                .is_some_and(|c| c.distance(p.path[0]) <= START_RADIUS);
            self.started = input.pressed && on_start;
            return Outcome::Running;
        }
        self.elapsed += input.dt;
        if !input.held || self.elapsed >= p.max_secs {
            return self.done(p);
        }
        if let Some(c) = input.cursor {
            self.record(p, c);
        }
        if let Some(c) = input.cursor
            && let Some((s, d)) = nearest_ahead(p.path, c, self.progress, REACH)
            && d <= 2.0 * p.tolerance
            && s > self.progress
        {
            self.deviation += d * (s - self.progress);
            self.progress = s;
        }
        if self.progress >= self.total - 1e-4 {
            return self.done(p);
        }
        Outcome::Running
    }

    /// Add `c` to the trail if it has moved far enough, graded by its
    /// distance to the nearest point anywhere on the path.
    fn record(&mut self, p: &TraceParams, c: Vec2) {
        if self.trail.len() >= TRAIL_MAX
            || self
                .trail
                .last()
                .is_some_and(|t| t.at.distance(c) < TRAIL_SPACING)
        {
            return;
        }
        let d = nearest_ahead(p.path, c, 0.0, self.total).map_or(f32::INFINITY, |(_, d)| d);
        let grade = if d <= GOOD_SHARE * p.tolerance {
            Grade::Good
        } else if d <= p.tolerance {
            Grade::Off
        } else {
            Grade::Bad
        };
        self.trail.push(TrailPoint { at: c, grade });
    }

    /// 1 for drawing exactly on the line, falling to 0 at the tolerance.
    fn accuracy(&self, p: &TraceParams) -> f32 {
        if self.progress > 0.0 {
            (1.0 - self.deviation / self.progress / p.tolerance).clamp(0.0, 1.0)
        } else {
            1.0
        }
    }

    fn done(&self, p: &TraceParams) -> Outcome {
        let accuracy = if self.progress > 0.0 {
            self.accuracy(p)
        } else {
            0.0
        };
        Outcome::Done {
            score: self.fraction() * accuracy,
        }
    }
}

fn length(path: &[Vec2]) -> f32 {
    path.windows(2).map(|w| w[0].distance(w[1])).sum()
}

/// The point `fraction` (0..1) of the way along `path` by arc length.
pub fn point_along(path: &[Vec2], fraction: f32) -> Vec2 {
    let mut left = fraction.clamp(0.0, 1.0) * length(path);
    for w in path.windows(2) {
        let len = w[0].distance(w[1]);
        if left <= len && len > 0.0 {
            return w[0].lerp(w[1], left / len);
        }
        left -= len;
    }
    *path.last().expect("a path has points")
}

/// The point on `path` nearest `c` with arc length in
/// `[from, from + reach]`: its arc length and its distance from `c`.
fn nearest_ahead(path: &[Vec2], c: Vec2, from: f32, reach: f32) -> Option<(f32, f32)> {
    let mut best: Option<(f32, f32)> = None;
    let mut start = 0.0;
    for w in path.windows(2) {
        let len = w[0].distance(w[1]);
        let (lo, hi) = (from.max(start), (from + reach).min(start + len));
        if len > 0.0 && lo <= hi {
            // Closest point on the segment's line, kept inside the window.
            let t = (c - w[0]).dot(w[1] - w[0]) / (len * len);
            let s = (start + t * len).clamp(lo, hi);
            let d = c.distance(w[0].lerp(w[1], (s - start) / len));
            if best.is_none_or(|(_, bd)| d < bd) {
                best = Some((s, d));
            }
        }
        start += len;
    }
    best
}

#[cfg(test)]
mod tests {
    use super::super::test_util::*;
    use super::*;

    /// An L: right along the bottom, then up.
    const L: &[Vec2] = &[
        Vec2::new(0.1, 0.1),
        Vec2::new(0.9, 0.1),
        Vec2::new(0.9, 0.9),
    ];
    const P: TraceParams = TraceParams {
        path: L,
        tolerance: 0.06,
        max_secs: 6.0,
    };

    /// Start on the first point, then follow `cursor(fraction)` at a steady
    /// pace over `secs` (holding throughout), and return the score.
    fn draw(secs: f32, cursor: impl Fn(f32) -> Vec2) -> f32 {
        let mut s = TraceState::new(&P, &mut rng());
        assert_eq!(s.update(&P, &at(press(), L[0])), Outcome::Running);
        let frames = (secs / DT) as usize;
        for f in 1..=frames + 600 {
            let frac = (f as f32 / frames as f32).min(1.0);
            if let Outcome::Done { score } = s.update(&P, &at(hold(), cursor(frac))) {
                return score;
            }
        }
        panic!("the step must end");
    }

    #[test]
    fn point_along_walks_the_path_by_arc_length() {
        assert_eq!(point_along(L, 0.0), L[0]);
        assert!(point_along(L, 0.5).distance(L[1]) < 1e-5);
        assert!(point_along(L, 0.75).distance(Vec2::new(0.9, 0.5)) < 1e-5);
        assert_eq!(point_along(L, 1.0), L[2]);
    }

    #[test]
    fn following_the_path_exactly_scores_full_marks() {
        assert!(draw(2.0, |f| point_along(L, f)) >= 0.99);
    }

    #[test]
    fn drifting_off_the_path_costs_accuracy() {
        // Half the tolerance outside the L the whole way.
        let off = |f: f32| {
            let q = point_along(L, f);
            q + if q.x < 0.9 - 1e-4 {
                Vec2::new(0.0, -0.03)
            } else {
                Vec2::new(0.03, 0.0)
            }
        };
        let score = draw(2.0, off);
        assert!((0.4..=0.6).contains(&score), "{score}");
    }

    #[test]
    fn cutting_the_corner_skips_nothing() {
        // Straight from the start to the end: the corner is never drawn.
        let score = draw(2.0, |f| L[0].lerp(L[2], f));
        assert!(score <= 0.2, "{score}");
    }

    #[test]
    fn lifting_half_way_keeps_half_the_drawing() {
        let mut s = TraceState::new(&P, &mut rng());
        s.update(&P, &at(press(), L[0]));
        let frames = 120;
        for f in 1..=frames / 2 {
            s.update(&P, &at(hold(), point_along(L, f as f32 / frames as f32)));
        }
        match s.update(&P, &release()) {
            Outcome::Done { score } => assert!((score - 0.5).abs() < 0.03, "{score}"),
            Outcome::Running => panic!("letting go must end the step"),
        }
    }

    #[test]
    fn a_press_away_from_the_start_does_nothing() {
        let mut s = TraceState::new(&P, &mut rng());
        s.update(&P, &at(press(), Vec2::new(0.5, 0.5)));
        for _ in 0..60 {
            assert_eq!(s.update(&P, &at(hold(), L[1])), Outcome::Running);
        }
        assert_eq!(s.fraction(), 0.0);
    }

    #[test]
    fn the_trail_records_where_the_cursor_went_and_grades_it() {
        let mut s = TraceState::new(&P, &mut rng());
        s.update(&P, &at(press(), L[0]));
        // Along the bottom edge, then well off the path.
        for i in 1..=20 {
            s.update(&P, &at(hold(), Vec2::new(0.1 + 0.02 * i as f32, 0.1)));
        }
        s.update(&P, &at(hold(), Vec2::new(0.5, 0.5)));
        let trail = s.trail();
        assert!(trail.len() >= 15, "{}", trail.len());
        assert!(
            trail[..trail.len() - 1]
                .iter()
                .all(|t| t.grade == Grade::Good)
        );
        assert_eq!(trail.last().map(|t| t.grade), Some(Grade::Bad));
    }

    #[test]
    fn running_dry_ends_the_step() {
        let score = draw(100.0, |f| point_along(L, f));
        assert!(score < 0.1, "{score}");
    }
}
