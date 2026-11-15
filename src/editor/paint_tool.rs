//! Paint tool: paint the world's palette layers onto the terrain
//! (`docs/TERRAIN_PAINTING_PLAN.md` §4.4, Phase 1).
//!
//! The brush machinery — the hit under the cursor, the disc, `[`/`]`,
//! radius and falloff — is shared with the Terrain tool (`terrain_edit`);
//! this module owns what happens to the *paint* under it.
//!
//! Sub-tools. **Paint** raises the blend towards the selected layer at
//! `flow` per second (Ctrl erases: lowers it back towards the base); a tile
//! whose layer pair doesn't include the selected layer snaps its base to
//! whichever of its two layers was dominant and starts blending the new one
//! in, so a transition zone always shares one pair end to end (§4.7). When
//! the blend saturates the overlay *becomes* the base. **Fill** sets every
//! tile in the disc to exactly the selected layer, no falloff. **Smooth**
//! pulls each tile's blend towards its neighbours' (same pair only).
//! **Jitter** pushes blends apart by a per-tile hash for a ragged edge.
//! **Replace** swaps `replace_from` for the selected layer under the disc.
//! Alt+LMB picks the dominant layer under the cursor (the *from* layer in
//! Replace). Tiles dominated by a **locked** layer are never written.
//!
//! **Auto** paints the rules' answer (`rules::rules_sample`) into the disc,
//! flagged auto so sculpting underneath re-evaluates it; Ctrl+LMB clears
//! the flag and hands the tiles back to hand painting. Every hand sub-tool
//! clears the flag on the tiles it writes. [`BrushMasks`] restrict any
//! sub-tool by height, slope, noise or the layer already there.
//!
//! Whole-world operations the panel can't do itself (it has no paint-map
//! access) arrive as a [`PaintRequest`] and run in `apply_paint_requests`.
//!
//! A stroke is one undo transaction: `Change::Paint` holds each touched
//! chunk's grid before and after, run-length coded, which is small for a
//! stroke (long runs) and for a fill (one run) alike.

use std::collections::{HashMap, HashSet};

use bevy::prelude::*;
use engine_api::UnlitMaterial;

use super::history::{Change, History, PaintChunkChange};
use super::picking::ViewportInput;
use super::terrain_edit::{BrushPreviewMarker, BrushSettings, TerrainBrushHit, brush_weight};
use crate::game::world::terrain::{
    HeightmapData, TILE_SIZE, chunks_touching_tile,
    paint::{
        PAINT_FLAG_AUTO, PaintData, PaintSample, PaintmapData, paint_base, paint_blend,
        paint_flags, paint_overlay, paint_sample, remap_sample,
    },
    rules::{Band, NoiseMask, reevaluate_auto, rules_sample, tile_height_slope},
    storage::{TerrainChunkRegistry, TerrainWorld},
};

/// Blend change per second at full brush weight, in 0..1 of the blend range.
pub const PAINT_FLOW_RANGE: std::ops::RangeInclusive<f32> = 0.2..=8.0;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum PaintSubTool {
    #[default]
    Paint,
    Fill,
    Smooth,
    Jitter,
    Replace,
    Auto,
}

impl PaintSubTool {
    pub const ALL: [PaintSubTool; 6] = [
        PaintSubTool::Paint,
        PaintSubTool::Fill,
        PaintSubTool::Smooth,
        PaintSubTool::Jitter,
        PaintSubTool::Replace,
        PaintSubTool::Auto,
    ];

    pub fn label(self) -> &'static str {
        match self {
            PaintSubTool::Paint => "Paint",
            PaintSubTool::Fill => "Fill",
            PaintSubTool::Smooth => "Smooth",
            PaintSubTool::Jitter => "Jitter",
            PaintSubTool::Replace => "Replace",
            PaintSubTool::Auto => "Auto",
        }
    }

    /// Whether the flow slider means anything here.
    pub fn uses_flow(self) -> bool {
        matches!(
            self,
            PaintSubTool::Paint | PaintSubTool::Smooth | PaintSubTool::Jitter
        )
    }
}

