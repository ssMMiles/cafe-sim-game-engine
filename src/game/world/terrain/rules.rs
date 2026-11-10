//! Paint rules: which layer the ground *should* be, from height, slope and
//! noise (`docs/TERRAIN_PAINTING_PLAN.md` §4.5, Phase 3).
//!
//! A world has an ordered stack of [`PaintRule`]s. Evaluated at a tile, each
//! rule yields a strength 0..1 (the product of its band weights); the rules
//! are applied in order like paint strokes, so a later rule paints over an
//! earlier one — the result is one [`PaintSample`] carrying
//! [`PAINT_FLAG_AUTO`]. Tiles with that flag are re-evaluated when the
//! heights under them change or the rules are edited; a hand-painted tile
//! clears it, so hand work sits on top of the rules.
//!
//! The same [`Band`] / [`NoiseMask`] types are the brush masks
//! (`paint_tool::BrushMasks`), so "rock above 60°" means the same thing
//! whether a rule says it or a brush is restricted to it.

use bevy::prelude::*;
use rkyv::{Archive, Deserialize, Serialize};

use super::{
    CHUNK_SIZE, HeightmapData, TILE_SIZE,
    paint::{
        PAINT_FLAG_AUTO, PaintSample, PaintmapData, paint_base, paint_blend, paint_flags,
        paint_index, paint_overlay, paint_sample,
    },
};

/// A soft range: weight 1 inside `[min, max]`, fading to 0 over `soft`
/// outside each end.
#[derive(Archive, Deserialize, Serialize, Clone, Copy, Debug, PartialEq)]
pub struct Band {
    pub min: f32,
    pub max: f32,
    pub soft: f32,
}

impl Band {
    pub fn new(min: f32, max: f32, soft: f32) -> Self {
        Self { min, max, soft }
    }

    pub fn weight(&self, v: f32) -> f32 {
        let soft = self.soft.max(1e-4);
        let lo = smoothstep((v - (self.min - soft)) / soft);
        let hi = smoothstep(((self.max + soft) - v) / soft);
        lo * hi
    }
}

/// Smooth value noise over world xz: weight is how far the noise exceeds
/// `threshold`, resolved over `soft`.
#[derive(Archive, Deserialize, Serialize, Clone, Copy, Debug, PartialEq)]
pub struct NoiseMask {
    /// Feature size in metres.
    pub scale: f32,
    /// 0 = everywhere, 1 = nowhere.
    pub threshold: f32,
    pub soft: f32,
}

impl NoiseMask {
    pub fn weight(&self, xz: Vec2) -> f32 {
        let n = value_noise(xz / self.scale.max(0.05));
        smoothstep((n - self.threshold) / self.soft.max(1e-4) + 0.5)
    }
}

impl Default for NoiseMask {
    fn default() -> Self {
        Self {
            scale: 8.0,
            threshold: 0.5,
            soft: 0.2,
        }
    }
}

/// One rule: paint `layer` where every enabled condition holds.
#[derive(Archive, Deserialize, Serialize, Clone, Debug, PartialEq)]
pub struct PaintRule {
    pub name: String,
    pub layer: u8,
    pub enabled: bool,
    /// World height, metres.
    pub height: Option<Band>,
    /// Slope from horizontal, degrees.
    pub slope: Option<Band>,
    pub noise: Option<NoiseMask>,
}

impl PaintRule {
    pub fn new(name: impl Into<String>, layer: u8) -> Self {
        Self {
            name: name.into(),
            layer,
            enabled: true,
            height: None,
            slope: None,
            noise: None,
        }
    }

    /// 0..1 strength at a point. A rule with no conditions applies
    /// everywhere (a base coat).
    pub fn strength(&self, height: f32, slope_deg: f32, xz: Vec2) -> f32 {
        if !self.enabled {
            return 0.0;
        }
        let mut w = 1.0;
        if let Some(b) = &self.height {
            w *= b.weight(height);
        }
        if let Some(b) = &self.slope {
            w *= b.weight(slope_deg);
        }
        if let Some(n) = &self.noise {
            w *= n.weight(xz);
        }
        w
    }
}

/// Apply the stack at a point: start from layer 0 and let each rule paint
/// over the result by its strength. Flagged auto.
pub fn rules_sample(rules: &[PaintRule], height: f32, slope_deg: f32, xz: Vec2) -> PaintSample {
    let mut sample = paint_sample(0, 0, 0, PAINT_FLAG_AUTO);
    for rule in rules {
        let w = rule.strength(height, slope_deg, xz);
        if w <= 0.002 {
            continue;
        }
        sample = cover(sample, rule.layer, w);
    }
    sample
}

/// `layer` painted over `sample` with coverage `w` (0..1): the current
/// dominant layer becomes the base, `layer` the overlay. Full coverage is
/// just `layer`.
fn cover(sample: PaintSample, layer: u8, w: f32) -> PaintSample {
    let dominant = if paint_blend(sample) >= 128 {
        paint_overlay(sample)
    } else {
        paint_base(sample)
    };
    if w >= 0.998 || dominant == layer {
        return paint_sample(layer, layer, 0, PAINT_FLAG_AUTO);
    }
    paint_sample(dominant, layer, (w * 255.0).round() as u8, PAINT_FLAG_AUTO)
}

