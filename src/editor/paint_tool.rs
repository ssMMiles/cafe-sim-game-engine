//! Paint tool: paint the world's palette layers onto the terrain
//! (`docs/TERRAIN_PAINTING_PLAN.md` §4.4, Phase 1).
//!
//! The brush machinery — the hit under the cursor, the disc, `[`/`]`,
//! radius and falloff — is shared with the Terrain tool (`terrain_edit`);
//! this module owns what happens to the *paint* under it.
//!
//! Two sub-tools. **Paint** raises the blend towards the selected layer at
//! `flow` per second (Ctrl erases: lowers it back towards the base); a tile
//! whose layer pair doesn't include the selected layer snaps its base to
//! whichever of its two layers was dominant and starts blending the new one
//! in, so a transition zone always shares one pair end to end (§4.7). When
//! the blend saturates the overlay *becomes* the base. **Fill** sets every
//! tile in the disc to exactly the selected layer, no falloff. Alt+LMB picks
//! the dominant layer under the cursor.
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
    TILE_SIZE,
    paint::{
        PaintData, PaintSample, PaintmapData, paint_base, paint_blend, paint_overlay, paint_sample,
    },
    storage::{TerrainChunkRegistry, TerrainWorld},
};

/// Blend change per second at full brush weight, in 0..1 of the blend range.
pub const PAINT_FLOW_RANGE: std::ops::RangeInclusive<f32> = 0.2..=8.0;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum PaintSubTool {
    #[default]
    Paint,
    Fill,
}

impl PaintSubTool {
    pub fn label(self) -> &'static str {
        match self {
            PaintSubTool::Paint => "Paint",
            PaintSubTool::Fill => "Fill",
        }
    }
}

#[derive(Resource)]
pub struct PaintToolState {
    /// Palette index of the layer the brush paints with.
    pub selected: usize,
    pub sub_tool: PaintSubTool,
    /// Blend change per second at full weight (Paint sub-tool).
    pub flow: f32,
    /// Set by the panel: fill every loaded chunk with the selected layer.
    pub fill_all_requested: bool,
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
            fill_all_requested: false,
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

/// LMB over the terrain: Paint or Fill under the disc. Alt is the
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
    let (Some(terrain_world), Some(mut paintmap), Some(mut registry)) =
        (terrain_world, paintmap, registry)
    else {
        return;
    };
    let layer_count = terrain_world.metadata.palette.layers.len();
    if layer_count == 0 {
        return;
    }
    let layer = state.selected.min(layer_count - 1) as u8;

    let radius = brush.radius;
    let center_tx = (hit.x / TILE_SIZE.0).round() as i32;
    let center_ty = (hit.z / TILE_SIZE.1).round() as i32;
    let extent = (radius / TILE_SIZE.0.min(TILE_SIZE.1)) as i32 + 1;
    let frame_flow = state.flow * time.delta_secs();
    let erase = ctrl(&keys);
    let fill = state.sub_tool == PaintSubTool::Fill;

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
            let next = if fill {
                paint_sample(layer, layer, 0, 0)
            } else {
                let weight = brush_weight(dist, radius, brush.falloff);
                let delta = frame_flow * weight * if erase { -1.0 } else { 1.0 };
                paint_step(current, layer, delta)
            };
            if next == current {
                continue;
            }
            // Snapshot each chunk's grid before its first change this stroke.
            for chunk in crate::game::world::terrain::chunks_touching_tile(tx, ty) {
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
        state.selected = dominant_layer(sample) as usize;
    }
}

/// The panel's "Fill loaded chunks": every loaded chunk becomes the
/// selected layer, as one transaction.
pub fn fill_loaded_chunks(
    mut state: ResMut<PaintToolState>,
    terrain_world: Option<Res<TerrainWorld>>,
    paintmap: Option<ResMut<PaintmapData>>,
    mut images: ResMut<Assets<Image>>,
    registry: Option<ResMut<TerrainChunkRegistry>>,
    mut history: ResMut<History>,
) {
    if !state.fill_all_requested {
        return;
    }
    state.fill_all_requested = false;
    let (Some(terrain_world), Some(mut paintmap), Some(mut registry)) =
        (terrain_world, paintmap, registry)
    else {
        return;
    };
    let layer_count = terrain_world.metadata.palette.layers.len();
    if layer_count == 0 {
        return;
    }
    let layer = state.selected.min(layer_count - 1) as u8;
    let after = PaintData::Uniform(paint_sample(layer, layer, 0, 0));
    let loaded: Vec<(i32, i32)> = registry.loaded_chunks.keys().copied().collect();
    let mut changes = Vec::new();
    for chunk in loaded {
        let before = paintmap.encode(chunk);
        if before == after {
            continue;
        }
        if paintmap.replace_chunk(&mut images, chunk, after.decode()) {
            registry.dirty_chunks.insert(chunk);
            changes.push(PaintChunkChange {
                chunk,
                before,
                after: after.clone(),
            });
        }
    }
    if !changes.is_empty() {
        let name = &terrain_world.metadata.palette.layers[layer as usize].name;
        history.push(
            format!("Fill terrain with '{name}'"),
            vec![Change::Paint { chunks: changes }],
        );
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
    fn a_tile_already_fully_the_layer_is_untouched() {
        let s = paint_sample(3, 3, 0, 0);
        assert_eq!(paint_step(s, 3, 0.4), s);
        let unpainted = paint_sample(0, 0, 0, 0);
        assert_eq!(paint_step(unpainted, 0, 0.4), unpainted);
    }
}
