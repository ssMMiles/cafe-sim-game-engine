use bevy::{
    prelude::*,
    render::render_resource::{AsBindGroup, ShaderType},
};

use super::paint::{MAX_LAYERS, TerrainPalette};

/// Terrain's per-material uniform. Mirrors `TerrainShading` in
/// `terrain_toon.wesl`.
///
/// The toon curve used to be here too (and in `ToonShading`), written by
/// `sync_toon_look` into every material; it is per scene, not per material,
/// and travels in the scene pass's uniform now (`scene_pass::SceneUniform`),
/// so terrain and the objects standing on it read literally the same ramp.
/// The base colour went the same way when painting arrived: colour is per
/// *layer* now (`TerrainPaletteGpu`), so this is down to the debug toggle.
#[derive(Clone, Copy, Debug, ShaderType)]
pub struct TerrainShading {
    /// 1 = shade from the baked lightmap, 0 = flat unlit debug view (Shift+L).
    pub use_lightmap: u32,
    /// A [`TerrainDebugMode`](crate::editor::lightmap_toggle::TerrainDebugMode)
    /// as a byte: 0 = off, 1 = colour by layer id, 2 = highlight auto tiles.
    pub debug: u32,
    pub _pad: Vec2,
}

impl Default for TerrainShading {
    fn default() -> Self {
        Self {
            use_lightmap: 1,
            debug: 0,
            _pad: Vec2::ZERO,
        }
    }
}

/// One palette layer as the shader reads it. Mirrors `TerrainLayer` in
/// `terrain_toon.wesl`.
#[derive(Clone, Copy, Debug, ShaderType, PartialEq)]
pub struct TerrainLayerGpu {
    /// Linear RGB tint; `w` = pattern strength.
    pub tint: Vec4,
    /// `x` = repeats per metre (1 / tiling), `y` = 1 for the stochastic
    /// no-tile sampler, `z` = blend profile, `w` = the profile's parameter.
    pub params: Vec4,
    /// `x` = the layer id this slot displays as (itself, or a removed
    /// slot's fallback — `TerrainPalette::resolve`); `yzw` unused.
    pub misc: Vec4,
}

impl Default for TerrainLayerGpu {
    fn default() -> Self {
        Self {
            tint: Vec4::new(0.6, 0.6, 0.6, 0.0),
            params: Vec4::new(1.0 / 3.0, 1.0, 0.0, 0.5),
            misc: Vec4::ZERO,
        }
    }
}

/// The whole palette, one fixed-size uniform so a layer index from the
/// paint map can address it directly. Mirrors `TerrainPaletteUniform` in
/// `terrain_toon.wesl`.
///
/// Per material rather than per scene: a palette edit rewrites every loaded
/// chunk's material (≤ 81 of them, editor-only, rare) — the same price a
/// bake landing pays — which keeps the scene uniform out of it.
#[derive(Clone, Copy, Debug, ShaderType, PartialEq)]
pub struct TerrainPaletteGpu {
    /// How many entries are live; ids at or past this clamp to the last one.
    pub layer_count: u32,
    pub _pad: Vec3,
    pub layers: [TerrainLayerGpu; MAX_LAYERS],
}

impl Default for TerrainPaletteGpu {
    fn default() -> Self {
        Self {
            layer_count: 1,
            _pad: Vec3::ZERO,
            layers: [TerrainLayerGpu::default(); MAX_LAYERS],
        }
    }
}

impl TerrainPaletteGpu {
    pub fn from_palette(palette: &TerrainPalette) -> Self {
        let mut out = Self {
            layer_count: palette.layers.len().clamp(1, MAX_LAYERS) as u32,
            ..Default::default()
        };
        for (i, (slot, layer)) in out.layers.iter_mut().zip(&palette.layers).enumerate() {
            *slot = TerrainLayerGpu {
                tint: Vec4::new(
                    layer.tint[0],
                    layer.tint[1],
                    layer.tint[2],
                    layer.pattern_strength,
                ),
                params: Vec4::new(
                    1.0 / layer.tiling.max(0.01),
                    if layer.no_tile { 1.0 } else { 0.0 },
                    layer.blend_profile as f32,
                    layer.blend_param.clamp(0.0, 1.0),
                ),
                misc: Vec4::new(palette.resolve(i as u8) as f32, 0.0, 0.0, 0.0),
            };
        }
        out
    }
}