/// A whole-world edit queued by the panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaintRequest {
    /// Every loaded chunk becomes the selected layer.
    FillAll,
    /// `from` → `to` in every loaded chunk.
    ReplaceAll { from: u8, to: u8 },
    /// Retire `layer`: loaded chunks are rewritten to `fallback`, the slot
    /// redirects there for chunks not loaded yet.
    RemoveLayer { layer: u8, fallback: u8 },
    /// Paint the rules' answer (flagged auto) into every tile of the chunk
    /// under the cursor, or of every loaded chunk.
    ApplyRules { all_loaded: bool },
}

/// Restrictions on where a brush may write. Each is a weight multiplier.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct BrushMasks {
    /// World height, metres.
    pub height: Option<Band>,
    /// Slope from horizontal, degrees.
    pub slope: Option<Band>,
    pub noise: Option<NoiseMask>,
    /// Only tiles whose dominant layer is this one.
    pub only_over: Option<u8>,
}

impl BrushMasks {
    pub fn is_any(&self) -> bool {
        self.height.is_some()
            || self.slope.is_some()
            || self.noise.is_some()
            || self.only_over.is_some()
    }

    fn weight(&self, heightmap: &HeightmapData, tx: i32, ty: i32, current: PaintSample) -> f32 {
        if let Some(only) = self.only_over
            && dominant_layer(current) != only
        {
            return 0.0;
        }
        if !(self.height.is_some() || self.slope.is_some() || self.noise.is_some()) {
            return 1.0;
        }
        let (h, slope, xz) = tile_height_slope(heightmap, tx, ty);
        let mut w = 1.0;
        if let Some(b) = &self.height {
            w *= b.weight(h);
        }
        if let Some(b) = &self.slope {
            w *= b.weight(slope);
        }
        if let Some(n) = &self.noise {
            w *= n.weight(xz);
        }
        w
    }
}

#[derive(Resource)]
pub struct PaintToolState {
    /// Palette index of the layer the brush paints with.
    pub selected: usize,
    pub sub_tool: PaintSubTool,
    /// Blend change per second at full weight (Paint / Smooth / Jitter).
    pub flow: f32,
    /// Replace sub-tool: the layer being replaced (by `selected`).
    pub replace_from: usize,
    /// Set by the panel; run by `apply_paint_requests`.
    pub request: Option<PaintRequest>,
    pub masks: BrushMasks,
    /// The rules as last re-evaluated, and when they last changed, so a
    /// slider drag re-runs the loaded chunks once it settles.
    rules_seen: Option<Vec<crate::game::world::terrain::rules::PaintRule>>,
    rules_changed_at: Option<f64>,
    /// Grids as they were when the current stroke first touched each chunk.
    stroke_before: HashMap<(i32, i32), PaintData>,
    was_painting: bool,
    /// Layer the disc is currently dressed as, to avoid rewriting its
    /// material every frame.
    disc_layer: Option<(usize, String, [f32; 3])>,
}

impl Default for PaintToolState {
    fn default() -> Self {
        Self {
            selected: 0,
            sub_tool: PaintSubTool::default(),
            flow: 2.0,
            replace_from: 0,
            request: None,
            masks: BrushMasks::default(),
            rules_seen: None,
            rules_changed_at: None,
            stroke_before: HashMap::new(),
            was_painting: false,
            disc_layer: None,
        }
    }
}

fn alt(keys: &ButtonInput<KeyCode>) -> bool {
    keys.pressed(KeyCode::AltLeft) || keys.pressed(KeyCode::AltRight)
}

fn ctrl(keys: &ButtonInput<KeyCode>) -> bool {
    keys.pressed(KeyCode::ControlLeft) || keys.pressed(KeyCode::ControlRight)
}

