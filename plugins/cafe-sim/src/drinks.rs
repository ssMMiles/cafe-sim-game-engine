//! Drink catalog shared between systems that take orders (customers), make
//! drinks (the brew station, staff) and pay for them (delivery, staff serve).
//!
//! A drink is a *recipe*: an ordered list of [`Step`]s, each a minigame whose
//! 0..=1 score is weighted into the drink's quality. Quality drives the payout
//! ([`payout`]). See `docs/CAFE_DRINK_MINIGAMES_PLAN.md`.
//!
//! Every number below is a placeholder, tuned by playing. Player-facing text
//! is ASCII only: bevy's default font is an ASCII subset.

use bevy::math::Vec2;

use crate::minigames::{
    FrothParams, Minigame, PourParams, PressureParams, ScoopParams, SteepParams, StirParams,
    TraceParams,
};

#[derive(Debug)]
pub struct Drink {
    pub name: &'static str,
    pub price: i64,
    /// How long a member of staff takes to make it. The player's time is
    /// however long the steps take.
    pub brew_secs: f32,
    /// How hard the drink is for staff, 0 = trivial. Pulls their quality down.
    pub difficulty: f32,
    pub steps: &'static [Step],
}

#[derive(Debug)]
pub struct Step {
    pub label: &'static str,
    /// One line telling the player what to do.
    pub hint: &'static str,
    pub game: Minigame,
    /// Share of the drink's quality (relative to the other steps).
    pub weight: f32,
}

/// Every step at or above this makes a drink "perfect".
pub const PERFECT_STEP: f32 = 0.9;

// ----------------------------------------------------------------------------
// Steps shared between recipes
// ----------------------------------------------------------------------------

const SCOOP_HINT: &str =
    "Hold to fill the scoop, let go to tip it in. Scoops are limited - get the cup into the green";
const POUR_HINT: &str = "Hold to pour - it keeps flowing a moment after you let go";

const fn scoop(label: &'static str, weight: f32, capacity: f32, band: (f32, f32)) -> Step {
    Step {
        label,
        hint: SCOOP_HINT,
        weight,
        game: Minigame::Scoop(ScoopParams {
            capacity,
            capacity_jitter: 0.3,
            fill_secs: 1.2,
            band,
            jitter: 0.12,
            spare_scoops: 1,
        }),
    }
}

const fn pour(label: &'static str, weight: f32, game: PourParams) -> Step {
    Step {
        label,
        hint: POUR_HINT,
        weight,
        game: Minigame::Pour(game),
    }
}

const POUR_WATER: PourParams = PourParams {
    accel: 1.4,
    max_speed: 0.55,
    settle_secs: 0.28,
    line: (0.74, 0.79),
    jitter: 0.1,
};

/// Milk pours slower than water, to a lower line (room for the foam).
const POUR_MILK: PourParams = PourParams {
    accel: 1.1,
    max_speed: 0.45,
    settle_secs: 0.3,
    line: (0.6, 0.66),
    jitter: 0.1,
};

const TAMP: Step = Step {
    label: "Tamp",
    hint: "Hold to tamp, release with the needle in the green",
    weight: 0.6,
    game: Minigame::Pressure(PressureParams {
        rise_per_sec: 0.3,
        accel_per_sec2: 0.7,
        band: (0.62, 0.70),
        jitter: 0.12,
        cap: 0.92,
    }),
};

const PULL_SHOT: Step = Step {
    label: "Pull the shot",
    hint: "Click to stop the shot inside the green",
    weight: 0.4,
    game: Minigame::Steep(SteepParams {
        duration_secs: 2.2,
        window: (0.6, 0.7),
        jitter: 0.15,
    }),
};

const STEAM_HINT: &str = "Hold to heat, let go to cool. The green band drifts - keep the needle in it until the foam is full";

const STEAM_MILK: FrothParams = FrothParams {
    heat_rate: 0.45,
    cool_rate: 0.22,
    // Jitter + drift must keep the band below the scald line.
    band: (0.55, 0.65),
    jitter: 0.05,
    scald: 0.9,
    foam_secs: 3.0,
    time_limit_secs: 12.0,
    drift: 0.08,
    drift_period_secs: 3.5,
};

