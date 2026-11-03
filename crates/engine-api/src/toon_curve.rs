//! The toon transfer function: lightmap radiance in, displayed fraction of
//! base colour out.
//!
//! This is the *whole* runtime lighting model. Everything upstream — sun,
//! sky, bounce, every placed light — is folded into a radiance value at bake
//! time; all the shaders do is run that value through the curve below and
//! multiply by base colour. So this file decides what the game looks like far
//! more than any single bake setting does.
//!
//! The same function lives in `assets/shaders/toon_lighting.wesl` and
//! `assets/shaders/terrain_toon.wesl` (asset-path shaders have no shared
//! import module). The copy here is the one the tests measure and the one
//! `docs/LIGHTING.md`'s tables are generated from;
//! `toon_curve_matches_the_shaders` holds all three in step.
//!
//! ## Three separate decisions
//!
//! The curve used to be one constant, `TOON_FLOOR = 0.18`, doing three
//! unrelated jobs at once, which is why it could not be lowered:
//!
//! 1. **How dark a full shadow gets** ([`ToonCurve::floor`]) — the only one
//!    of the three that is a look decision.
//! 2. **Where the band boundaries fall** ([`ToonCurve::white`] and
//!    [`ToonCurve::gamma`]) — which radiance values the five levels are
//!    spread across.
//! 3. **How wide the bottom band's fade to neutral grey is** — which is a
//!    distance in *band* units, not a radiance, and only ever shared the
//!    number by coincidence.
//!
//! Lowering the floor alone therefore did two unwanted things: it made
//! `smoothstep(0.0, TOON_FLOOR, band)` degenerate at zero (undefined in WGSL
//! when the two edges are equal), and — because the bands were linear in
//! radiance, so the first boundary sat at 0.2 while a shaded surface
//! receives about 0.1 — it crushed every shaded surface in the game to the
//! floor value at once. See `docs/LIGHTING.md` §3.

use bevy::prelude::*;

/// Perceptual luminance of a linear RGB colour. Rec. 709 weights.
pub fn luminance(color: Vec3) -> f32 {
    color.dot(Vec3::new(0.2126, 0.7152, 0.0722))
}

/// Shape of the runtime toon ramp. Lives in the material uniform, so it is
/// tunable per world from the inspector without a recompile.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ToonCurve {
    /// Number of discrete shading levels. The displayed value can take
    /// `bands + 1` values: `floor`, then `bands` steps up to 1.0.
    pub bands: f32,
    /// Darkest displayed fraction of base colour. May be 0 — a full shadow
    /// then renders black rather than as a dark tint of the surface.
    pub floor: f32,
    /// Lightmap luminance that reads as fully lit. Radiance above it is
    /// clipped, so this is the scene's exposure: lower it to bring a bright
    /// world down into the bands, raise it to stop a sunlit surface from
    /// pinning at the top.
    pub white: f32,
    /// How brightness is distributed across the bands.
    ///
    /// `1.0` quantises radiance directly, which spends four of five bands on
    /// the top octave: a sunlit surface and a shaded one differ by about 10×,
    /// so everything in shade lands in band 0 and the ramp has no dark
    /// detail at all. Above 1 the bands are spaced by *perceived* brightness
    /// instead — the boundaries move down to `white · (k/bands)^gamma` — so
    /// ambient-lit surfaces occupy real bands and the floor becomes
    /// reachable only by genuine darkness.
    pub gamma: f32,
}

impl Default for ToonCurve {
    fn default() -> Self {
        Self {
            bands: 5.0,
            floor: DEFAULT_FLOOR,
            white: 1.0,
            gamma: 2.2,
        }
    }
}

/// Darkest displayed fraction of base colour, by default.
///
/// The old `0.18` was chosen when the bands were linear in radiance and band
/// 0 covered everything in shade — the floor *was* the shadow colour, so it
/// had to carry enough light to read as a surface. With perceptual band
/// spacing a shaded surface sits in band 1 or 2 and the floor is reached
/// only below `white · (1/bands)^gamma`, so it can be a real dark without
/// flattening anything.
pub const DEFAULT_FLOOR: f32 = 0.04;