/// Which layer a sample reads as: the overlay once it is at least half in.
pub fn dominant_layer(sample: PaintSample) -> u8 {
    if paint_blend(sample) >= 128 {
        paint_overlay(sample)
    } else {
        paint_base(sample)
    }
}

/// One brush step on one sample. `delta` is the blend change in 0..1 (the
/// frame's flow × weight); negative erases.
pub fn paint_step(sample: PaintSample, layer: u8, delta: f32) -> PaintSample {
    let base = paint_base(sample);
    let overlay = paint_overlay(sample);
    let blend = paint_blend(sample) as f32 / 255.0;
    let flags = 0; // a hand-painted tile is never auto (Phase 3)
    let step = (delta * 255.0).round() as i32;
    if step == 0 {
        return sample;
    }
    let clamp = |b: i32| b.clamp(0, 255) as u8;
    let b = (blend * 255.0).round() as i32;

    if delta < 0.0 {
        // Erase: back towards the base, whatever the selected layer.
        let nb = clamp(b + step);
        return if nb == 0 {
            paint_sample(base, base, 0, flags)
        } else {
            paint_sample(base, overlay, nb, flags)
        };
    }
    if layer == base && layer == overlay {
        return paint_sample(base, overlay, 0, flags);
    }
    if layer == base {
        // Painting the base over its own overlay: lower the blend.
        let nb = clamp(b - step);
        return if nb == 0 {
            paint_sample(base, base, 0, flags)
        } else {
            paint_sample(base, overlay, nb, flags)
        };
    }
    if layer == overlay {
        let nb = clamp(b + step);
        return if nb == 255 {
            paint_sample(layer, layer, 0, flags)
        } else {
            paint_sample(base, overlay, nb, flags)
        };
    }
    // A third layer: the dominant one becomes the base and the new one
    // starts blending in from zero.
    let new_base = dominant_layer(sample);
    paint_sample(new_base, layer, clamp(step), flags)
}

fn with_flags(sample: PaintSample, flags: u8) -> PaintSample {
    paint_sample(
        paint_base(sample),
        paint_overlay(sample),
        paint_blend(sample),
        flags,
    )
}

fn same_pair(a: PaintSample, b: PaintSample) -> bool {
    paint_base(a) == paint_base(b) && paint_overlay(a) == paint_overlay(b)
}

/// Move a sample's blend towards `target` (0..1) by at most `budget`,
/// collapsing to one layer at either end like `paint_step` does.
pub fn step_blend(sample: PaintSample, target: f32, budget: f32) -> PaintSample {
    let base = paint_base(sample);
    let overlay = paint_overlay(sample);
    if base == overlay {
        return sample;
    }
    let b = paint_blend(sample) as f32 / 255.0;
    let nb = (b + (target - b).clamp(-budget, budget)).clamp(0.0, 1.0);
    let byte = (nb * 255.0).round() as u8;
    match byte {
        0 => paint_sample(base, base, 0, 0),
        255 => paint_sample(overlay, overlay, 0, 0),
        _ => paint_sample(base, overlay, byte, 0),
    }
}

/// A fixed 0..1 value per tile.
fn tile_hash(tx: i32, ty: i32) -> f32 {
    let mut h = (tx as u32).wrapping_mul(0x9E37_79B9) ^ (ty as u32).wrapping_mul(0x85EB_CA6B);
    h ^= h >> 15;
    h = h.wrapping_mul(0x2C1B_3C6D);
    h ^= h >> 12;
    (h & 0xFFFF) as f32 / 65535.0
}