const STEAM: Step = Step {
    label: "Steam the milk",
    hint: STEAM_HINT,
    weight: 0.6,
    game: Minigame::Froth(STEAM_MILK),
};

/// Cappuccino wants stiffer foam: longer in a narrower band.
const STEAM_STIFF: Step = Step {
    label: "Steam the milk (stiff foam)",
    hint: STEAM_HINT,
    weight: 0.6,
    game: Minigame::Froth(FrothParams {
        band: (0.6, 0.68),
        foam_secs: 4.0,
        ..STEAM_MILK
    }),
};

const TRACE_HINT: &str =
    "Press on the gold dot and, holding, follow the dots before the milk runs out";

/// Latte art: from the notch at the top, round the right lobe, down to the
/// tip and back up the left.
const HEART: &[Vec2] = &[
    Vec2::new(0.5, 0.684),
    Vec2::new(0.501, 0.696),
    Vec2::new(0.512, 0.729),
    Vec2::new(0.536, 0.773),
    Vec2::new(0.579, 0.816),
    Vec2::new(0.638, 0.845),
    Vec2::new(0.707, 0.852),
    Vec2::new(0.776, 0.833),
    Vec2::new(0.835, 0.791),
    Vec2::new(0.876, 0.73),
    Vec2::new(0.89, 0.66),
    Vec2::new(0.876, 0.587),
    Vec2::new(0.835, 0.516),
    Vec2::new(0.776, 0.449),
    Vec2::new(0.707, 0.387),
    Vec2::new(0.638, 0.328),
    Vec2::new(0.579, 0.273),
    Vec2::new(0.536, 0.223),
    Vec2::new(0.512, 0.183),
    Vec2::new(0.501, 0.157),
    Vec2::new(0.5, 0.148),
    Vec2::new(0.499, 0.157),
    Vec2::new(0.488, 0.183),
    Vec2::new(0.464, 0.223),
    Vec2::new(0.421, 0.273),
    Vec2::new(0.362, 0.328),
    Vec2::new(0.293, 0.387),
    Vec2::new(0.224, 0.449),
    Vec2::new(0.165, 0.516),
    Vec2::new(0.124, 0.587),
    Vec2::new(0.11, 0.66),
    Vec2::new(0.124, 0.73),
    Vec2::new(0.165, 0.791),
    Vec2::new(0.224, 0.833),
    Vec2::new(0.293, 0.852),
    Vec2::new(0.362, 0.845),
    Vec2::new(0.421, 0.816),
    Vec2::new(0.464, 0.773),
    Vec2::new(0.488, 0.729),
    Vec2::new(0.499, 0.696),
    Vec2::new(0.5, 0.684),
];