impl ToonCurve {
    /// Pack for the material uniform, matching `ToonCurve` in the WESL.
    pub fn to_vec4(self) -> Vec4 {
        Vec4::new(self.bands, self.floor, self.white, self.gamma)
    }

    /// Guard the values a user can dial in the inspector into a range the
    /// curve is defined on. `bands >= 1` keeps the neutral fade non-degenerate,
    /// `white > 0` keeps the exposure divide finite, and `gamma > 0` keeps
    /// the encode monotone.
    pub fn sanitized(self) -> Self {
        Self {
            bands: self.bands.round().clamp(1.0, 16.0),
            floor: self.floor.clamp(0.0, 1.0),
            white: self.white.max(1e-3),
            gamma: self.gamma.clamp(0.1, 8.0),
        }
    }

    /// Radiance luminance → the perceptual coordinate the bands are evenly
    /// spaced on, in `[0, 1]`. This is the only place [`Self::white`] is
    /// used: it is an *input* exposure, and does not scale the output.
    pub fn encode(self, lum: f32) -> f32 {
        (lum / self.white).clamp(0.0, 1.0).powf(1.0 / self.gamma)
    }

    /// The quantiser, in a flat region of the image.
    ///
    /// The shaders ramp each boundary over about one screen pixel with
    /// `fwidth`, which this does not model: there is no screen here. Away
    /// from a boundary the two agree exactly, and the boundary itself is one
    /// pixel wide, so every table in the docs is computed from this.
    pub fn band(self, lum: f32) -> f32 {
        (self.encode(lum) * self.bands).floor().min(self.bands) / self.bands
    }

    /// Displayed fraction of base colour for a banded value in `[0, 1]`:
    /// [`Self::floor`] at 0, exactly 1.0 at the top.
    ///
    /// Spaced evenly in the *same* perceptual space the input was quantised
    /// in, so the steps look evenly sized rather than crowding at one end.
    /// `white` deliberately does not appear — it sets which radiance values
    /// reach the top band, not how bright the top band is.
    pub fn level(self, band: f32) -> f32 {
        let floor_x = self.floor.clamp(0.0, 1.0).powf(1.0 / self.gamma);
        (floor_x + (1.0 - floor_x) * band.clamp(0.0, 1.0)).powf(self.gamma)
    }

    /// Displayed colour for a sampled radiance: the full runtime model.
    pub fn shade(self, radiance: Vec3) -> Vec3 {
        let lum = luminance(radiance);
        let band = self.band(lum);
        let level = self.level(band);
        // Rescale rather than band per channel, so coloured light keeps its
        // hue instead of shifting at every boundary. Both ends of the mix
        // below have luminance exactly `level`, so the curve stays monotone:
        // the fade moves chroma only, never brightness.
        let tinted = radiance * (level / lum.max(1e-4));
        let t = smoothstep(0.0, self.neutral_fade(), band);
        Vec3::splat(level).lerp(tinted, t)
    }

    /// Width, in band units, of the fade from neutral grey to hue-preserving
    /// at the bottom of the ramp, so a trace of strongly tinted light cannot
    /// paint a dark corner its own colour.
    ///
    /// One band wide, i.e. it completes exactly at the first boundary. This
    /// is a *band-space* distance and has nothing to do with [`Self::floor`];
    /// conflating the two is what made a floor of zero degenerate.
    pub fn neutral_fade(self) -> f32 {
        1.0 / self.bands.max(1.0)
    }

    /// Lightmap luminance at which band `k` begins (`k` in `1..=bands`).
    pub fn threshold(self, k: u32) -> f32 {
        (k as f32 / self.bands).powf(self.gamma) * self.white
    }

    /// The displayed levels, darkest first: `bands + 1` of them.
    pub fn levels(self) -> Vec<f32> {
        (0..=self.bands as u32)
            .map(|k| self.level(k as f32 / self.bands))
            .collect()
    }
}