/// LMB over the terrain: the sub-tool under the disc. Alt is the
/// eyedropper (`pick_layer`), so it never paints.
pub fn apply_paint_brush(
    mouse: Res<ButtonInput<MouseButton>>,
    keys: Res<ButtonInput<KeyCode>>,
    viewport: Res<ViewportInput>,
    brush: Res<BrushSettings>,
    brush_hit: Res<TerrainBrushHit>,
    terrain_world: Option<Res<TerrainWorld>>,
    mut state: ResMut<PaintToolState>,
    paintmap: Option<ResMut<PaintmapData>>,
    heightmap: Option<Res<HeightmapData>>,
    mut images: ResMut<Assets<Image>>,
    registry: Option<ResMut<TerrainChunkRegistry>>,
    time: Res<Time>,
) {
    let Some(hit) = brush_hit.hit else {
        return;
    };
    if !viewport.pointer_free() || !mouse.pressed(MouseButton::Left) || alt(&keys) {
        return;
    }
    let (Some(terrain_world), Some(mut paintmap), Some(mut registry), Some(heightmap)) =
        (terrain_world, paintmap, registry, heightmap)
    else {
        return;
    };
    let layer_count = terrain_world.metadata.palette.layers.len();
    if layer_count == 0 {
        return;
    }
    let palette = &terrain_world.metadata.palette;
    let rules = &terrain_world.metadata.rules;
    let layer = state.selected.min(layer_count - 1) as u8;
    let from = state.replace_from.min(layer_count - 1) as u8;

    let radius = brush.radius;
    let center_tx = (hit.x / TILE_SIZE.0).round() as i32;
    let center_ty = (hit.z / TILE_SIZE.1).round() as i32;
    let extent = (radius / TILE_SIZE.0.min(TILE_SIZE.1)) as i32 + 1;
    let frame_flow = state.flow * time.delta_secs();
    let erase = ctrl(&keys);
    let sub_tool = state.sub_tool;

    // Smooth reads its neighbours from a snapshot taken before any write
    // this frame (one tile of margin so rim tiles have a full 3×3).
    let snap_extent = extent + 1;
    let snap_w = (2 * snap_extent + 1) as usize;
    let snapshot: Option<Vec<Option<PaintSample>>> =
        (sub_tool == PaintSubTool::Smooth).then(|| {
            let mut v = Vec::with_capacity(snap_w * snap_w);
            for dy in -snap_extent..=snap_extent {
                for dx in -snap_extent..=snap_extent {
                    v.push(paintmap.sample_at(center_tx + dx, center_ty + dy));
                }
            }
            v
        });
    let snap = |v: &[Option<PaintSample>], tx: i32, ty: i32| -> Option<PaintSample> {
        let lx = (tx - (center_tx - snap_extent)) as usize;
        let ly = (ty - (center_ty - snap_extent)) as usize;
        v[ly * snap_w + lx]
    };

    let mut touched: HashSet<(i32, i32)> = HashSet::new();
    for dx in -extent..=extent {
        for dy in -extent..=extent {
            let (tx, ty) = (center_tx + dx, center_ty + dy);
            let dist = Vec2::new(
                tx as f32 * TILE_SIZE.0 - hit.x,
                ty as f32 * TILE_SIZE.1 - hit.z,
            )
            .length();
            if dist > radius {
                continue;
            }
            let Some(current) = paintmap.sample_at(tx, ty) else {
                continue;
            };
            if palette.is_locked(dominant_layer(current)) {
                continue;
            }
            let weight = brush_weight(dist, radius, brush.falloff)
                * state.masks.weight(&heightmap, tx, ty, current);
            if weight <= 0.0 {
                continue;
            }
            let next = match sub_tool {
                PaintSubTool::Fill => paint_sample(layer, layer, 0, 0),
                PaintSubTool::Paint => {
                    let delta = frame_flow * weight * if erase { -1.0 } else { 1.0 };
                    paint_step(current, layer, delta)
                }
                PaintSubTool::Smooth => {
                    let v = snapshot.as_deref().expect("snapshot built for Smooth");
                    let mut sum = 0.0;
                    let mut n = 0.0;
                    for ny in -1..=1 {
                        for nx in -1..=1 {
                            if let Some(s) = snap(v, tx + nx, ty + ny)
                                && same_pair(s, current)
                            {
                                sum += paint_blend(s) as f32;
                                n += 1.0;
                            }
                        }
                    }
                    if n == 0.0 {
                        continue;
                    }
                    let avg = sum / n / 255.0;
                    step_blend(current, avg, frame_flow * weight)
                }
                PaintSubTool::Jitter => {
                    if paint_base(current) == paint_overlay(current) {
                        continue;
                    }
                    // Deterministic per tile so holding the button converges
                    // on a fixed ragged edge instead of boiling.
                    let target = tile_hash(tx, ty);
                    step_blend(current, target, frame_flow * weight)
                }
                PaintSubTool::Replace => {
                    if from == layer {
                        continue;
                    }
                    remap_sample(current, from, layer)
                }
                PaintSubTool::Auto => {
                    if erase {
                        // Hand the tile back: same look, no longer auto.
                        with_flags(current, paint_flags(current) & !PAINT_FLAG_AUTO)
                    } else {
                        let (h, slope, xz) = tile_height_slope(&heightmap, tx, ty);
                        rules_sample(rules, h, slope, xz)
                    }
                }
            };
            // A hand-painted tile is never auto.
            let next = if sub_tool == PaintSubTool::Auto {
                next
            } else {
                with_flags(next, paint_flags(next) & !PAINT_FLAG_AUTO)
            };
            if next == current {
                continue;
            }
            // Snapshot each chunk's grid before its first change this stroke.
            for chunk in chunks_touching_tile(tx, ty) {
                if paintmap.is_loaded(chunk) && !state.stroke_before.contains_key(&chunk) {
                    state.stroke_before.insert(chunk, paintmap.encode(chunk));
                }
            }
            touched.extend(paintmap.write_tile(&mut images, tx, ty, next));
        }
    }
    for chunk in touched {
        registry.dirty_chunks.insert(chunk);
    }
    state.was_painting = true;
}