/// Cocoa dusting: two turns out from the centre.
const SPIRAL: &[Vec2] = &[
    Vec2::new(0.54, 0.5),
    Vec2::new(0.545, 0.512),
    Vec2::new(0.547, 0.527),
    Vec2::new(0.543, 0.543),
    Vec2::new(0.534, 0.559),
    Vec2::new(0.52, 0.573),
    Vec2::new(0.5, 0.583),
    Vec2::new(0.477, 0.587),
    Vec2::new(0.452, 0.584),
    Vec2::new(0.427, 0.573),
    Vec2::new(0.404, 0.555),
    Vec2::new(0.386, 0.531),
    Vec2::new(0.375, 0.5),
    Vec2::new(0.372, 0.466),
    Vec2::new(0.379, 0.43),
    Vec2::new(0.397, 0.397),
    Vec2::new(0.423, 0.367),
    Vec2::new(0.458, 0.345),
    Vec2::new(0.5, 0.333),
    Vec2::new(0.545, 0.331),
    Vec2::new(0.591, 0.343),
    Vec2::new(0.633, 0.367),
    Vec2::new(0.67, 0.402),
    Vec2::new(0.696, 0.447),
    Vec2::new(0.71, 0.5),
    Vec2::new(0.71, 0.556),
    Vec2::new(0.694, 0.612),
    Vec2::new(0.664, 0.664),
    Vec2::new(0.619, 0.706),
    Vec2::new(0.564, 0.737),
    Vec2::new(0.5, 0.752),
    Vec2::new(0.433, 0.751),
    Vec2::new(0.367, 0.731),
    Vec2::new(0.306, 0.694),
    Vec2::new(0.257, 0.64),
    Vec2::new(0.222, 0.575),
    Vec2::new(0.205, 0.5),
    Vec2::new(0.208, 0.422),
    Vec2::new(0.232, 0.345),
    Vec2::new(0.276, 0.276),
    Vec2::new(0.338, 0.22),
    Vec2::new(0.414, 0.181),
    Vec2::new(0.5, 0.162),
    Vec2::new(0.589, 0.167),
    Vec2::new(0.676, 0.195),
    Vec2::new(0.754, 0.246),
    Vec2::new(0.817, 0.317),
    Vec2::new(0.86, 0.403),
    Vec2::new(0.88, 0.5),
];

// ----------------------------------------------------------------------------
// The menu
// ----------------------------------------------------------------------------

pub const DRINKS: &[Drink] = &[
    Drink {
        name: "Coffee",
        price: 3,
        brew_secs: 4.0,
        difficulty: 0.2,
        steps: &[
            scoop("Scoop the grounds", 0.5, 0.2, (0.55, 0.6)),
            pour("Pour the water", 0.5, POUR_WATER),
        ],
    },
    Drink {
        name: "Latte",
        price: 5,
        brew_secs: 6.0,
        difficulty: 0.8,
        steps: &[
            TAMP,
            PULL_SHOT,
            STEAM,
            Step {
                label: "Pour a heart",
                hint: TRACE_HINT,
                weight: 0.8,
                game: Minigame::Trace(TraceParams {
                    path: HEART,
                    tolerance: 0.05,
                    max_secs: 5.0,
                }),
            },
        ],
    },
    Drink {
        name: "Tea",
        price: 2,
        brew_secs: 3.0,
        difficulty: 0.1,
        steps: &[
            scoop("Scoop the leaves", 0.3, 0.14, (0.45, 0.5)),
            pour("Pour the water", 0.3, POUR_WATER),
            Step {
                label: "Steep",
                hint: "Click to lift the leaves out inside the green",
                weight: 0.4,
                game: Minigame::Steep(SteepParams {
                    duration_secs: 4.0,
                    window: (0.5, 0.6),
                    jitter: 0.15,
                }),
            },
        ],
    },
    Drink {
        name: "Espresso",
        price: 3,
        brew_secs: 3.0,
        difficulty: 0.4,
        steps: &[TAMP, PULL_SHOT],
    },
    Drink {
        name: "Cappuccino",
        price: 5,
        brew_secs: 6.0,
        difficulty: 0.8,
        steps: &[
            TAMP,
            PULL_SHOT,
            STEAM_STIFF,
            Step {
                label: "Dust the cocoa",
                hint: TRACE_HINT,
                weight: 0.6,
                game: Minigame::Trace(TraceParams {
                    path: SPIRAL,
                    tolerance: 0.04,
                    max_secs: 5.5,
                }),
            },
        ],
    },
    Drink {
        name: "Hot Chocolate",
        price: 4,
        brew_secs: 5.0,
        difficulty: 0.5,
        steps: &[
            scoop("Scoop the cocoa", 0.2, 0.18, (0.5, 0.56)),
            pour("Pour the milk", 0.2, POUR_MILK),
            Step {
                weight: 0.3,
                ..STEAM
            },
            Step {
                label: "Whisk",
                hint: "Circle the cursor round the dish. Keep the speed needle in the green until it's mixed",
                weight: 0.3,
                game: Minigame::Stir(StirParams {
                    mix_per_rad: 1.0 / (6.0 * core::f32::consts::TAU), // six turns
                    safe_speed: 1.5 * core::f32::consts::TAU,          // 1.5 turns a second
                    good_from: 0.6,
                    slosh_gain: 0.01,
                    slosh_decay: 0.4,
                    max_secs: 15.0,
                }),
            },
        ],
    },
];