/// Height and slope (degrees) of a tile, from the loaded heightmap.
pub fn tile_height_slope(heightmap: &HeightmapData, tx: i32, ty: i32) -> (f32, f32, Vec2) {
    let xz = Vec2::new(tx as f32 * TILE_SIZE.0, ty as f32 * TILE_SIZE.1);
    let h = heightmap.get_tile(tx, ty);
    let slope = heightmap
        .sample_normal(xz.x, xz.y)
        .map(|(_, s)| s.to_degrees())
        .unwrap_or(0.0);
    (h, slope, xz)
}

/// Re-run the rules on every auto-flagged tile of a loaded chunk (after the
/// heights under it changed, or the rules did). Returns whether anything
/// changed; the caller marks the chunk dirty. Not recorded in history: the
/// auto tiles are derived state, and re-running after an undo converges.
pub fn reevaluate_auto(
    rules: &[PaintRule],
    heightmap: &HeightmapData,
    paintmap: &mut PaintmapData,
    images: &mut Assets<Image>,
    chunk: (i32, i32),
) -> bool {
    let Some(current) = paintmap.chunk_samples(chunk) else {
        return false;
    };
    if !current
        .iter()
        .any(|&s| paint_flags(s) & PAINT_FLAG_AUTO != 0)
    {
        return false;
    }
    let cs = (CHUNK_SIZE - 1) as i32;
    let (start_x, start_y) = (chunk.0 * cs, chunk.1 * cs);
    let mut next = current.to_vec();
    let mut changed = false;
    for lx in 0..CHUNK_SIZE {
        for ly in 0..CHUNK_SIZE {
            let i = paint_index(lx, ly);
            if paint_flags(next[i]) & PAINT_FLAG_AUTO == 0 {
                continue;
            }
            let (h, slope, xz) =
                tile_height_slope(heightmap, start_x + lx as i32, start_y + ly as i32);
            let s = rules_sample(rules, h, slope, xz);
            if s != next[i] {
                next[i] = s;
                changed = true;
            }
        }
    }
    if changed {
        paintmap.replace_chunk(images, chunk, next);
    }
    changed
}

fn smoothstep(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

fn hash01(p: IVec2) -> f32 {
    let mut h = (p.x as u32).wrapping_mul(0x9E37_79B9) ^ (p.y as u32).wrapping_mul(0x85EB_CA6B);
    h ^= h >> 15;
    h = h.wrapping_mul(0x2C1B_3C6D);
    h ^= h >> 12;
    (h & 0xFFFF) as f32 / 65535.0
}

/// Bilinear value noise on the integer lattice, 0..1.
pub fn value_noise(p: Vec2) -> f32 {
    let i = p.floor();
    let f = p - i;
    let w = f * f * (Vec2::splat(3.0) - 2.0 * f);
    let i = i.as_ivec2();
    let a = hash01(i);
    let b = hash01(i + IVec2::X);
    let c = hash01(i + IVec2::Y);
    let d = hash01(i + IVec2::ONE);
    let top = a + (b - a) * w.x;
    let bottom = c + (d - c) * w.x;
    top + (bottom - top) * w.y
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn band_is_one_inside_and_fades_over_soft() {
        let b = Band::new(10.0, 20.0, 2.0);
        assert_eq!(b.weight(15.0), 1.0);
        assert_eq!(b.weight(10.0), 1.0);
        assert_eq!(b.weight(7.0), 0.0);
        let mid = b.weight(9.0);
        assert!(mid > 0.0 && mid < 1.0);
        assert_eq!(b.weight(23.0), 0.0);
    }

    #[test]
    fn later_rules_paint_over_earlier_ones() {
        let rules = vec![
            PaintRule::new("grass", 1),
            PaintRule {
                height: Some(Band::new(50.0, 1000.0, 5.0)),
                ..PaintRule::new("snow", 2)
            },
            PaintRule {
                slope: Some(Band::new(45.0, 90.0, 5.0)),
                ..PaintRule::new("rock", 3)
            },
        ];
        let low_flat = rules_sample(&rules, 10.0, 0.0, Vec2::ZERO);
        assert_eq!(paint_base(low_flat), 1);
        assert_eq!(paint_blend(low_flat), 0);
        let high_flat = rules_sample(&rules, 100.0, 0.0, Vec2::ZERO);
        assert_eq!(paint_base(high_flat), 2);
        // Partway up the snow fade: grass base, snow overlay.
        let fading = rules_sample(&rules, 47.0, 0.0, Vec2::ZERO);
        assert_eq!((paint_base(fading), paint_overlay(fading)), (1, 2));
        assert!(paint_blend(fading) > 0 && paint_blend(fading) < 255);
        // Steep wins regardless of height.
        let cliff = rules_sample(&rules, 100.0, 70.0, Vec2::ZERO);
        assert_eq!(paint_base(cliff), 3);
        assert_eq!(super::super::paint::paint_flags(cliff), PAINT_FLAG_AUTO);
    }

    #[test]
    fn disabled_and_empty_rules() {
        let mut r = PaintRule::new("x", 4);
        r.enabled = false;
        assert_eq!(paint_base(rules_sample(&[r], 0.0, 0.0, Vec2::ZERO)), 0);
        assert_eq!(paint_base(rules_sample(&[], 0.0, 0.0, Vec2::ZERO)), 0);
    }

    #[test]
    fn value_noise_is_in_range_and_continuous() {
        let a = value_noise(Vec2::new(3.4, 7.9));
        let b = value_noise(Vec2::new(3.401, 7.9));
        assert!((0.0..=1.0).contains(&a));
        assert!((a - b).abs() < 0.01);
    }
}