/// Button released: record the stroke as one transaction.
pub fn finalize_paint_stroke(
    mouse: Res<ButtonInput<MouseButton>>,
    mut state: ResMut<PaintToolState>,
    paintmap: Option<Res<PaintmapData>>,
    mut history: ResMut<History>,
    mut lighting: ResMut<crate::editor::edits::LightingState>,
) {
    if mouse.pressed(MouseButton::Left) || !state.was_painting {
        return;
    }
    state.was_painting = false;
    let Some(paintmap) = paintmap else {
        state.stroke_before.clear();
        return;
    };
    let label = state.sub_tool.label();
    let chunks: Vec<PaintChunkChange> = state
        .stroke_before
        .drain()
        .map(|(chunk, before)| PaintChunkChange {
            chunk,
            before,
            after: paintmap.encode(chunk),
        })
        .filter(|c| c.before != c.after)
        .collect();
    if !chunks.is_empty() {
        history.push(format!("{label} terrain"), vec![Change::Paint { chunks }]);
        // A tile's layer changes its bake albedo, so the baked bounce light
        // no longer matches what is painted.
        lighting.stale = true;
    }
}

/// Alt+LMB: select the dominant layer under the cursor.
pub fn pick_layer(
    mouse: Res<ButtonInput<MouseButton>>,
    keys: Res<ButtonInput<KeyCode>>,
    viewport: Res<ViewportInput>,
    brush_hit: Res<TerrainBrushHit>,
    paintmap: Option<Res<PaintmapData>>,
    mut state: ResMut<PaintToolState>,
) {
    if !viewport.pointer_free() || !alt(&keys) || !mouse.just_pressed(MouseButton::Left) {
        return;
    }
    let (Some(hit), Some(paintmap)) = (brush_hit.hit, paintmap) else {
        return;
    };
    let tx = (hit.x / TILE_SIZE.0).round() as i32;
    let ty = (hit.z / TILE_SIZE.1).round() as i32;
    if let Some(sample) = paintmap.sample_at(tx, ty) {
        let picked = dominant_layer(sample) as usize;
        if state.sub_tool == PaintSubTool::Replace {
            state.replace_from = picked;
        } else {
            state.selected = picked;
        }
    }
}