#[derive(Asset, TypePath, AsBindGroup, Debug, Clone)]
pub struct TerrainMaterial {
    /// Every palette layer's pattern, one array slice per layer
    /// (`paint::build_palette_array`). Shared by every chunk; swapped on all
    /// of them by `sync_palette` when the palette changes.
    #[texture(100, dimension = "2d_array")]
    #[sampler(101)]
    pub layers: Handle<Image>,

    /// Per-chunk baked HDR lightmap: outgoing radiance in lightmap units
    /// (`Rgba16Float`, see `baking::lightmap`). Defaults to the shared 1×1
    /// white texel so an unbaked chunk renders fully lit rather than black.
    #[texture(102)]
    #[sampler(103)]
    pub lightmap: Handle<Image>,

    #[uniform(104)]
    pub shading: TerrainShading,

    /// Per-chunk paint map (`paint::paint_image`): one `Rgba8Unorm` texel per
    /// tile, read with `textureLoad` — no sampler.
    #[texture(105)]
    pub paint: Handle<Image>,

    #[uniform(106)]
    pub palette: TerrainPaletteGpu,
}

/// Terrain is **not** drawn by `MaterialPlugin` any more: `gfx::scene_pass`
/// owns its extraction, pipelines and passes (docs/SCENE_PASS_PLAN.md,
/// Phase 1). This impl survives only because `MeshMaterial3d<M>` — the
/// authoring component, which deliberately did not change — is bounded on
/// `M: Material`. Nothing reads it.
///
/// The shaders live in `assets/shaders/scene/terrain_vertex.wesl` and
/// `assets/shaders/terrain_toon.wesl`, and are named by
/// `scene_pass::pipeline::TerrainScenePipeline`. They are *not* usable
/// through bevy's mesh pipeline: they read our bind group 0, not
/// `mesh_view` + `mesh`. Naming them here would be a trap, so the defaults
/// stand.
impl Material for TerrainMaterial {
    /// See `ToonMaterial::enable_shadows`: nothing casts real-time shadows.
    fn enable_shadows() -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::world::terrain::paint::TerrainLayerData;

    #[test]
    fn palette_uniform_carries_every_layer_and_clamps_the_count() {
        let mut palette = TerrainPalette::default();
        let mut grass = TerrainLayerData::new("Grass", "textures/grass.png");
        grass.tint = [0.1, 0.5, 0.2];
        grass.pattern_strength = 0.3;
        grass.tiling = 2.0;
        grass.no_tile = false;
        palette.layers.push(grass);

        let gpu = TerrainPaletteGpu::from_palette(&palette);
        assert_eq!(gpu.layer_count, 2);
        assert_eq!(gpu.layers[1].tint, Vec4::new(0.1, 0.5, 0.2, 0.3));
        assert_eq!(gpu.layers[1].params.x, 0.5);
        assert_eq!(gpu.layers[1].params.y, 0.0);
        assert_eq!(gpu.layers[1].misc.x, 1.0);

        // A removed slot redirects to its fallback.
        palette.add(TerrainLayerData::new("Rock", "textures/rock.png"));
        assert!(palette.remove(1, 2));
        let gpu = TerrainPaletteGpu::from_palette(&palette);
        assert_eq!(gpu.layers[1].misc.x, 2.0);
        assert_eq!(gpu.layers[2].misc.x, 2.0);

        // An empty palette (never saved, but never crash) still reports one.
        palette.layers.clear();
        assert_eq!(TerrainPaletteGpu::from_palette(&palette).layer_count, 1);
    }
}