pub fn drink(name: &str) -> Option<&'static Drink> {
    DRINKS.iter().find(|d| d.name == name)
}

impl Drink {
    /// Weighted mean of the step scores (`scores[i]` belongs to `steps[i]`),
    /// and whether every step was perfect.
    pub fn quality(&self, scores: &[f32]) -> (f32, bool) {
        let total: f32 = self.steps.iter().map(|s| s.weight).sum();
        if total <= 0.0 {
            return (0.0, false);
        }
        let sum: f32 = self
            .steps
            .iter()
            .zip(scores)
            .map(|(s, &score)| s.weight * score)
            .sum();
        let perfect = scores.len() == self.steps.len() && scores.iter().all(|&s| s >= PERFECT_STEP);
        ((sum / total).clamp(0.0, 1.0), perfect)
    }
}

// ----------------------------------------------------------------------------
// Payout
// ----------------------------------------------------------------------------

/// Below this quality the customer is unhappy and pays half.
pub const REFUSE_BELOW: f32 = 0.3;
/// Quality at which a drink starts earning a tip; it is a full tip at 1.0.
pub const TIP_FROM: f32 = 0.5;
/// A full tip is `price × TIP_FRACTION`, earned by quality, friendliness or both.
pub const TIP_FRACTION: f32 = 0.5;
/// Flat bonus for a perfect drink.
pub const PERFECT_BONUS: i64 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Payout {
    pub amount: i64,
    /// The customer leaves unhappy (bad drink, half pay).
    pub unhappy: bool,
}

/// What a correctly-ordered drink earns. `friendliness` is the server's
/// (0 for the player, who earns their tip by playing well).
pub fn payout(price: i64, quality: f32, perfect: bool, friendliness: f32) -> Payout {
    if quality < REFUSE_BELOW {
        return Payout {
            amount: price / 2,
            unhappy: true,
        };
    }
    let quality_tip = ((quality - TIP_FROM) / (1.0 - TIP_FROM)).max(0.0);
    let tip = (price as f32 * TIP_FRACTION * (quality_tip + friendliness)).round() as i64;
    let bonus = if perfect { PERFECT_BONUS } else { 0 };
    Payout {
        amount: price + tip + bonus,
        unhappy: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steps_are_well_formed() {
        for d in DRINKS {
            assert!(d.steps.len() >= 2, "{} needs a recipe", d.name);
            for s in d.steps {
                assert!(s.weight > 0.0, "{}: {}", d.name, s.label);
                assert!(s.game.is_well_formed(), "{}: {}", d.name, s.label);
                assert!(
                    s.label.is_ascii() && s.hint.is_ascii(),
                    "{}: {}",
                    d.name,
                    s.label
                );
            }
        }
    }

    #[test]
    fn quality_is_the_weighted_mean() {
        let espresso = drink("Espresso").unwrap();
        let (q, perfect) = espresso.quality(&[1.0, 0.0]);
        assert!((q - 0.6).abs() < 1e-6);
        assert!(!perfect);
        assert!(espresso.quality(&[0.95, 0.9]).1);
    }

    #[test]
    fn payout_table() {
        // An average drink pays exactly its price.
        assert_eq!(payout(4, TIP_FROM, false, 0.0).amount, 4);
        assert_eq!(payout(4, 1.0, false, 0.0).amount, 6);
        assert_eq!(payout(4, 1.0, true, 0.0).amount, 6 + PERFECT_BONUS);
        assert_eq!(payout(4, 0.6, false, 1.0).amount, 4 + 2); // 0.2+1.0 → 2.4 → 2
        let bad = payout(5, 0.1, false, 1.0);
        assert!(bad.unhappy);
        assert_eq!(bad.amount, 2);
    }
}