/// Run a queued [`PaintRequest`] over every loaded chunk, as one
/// transaction (plus the palette change, for a removal).
pub fn apply_paint_requests(
    mut state: ResMut<PaintToolState>,
    terrain_world: Option<ResMut<TerrainWorld>>,
    paintmap: Option<ResMut<PaintmapData>>,
    heightmap: Option<Res<HeightmapData>>,
    brush_hit: Res<TerrainBrushHit>,
    mut images: ResMut<Assets<Image>>,
    registry: Option<ResMut<TerrainChunkRegistry>>,
    mut history: ResMut<History>,
    mut lighting: ResMut<crate::editor::edits::LightingState>,
) {
    let Some(request) = state.request.take() else {
        return;
    };
    // Every request repaints tiles, which changes what the ground bounces.
    lighting.stale = true;
    let (Some(mut terrain_world), Some(mut paintmap), Some(mut registry), Some(heightmap)) =
        (terrain_world, paintmap, registry, heightmap)
    else {
        return;
    };
    let palette = &terrain_world.metadata.palette;
    let layer_count = palette.layers.len();
    if layer_count == 0 {
        return;
    }
    let selected = state.selected.min(layer_count - 1) as u8;

    // What each loaded chunk should become, and the label.
    let (label, rewrite): (
        String,
        Box<dyn Fn(&PaintmapData, (i32, i32)) -> Option<PaintData>>,
    ) = match request {
        PaintRequest::FillAll => {
            let after = PaintData::Uniform(paint_sample(selected, selected, 0, 0));
            let name = palette.layers[selected as usize].name.clone();
            (
                format!("Fill terrain with '{name}'"),
                Box::new(move |_, _| Some(after.clone())),
            )
        }
        PaintRequest::ReplaceAll { from, to } => {
            if from == to || palette.is_locked(from) {
                return;
            }
            let (fname, tname) = (
                palette
                    .layer(from)
                    .map(|l| l.name.clone())
                    .unwrap_or_default(),
                palette
                    .layer(to)
                    .map(|l| l.name.clone())
                    .unwrap_or_default(),
            );
            (
                format!("Replace '{fname}' with '{tname}'"),
                Box::new(move |pm, chunk| {
                    pm.remapped(chunk, from, to).map(|g| PaintData::encode(&g))
                }),
            )
        }
        PaintRequest::RemoveLayer { layer, fallback } => {
            let name = palette
                .layer(layer)
                .map(|l| l.name.clone())
                .unwrap_or_default();
            (
                format!("Remove layer '{name}'"),
                Box::new(move |pm, chunk| {
                    pm.remapped(chunk, layer, fallback)
                        .map(|g| PaintData::encode(&g))
                }),
            )
        }
        PaintRequest::ApplyRules { .. } => {
            let rules = terrain_world.metadata.rules.clone();
            let heightmap = &heightmap;
            (
                "Apply paint rules".to_owned(),
                Box::new(move |_, chunk| {
                    let cs = (crate::game::world::terrain::CHUNK_SIZE - 1) as i32;
                    let (sx, sy) = (chunk.0 * cs, chunk.1 * cs);
                    let n = crate::game::world::terrain::CHUNK_SIZE;
                    let mut grid = vec![0u32; n * n];
                    for lx in 0..n {
                        for ly in 0..n {
                            let (h, slope, xz) =
                                tile_height_slope(heightmap, sx + lx as i32, sy + ly as i32);
                            grid[crate::game::world::terrain::paint::paint_index(lx, ly)] =
                                rules_sample(&rules, h, slope, xz);
                        }
                    }
                    Some(PaintData::encode(&grid))
                }),
            )
        }
    };

    let loaded: Vec<(i32, i32)> = match request {
        PaintRequest::ApplyRules { all_loaded: false } => match brush_hit.hit {
            Some(hit) => {
                let tx = (hit.x / TILE_SIZE.0).round() as i32;
                let ty = (hit.z / TILE_SIZE.1).round() as i32;
                chunks_touching_tile(tx, ty).take(1).collect()
            }
            None => Vec::new(),
        },
        _ => paintmap.loaded().collect(),
    };
    let mut changes = Vec::new();
    for chunk in loaded {
        let before = paintmap.encode(chunk);
        let Some(after) = rewrite(&paintmap, chunk) else {
            continue;
        };
        if before == after {
            continue;
        }
        if paintmap.replace_chunk(&mut images, chunk, after.decode()) {
            registry.dirty_chunks.insert(chunk);
            changes.push(PaintChunkChange {
                chunk,
                before,
                after,
            });
        }
    }

    let mut transaction = Vec::new();
    if !changes.is_empty() {
        transaction.push(Change::Paint { chunks: changes });
    }
    if let PaintRequest::RemoveLayer { layer, fallback } = request {
        let before = terrain_world.metadata.palette.clone();
        let mut after = before.clone();
        if after.remove(layer, fallback) {
            terrain_world.metadata.palette = after.clone();
            transaction.push(Change::Palette { before, after });
            if state.selected == layer as usize {
                state.selected = fallback as usize;
            }
        }
    }
    if !transaction.is_empty() {
        history.push(label, transaction);
    }
}

