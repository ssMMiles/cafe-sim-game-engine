//! Keeps the GPU side of the terrain palette in step with the world's
//! `TerrainPalette` (`docs/TERRAIN_PAINTING_PLAN.md` §4.1).
//!
//! Two things derive from the palette: the `2d_array` of layer patterns
//! every chunk material binds, and the per-layer uniform (tint, tiling, …).
//! The uniform is cheap and rewritten whenever the palette resource changes;
//! the array is rebuilt only when the *textures* or the array size change,
//! and only once every source image has loaded. Until then the materials
//! keep whatever array they had (the 1×1 placeholder on a fresh world), so
//! terrain is never un-drawable, just un-patterned.

use bevy::{asset::LoadState, prelude::*};

use super::{
    bootstrap::TerrainRenderAssets,
    material::{TerrainMaterial, TerrainPaletteGpu},
    paint::{build_palette_array, layer_bake_albedo, pattern_dark_factor},
    storage::TerrainWorld,
};

#[derive(Resource, Default)]
pub struct PaletteTextures {
    /// The layer textures the current request is for, in layer order, with
    /// their handles. Compared against the palette to notice a change.
    sources: Vec<(String, Handle<Image>)>,
    /// Array size the current request is for.
    size: u32,
    /// A rebuild is owed once every source has finished loading.
    pending: bool,
    /// Paths already reported as failed, so the warning fires once each.
    failed: Vec<String>,
    /// Each source pattern's mean dark factor (`paint::pattern_dark_factor`),
    /// measured when the array was last rebuilt; drives the derived
    /// per-layer `bake_albedo`.
    dark_factors: Vec<f32>,
}

pub fn sync_palette(
    terrain_world: Option<ResMut<TerrainWorld>>,
    render_assets: Option<Res<TerrainRenderAssets>>,
    asset_server: Res<AssetServer>,
    mut images: ResMut<Assets<Image>>,
    mut materials: ResMut<Assets<TerrainMaterial>>,
    mut state: ResMut<PaletteTextures>,
) {
    let (Some(mut terrain_world), Some(render_assets)) = (terrain_world, render_assets) else {
        // Between worlds: forget the old request so the next world starts
        // clean (its handles may well be the same paths, which is fine).
        if !state.sources.is_empty() {
            *state = PaletteTextures::default();
        }
        return;
    };
    // ── Textures: request loads when the layer list changes ──────────────
    let wanted: Vec<String> = terrain_world
        .metadata
        .palette
        .layers
        .iter()
        .map(|l| l.texture.clone())
        .collect();
    let same_sources = state.sources.len() == wanted.len()
        && state.sources.iter().zip(&wanted).all(|((p, _), w)| p == w);
    if !same_sources
        || state.size != terrain_world.metadata.palette.texture_size
        || render_assets.is_added()
    {
        state.sources = wanted
            .iter()
            .map(|path| {
                let handle = state
                    .sources
                    .iter()
                    .find(|(p, _)| p == path)
                    .map(|(_, h)| h.clone())
                    .unwrap_or_else(|| asset_server.load::<Image>(path.to_string()));
                (path.to_string(), handle)
            })
            .collect();
        state.size = terrain_world.metadata.palette.texture_size;
        state.pending = true;
    }

    // ── Array: build once everything is in ───────────────────────────────
    if state.pending {
        let mut ready = true;
        let mut newly_failed = Vec::new();
        for (path, handle) in &state.sources {
            if images.get(handle).is_some() {
                continue;
            }
            match asset_server.get_load_state(handle) {
                Some(LoadState::Failed(err)) => {
                    if !state.failed.iter().any(|f| f == path) {
                        warn!(
                            "palette texture '{path}' failed to load ({err}); layer renders flat"
                        );
                        newly_failed.push(path.clone());
                    }
                }
                _ => ready = false,
            }
        }
        state.failed.extend(newly_failed);
        if ready {
            let flat = Image::new_fill(
                bevy::render::render_resource::Extent3d::default(),
                bevy::render::render_resource::TextureDimension::D2,
                &[255, 255, 255, 255],
                bevy::render::render_resource::TextureFormat::Rgba8UnormSrgb,
                bevy::asset::RenderAssetUsages::MAIN_WORLD,
            );
            let (array, factors) = {
                let sources: Vec<&Image> = state
                    .sources
                    .iter()
                    .map(|(_, h)| images.get(h).unwrap_or(&flat))
                    .collect();
                let factors = sources.iter().map(|img| pattern_dark_factor(img)).collect();
                (build_palette_array(&sources, state.size), factors)
            };
            state.dark_factors = factors;
            let array = images.add(array);
            for (_, mat) in materials.iter_mut() {
                mat.layers = array.clone();
            }
            state.pending = false;
            info!(
                "terrain palette array rebuilt: {} layer(s) at {}px",
                state.sources.len(),
                state.size
            );
        }
    }

    // ── Bake albedo: derived, never authored ──────────────────────────────
    //
    // What a bounce or probe ray should see where a layer is painted is the
    // colour the *screen* shows there: the shader's own layer colour with the
    // pattern replaced by its mean dark factor (`paint::layer_bake_albedo`).
    // Recomputed whenever the palette changes (a tint drag) or the patterns
    // finish loading; written through `bypass_change_detection` because it is
    // derived bookkeeping — not an edit, not undoable, and it must not
    // retrigger this system or the uniform sync below. Shift+S persists it
    // with the palette, so the bakers never need the textures in memory.
    {
        let world = terrain_world.bypass_change_detection();
        for (i, layer) in world.metadata.palette.layers.iter_mut().enumerate() {
            let Some(factor) = state.dark_factors.get(i).copied() else {
                break; // array not built yet this world; keep saved values
            };
            let derived = layer_bake_albedo(layer, factor);
            if layer.bake_albedo != derived {
                layer.bake_albedo = derived;
            }
        }
    }

    // ── Uniform: follow every palette edit ───────────────────────────────
    if terrain_world.is_changed() || render_assets.is_added() {
        let gpu = TerrainPaletteGpu::from_palette(&terrain_world.metadata.palette);
        let base_differs = materials
            .get(&render_assets.material)
            .is_none_or(|m| m.palette != gpu);
        if base_differs {
            for (_, mat) in materials.iter_mut() {
                mat.palette = gpu;
            }
        }
    }
}