fn smoothstep(low: f32, high: f32, x: f32) -> f32 {
    let t = ((x - low) / (high - low)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The defect that made the floor untunable: with the fade width tied to
    /// `floor`, a floor of zero made `smoothstep(0, 0, band)` a 0/0 — which
    /// WGSL leaves undefined and naga lowers to a divide, so the shader
    /// returned NaN for *every* fragment, not just dark ones.
    #[test]
    fn a_zero_floor_is_well_defined() {
        let curve = ToonCurve {
            floor: 0.0,
            ..default()
        };
        assert!(curve.neutral_fade() > 0.0);
        for i in 0..=100 {
            let out = curve.shade(Vec3::splat(i as f32 / 50.0));
            assert!(out.is_finite(), "non-finite output at lum {i}");
        }
        assert_eq!(curve.level(0.0), 0.0);
    }

    /// Displayed brightness must never fall as radiance rises. The old hue
    /// fade mixed a constant floor with a value climbing from zero and dipped
    /// 26% below the floor in between, drawing a dark outline one pixel wide
    /// along the edge of every shadow in the game.
    #[test]
    fn the_curve_is_monotone_in_radiance() {
        for curve in [
            ToonCurve::default(),
            ToonCurve {
                floor: 0.0,
                ..default()
            },
            ToonCurve {
                gamma: 1.0,
                ..default()
            },
        ] {
            let mut prev = -1.0;
            for i in 0..2000 {
                let lum = i as f32 / 1000.0;
                let out = luminance(curve.shade(Vec3::splat(lum)));
                assert!(
                    out >= prev - 1e-5,
                    "{curve:?} dipped at lum {lum}: {out} < {prev}"
                );
                prev = out;
            }
        }
    }

    /// `shade` preserves hue above the bottom band, and only rescales
    /// brightness — a coloured light must not drift towards grey or towards
    /// its own hue as it crosses a boundary.
    #[test]
    fn hue_is_preserved_above_the_bottom_band() {
        let curve = ToonCurve::default();
        let radiance = Vec3::new(0.9, 0.5, 0.2);
        let out = curve.shade(radiance);
        let ratio = out / radiance;
        assert!(
            (ratio.x - ratio.y).abs() < 1e-5 && (ratio.y - ratio.z).abs() < 1e-5,
            "hue shifted: {ratio:?}"
        );
    }

    /// Band `k`'s threshold is exactly where the band index changes, and the
    /// levels run from floor to 1 — whatever the exposure. `white` moves the
    /// *boundaries*; it must never make the top band brighter than the base
    /// colour, which would be an overbright surface rather than an exposure.
    #[test]
    fn thresholds_and_levels_line_up() {
        for white in [0.5, 1.0, 1.25, 4.0] {
            let curve = ToonCurve { white, ..default() };
            for k in 1..=curve.bands as u32 {
                let t = curve.threshold(k);
                assert!(
                    curve.band(t * 0.999) < curve.band(t * 1.001),
                    "no band change at threshold {k} of {curve:?}"
                );
            }
            let levels = curve.levels();
            assert_eq!(levels.len(), 6);
            assert!((levels[0] - curve.floor).abs() < 1e-5, "{levels:?}");
            assert!((levels[5] - 1.0).abs() < 1e-5, "{levels:?}");
            assert!((curve.threshold(5) - white).abs() < 1e-5);
        }
    }

    /// The whole point of a non-unit gamma: with the bands linear in radiance
    /// the scene's two commonest states (shaded, ~0.1, and sunlit, ~1.0) both
    /// pin at an end of the ramp and the three middle levels are unreachable
    /// outside a penumbra.
    #[test]
    fn shaded_surfaces_reach_a_band_above_the_floor() {
        // ambient 0.25 x colour luminance 0.91, under typical open AO.
        let shaded = 0.227 * 0.5;
        let linear = ToonCurve {
            gamma: 1.0,
            ..default()
        };
        assert_eq!(linear.band(shaded), 0.0, "linear bands crush shade");

        let curve = ToonCurve::default();
        assert!(
            curve.band(shaded) >= 1.0 / curve.bands,
            "shade should clear the first boundary, got band {}",
            curve.band(shaded)
        );
    }
}