/// After a sculpt stroke, auto tiles in the chunks it changed follow the
/// new heights. Runs under the Terrain tool.
pub fn reevaluate_auto_after_sculpt(
    mut brush_hit: ResMut<TerrainBrushHit>,
    terrain_world: Option<Res<TerrainWorld>>,
    heightmap: Option<Res<HeightmapData>>,
    paintmap: Option<ResMut<PaintmapData>>,
    mut images: ResMut<Assets<Image>>,
    registry: Option<ResMut<TerrainChunkRegistry>>,
) {
    if brush_hit.just_finalized.is_empty() {
        return;
    }
    let chunks = std::mem::take(&mut brush_hit.just_finalized);
    let (Some(tw), Some(hm), Some(mut pm), Some(mut registry)) =
        (terrain_world, heightmap, paintmap, registry)
    else {
        return;
    };
    for chunk in chunks {
        if reevaluate_auto(&tw.metadata.rules, &hm, &mut pm, &mut images, chunk) {
            registry.dirty_chunks.insert(chunk);
        }
    }
}

/// Seconds of no rule edits before loaded chunks' auto tiles are re-run.
const RULES_SETTLE_SECS: f64 = 0.4;

/// Rules edited (panel or undo): once the edit settles, re-run every loaded
/// chunk's auto tiles.
pub fn reevaluate_auto_on_rules_change(
    mut state: ResMut<PaintToolState>,
    terrain_world: Option<Res<TerrainWorld>>,
    heightmap: Option<Res<HeightmapData>>,
    paintmap: Option<ResMut<PaintmapData>>,
    mut images: ResMut<Assets<Image>>,
    registry: Option<ResMut<TerrainChunkRegistry>>,
    time: Res<Time>,
) {
    let Some(tw) = terrain_world else {
        state.rules_seen = None;
        state.rules_changed_at = None;
        return;
    };
    let now = time.elapsed_secs_f64();
    match &state.rules_seen {
        None => {
            // First sight of this world's rules: nothing to re-run.
            state.rules_seen = Some(tw.metadata.rules.clone());
            return;
        }
        Some(seen) if *seen != tw.metadata.rules => {
            state.rules_seen = Some(tw.metadata.rules.clone());
            state.rules_changed_at = Some(now);
            return;
        }
        _ => {}
    }
    let Some(at) = state.rules_changed_at else {
        return;
    };
    if now - at < RULES_SETTLE_SECS {
        return;
    }
    state.rules_changed_at = None;
    let (Some(hm), Some(mut pm), Some(mut registry)) = (heightmap, paintmap, registry) else {
        return;
    };
    let loaded: Vec<(i32, i32)> = pm.loaded().collect();
    for chunk in loaded {
        if reevaluate_auto(&tw.metadata.rules, &hm, &mut pm, &mut images, chunk) {
            registry.dirty_chunks.insert(chunk);
        }
    }
}

/// Dress the shared brush disc in the selected layer's pattern and tint so
/// you can see what you're about to paint. The disc is torn down on leaving
/// the tool, so Terrain gets its plain one back.
pub fn style_paint_disc(
    terrain_world: Option<Res<TerrainWorld>>,
    mut state: ResMut<PaintToolState>,
    asset_server: Res<AssetServer>,
    mut materials: ResMut<Assets<UnlitMaterial>>,
    disc: Query<&MeshMaterial3d<UnlitMaterial>, With<BrushPreviewMarker>>,
) {
    let Some(terrain_world) = terrain_world else {
        return;
    };
    let Ok(handle) = disc.single() else {
        return;
    };
    let Some(layer) = terrain_world.metadata.palette.layers.get(state.selected) else {
        return;
    };
    let key = (state.selected, layer.texture.clone(), layer.tint);
    if state.disc_layer.as_ref() == Some(&key) && materials.get(&handle.0).is_some() {
        return;
    }
    if let Some(mut mat) = materials.get_mut(&handle.0) {
        mat.base_color = Color::linear_rgba(layer.tint[0], layer.tint[1], layer.tint[2], 0.55);
        mat.base_color_texture = Some(asset_server.load::<Image>(layer.texture.clone()));
        state.disc_layer = Some(key);
    }
}

/// Leaving the tool: forget the disc dressing (the disc itself is despawned
/// by `teardown_terrain_brush_preview`) and drop any half stroke.
pub fn reset_paint_tool(mut state: ResMut<PaintToolState>) {
    state.disc_layer = None;
    state.stroke_before.clear();
    state.was_painting = false;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn painting_the_overlay_saturates_into_the_base() {
        let s = paint_sample(0, 2, 200, 0);
        let s = paint_step(s, 2, 0.5);
        assert_eq!(s, paint_sample(2, 2, 0, 0));
    }

    #[test]
    fn painting_the_base_lowers_the_blend_and_erase_too() {
        let s = paint_sample(0, 2, 100, 0);
        assert_eq!(paint_step(s, 0, 0.1), paint_sample(0, 2, 74, 0));
        assert_eq!(paint_step(s, 0, 1.0), paint_sample(0, 0, 0, 0));
        assert_eq!(paint_step(s, 5, -0.1), paint_sample(0, 2, 74, 0));
        assert_eq!(paint_step(s, 5, -1.0), paint_sample(0, 0, 0, 0));
    }

    #[test]
    fn a_third_layer_snaps_the_dominant_layer_to_base() {
        let mostly_overlay = paint_sample(0, 2, 200, 0);
        assert_eq!(
            paint_step(mostly_overlay, 7, 0.1),
            paint_sample(2, 7, 26, 0)
        );
        let mostly_base = paint_sample(0, 2, 50, 0);
        assert_eq!(paint_step(mostly_base, 7, 0.1), paint_sample(0, 7, 26, 0));
    }

    #[test]
    fn step_blend_moves_by_at_most_the_budget_and_collapses_at_the_ends() {
        let s = paint_sample(0, 2, 100, 0);
        assert_eq!(step_blend(s, 1.0, 0.1), paint_sample(0, 2, 126, 0));
        assert_eq!(step_blend(s, 0.0, 1.0), paint_sample(0, 0, 0, 0));
        assert_eq!(step_blend(s, 1.0, 1.0), paint_sample(2, 2, 0, 0));
        assert_eq!(
            step_blend(paint_sample(3, 3, 0, 0), 1.0, 1.0),
            paint_sample(3, 3, 0, 0)
        );
    }

    #[test]
    fn a_tile_already_fully_the_layer_is_untouched() {
        let s = paint_sample(3, 3, 0, 0);
        assert_eq!(paint_step(s, 3, 0.4), s);
        let unpainted = paint_sample(0, 0, 0, 0);
        assert_eq!(paint_step(unpainted, 0, 0.4), unpainted);
    }
}
