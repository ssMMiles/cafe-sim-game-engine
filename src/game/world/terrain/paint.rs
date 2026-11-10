//! Terrain painting data: which ground material shows where.
//!
//! `docs/TERRAIN_PAINTING_PLAN.md`. The model is a **material-ID map**, not a
//! weight per layer: every heightmap tile carries one [`PaintSample`] — a
//! *base* layer, an *overlay* layer, how far the overlay has been blended in,
//! and a few flag bits — so the shader's cost is constant whatever the
//! palette size, nothing can un-normalise, and two materials per tile is all
//! a toon look needs.
//!
//! Three things live here:
//!
//! * the sample encoding and the on-disk [`PaintData`] (inside the `.terrain`
//!   chunk file since format v3 — paint is level data edited, dirtied and
//!   saved with the heights);
//! * [`TerrainPalette`], the world's layer list (in `world.meta` since v7);
//! * [`PaintmapData`], the runtime grid per loaded chunk, and the builders
//!   that turn a chunk's grid into its paint-map `Image` and the palette's
//!   textures into the one `2d_array` every chunk material binds.

use std::collections::HashMap;

use bevy::{
    asset::RenderAssetUsages,
    image::{ImageAddressMode, ImageFilterMode, ImageSampler, ImageSamplerDescriptor},
    prelude::*,
    render::render_resource::{
        Extent3d, TextureDimension, TextureFormat, TextureViewDescriptor, TextureViewDimension,
    },
};
use rkyv::{Archive, Deserialize, Serialize};

use super::{CHUNK_SIZE, chunks_touching_tile};

// ============================================================================
// Samples
// ============================================================================

/// One tile's paint, packed little-endian so the same `u32` is the
/// `Rgba8Unorm` texel the shader reads: R = base layer, G = overlay layer,
/// B = blend (0 = all base, 255 = all overlay), A = flags.
pub type PaintSample = u32;

/// The sample a tile has when nothing was ever painted: layer 0, no overlay.
pub const PAINT_DEFAULT: PaintSample = 0;

/// Flag bit: the tile's layers come from the world's rules, not a brush.
/// Nothing evaluates it yet (Phase 3).
#[allow(dead_code)]
pub const PAINT_FLAG_AUTO: u8 = 1;

/// Most layers a palette can hold. Ids are a byte on disk, but the shader's
/// uniform holds exactly this many entries, so it is the real cap.
pub const MAX_LAYERS: usize = 32;

/// Samples per chunk side; equals the heightmap's vertex count so paint and
/// heights share tile coordinates (and the one-row overlap between chunks).
pub const PAINT_SIZE: usize = CHUNK_SIZE;

pub const fn paint_sample(base: u8, overlay: u8, blend: u8, flags: u8) -> PaintSample {
    (base as u32) | ((overlay as u32) << 8) | ((blend as u32) << 16) | ((flags as u32) << 24)
}

pub const fn paint_base(s: PaintSample) -> u8 {
    (s & 0xFF) as u8
}

pub const fn paint_overlay(s: PaintSample) -> u8 {
    ((s >> 8) & 0xFF) as u8
}

pub const fn paint_blend(s: PaintSample) -> u8 {
    ((s >> 16) & 0xFF) as u8
}

pub const fn paint_flags(s: PaintSample) -> u8 {
    (s >> 24) as u8
}

// ============================================================================
// On disk
// ============================================================================

/// A chunk's paint grid as stored in its `.terrain` file.
///
/// Row-major in `lx * PAINT_SIZE + ly` order — the same order the heights
/// use. Most chunks are one layer end to end, and a painted one is long runs
/// of identical samples, so this is run-length coded rather than dense.
#[derive(Archive, Deserialize, Serialize, Clone, Debug, PartialEq)]
pub enum PaintData {
    /// Every tile has this sample.
    Uniform(PaintSample),
    /// Runs covering exactly `PAINT_SIZE * PAINT_SIZE` tiles.
    Rle(Vec<PaintRun>),
}

#[derive(Archive, Deserialize, Serialize, Clone, Copy, Debug, PartialEq)]
pub struct PaintRun {
    pub count: u32,
    pub sample: PaintSample,
}

impl Default for PaintData {
    fn default() -> Self {
        PaintData::Uniform(PAINT_DEFAULT)
    }
}

impl PaintData {
    /// Encode a dense grid, collapsing to [`PaintData::Uniform`] when it can.
    pub fn encode(samples: &[PaintSample]) -> Self {
        debug_assert_eq!(samples.len(), PAINT_SIZE * PAINT_SIZE);
        let Some(&first) = samples.first() else {
            return PaintData::default();
        };
        if samples.iter().all(|&s| s == first) {
            return PaintData::Uniform(first);
        }
        let mut runs: Vec<PaintRun> = Vec::new();
        for &s in samples {
            match runs.last_mut() {
                Some(run) if run.sample == s => run.count += 1,
                _ => runs.push(PaintRun {
                    count: 1,
                    sample: s,
                }),
            }
        }
        PaintData::Rle(runs)
    }

    /// Decode to a dense grid of `PAINT_SIZE * PAINT_SIZE` samples. A
    /// malformed run list (wrong total) is clamped / padded with the default
    /// rather than trusted, so a bad file can't produce a short buffer.
    pub fn decode(&self) -> Vec<PaintSample> {
        let total = PAINT_SIZE * PAINT_SIZE;
        match self {
            PaintData::Uniform(s) => vec![*s; total],
            PaintData::Rle(runs) => {
                let mut out = Vec::with_capacity(total);
                for run in runs {
                    let n = (run.count as usize).min(total - out.len());
                    out.extend(std::iter::repeat_n(run.sample, n));
                    if out.len() == total {
                        break;
                    }
                }
                if out.len() != total {
                    warn!(
                        "paint RLE covered {} of {} tiles; padding with the default layer",
                        out.len(),
                        total
                    );
                    out.resize(total, PAINT_DEFAULT);
                }
                out
            }
        }
    }
}

// ============================================================================
// Palette
// ============================================================================

/// How a layer's blend amount becomes a visible transition. Stored as a byte;
/// only [`BlendProfile::Soft`] is implemented (Phase 0).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
#[repr(u8)]
pub enum BlendProfile {
    /// Linear fade across the painted blend.
    #[default]
    Soft = 0,
    /// Pinched into a one-pixel antialiased step.
    Hard = 1,
    /// Height-channel modulated: the overlay fills the base's low spots.
    Height = 2,
    /// Dithered patches.
    Scatter = 3,
    /// Quantised into a few bands, like the lighting.
    Stepped = 4,
}

impl BlendProfile {
    pub fn from_u8(v: u8) -> Self {
        match v {
            1 => BlendProfile::Hard,
            2 => BlendProfile::Height,
            3 => BlendProfile::Scatter,
            4 => BlendProfile::Stepped,
            _ => BlendProfile::Soft,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            BlendProfile::Soft => "Soft",
            BlendProfile::Hard => "Hard",
            BlendProfile::Height => "Height",
            BlendProfile::Scatter => "Scatter",
            BlendProfile::Stepped => "Stepped",
        }
    }

    pub const ALL: [BlendProfile; 5] = [
        BlendProfile::Soft,
        BlendProfile::Hard,
        BlendProfile::Height,
        BlendProfile::Scatter,
        BlendProfile::Stepped,
    ];

    /// What the layer's `blend_param` means under this profile (`None`:
    /// unused). Mirrors `blend_amount` in `terrain_toon.wesl`.
    pub fn param_label(self) -> Option<&'static str> {
        match self {
            BlendProfile::Soft => None,
            BlendProfile::Hard => Some("Hardness"),
            BlendProfile::Height => Some("Contrast"),
            BlendProfile::Scatter => Some("Patch size"),
            BlendProfile::Stepped => Some("Steps"),
        }
    }

    pub fn param_hint(self) -> &'static str {
        match self {
            BlendProfile::Soft => "",
            BlendProfile::Hard => "1 = a one-pixel cel edge, 0 = as soft as Soft",
            BlendProfile::Height => {
                "How much the patterns' light/dark decides where the overlay appears first"
            }
            BlendProfile::Scatter => "Size of the dithered patches (0 = 25 cm, 1 = 4 m)",
            BlendProfile::Stepped => "Number of bands across the transition (2–8)",
        }
    }
}

/// One ground material. The texture is a flat greyscale *pattern*: where it
/// is dark the tint is pulled towards black by `pattern_strength`, so one
/// pattern serves any number of coloured layers and all light and shade come
/// from the lightmap, as the toon look wants.
#[derive(Archive, Deserialize, Serialize, Clone, Debug, PartialEq)]
pub struct TerrainLayerData {
    pub name: String,
    /// Asset path of the pattern (`textures/….png`).
    pub texture: String,
    /// Linear RGB base colour.
    pub tint: [f32; 3],
    /// 0 = the pattern is ignored (flat tint); 1 = its dark parts go fully
    /// to the pattern colour.
    pub pattern_strength: f32,
    /// World metres per texture repeat.
    pub tiling: f32,
    /// Sample through the stochastic no-tile blend (hides repetition).
    pub no_tile: bool,
    /// [`BlendProfile`] as a byte.
    pub blend_profile: u8,
    /// The profile's one knob, 0..1 ([`BlendProfile::param_label`]).
    pub blend_param: f32,
    /// Linear RGB the bakers use as this layer's albedo (Phase 4).
    pub bake_albedo: [f32; 3],
    /// Brushes refuse to paint over tiles this layer dominates.
    pub locked: bool,
    /// A removed layer. The slot stays, because chunk files not loaded at
    /// removal time still carry its id: the shader shows `fallback` for it
    /// (`TerrainPalette::resolve`), loaded chunks are rewritten to the
    /// fallback at removal (`PaintmapData::remap_layer`), and the rest
    /// migrate as they are painted. Hidden from the palette UI; a new layer
    /// may reuse the slot, in which case an unmigrated chunk shows the new
    /// layer where the old one was.
    pub removed: bool,
    /// Layer a removed slot displays as.
    pub fallback: u8,
}

impl TerrainLayerData {
    pub fn new(name: impl Into<String>, texture: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            texture: texture.into(),
            tint: [0.6, 0.6, 0.6],
            pattern_strength: 0.85,
            tiling: 3.0,
            no_tile: true,
            blend_profile: BlendProfile::Soft as u8,
            blend_param: 0.5,
            bake_albedo: [0.5, 0.5, 0.5],
            locked: false,
            removed: false,
            fallback: 0,
        }
    }
}

/// The asset every new world's layer 0 wears: plain grey triangles, so
/// unpainted ground is visibly unpainted.
pub const DEFAULT_LAYER_TEXTURE: &str = "textures/terrain_default.png";

/// The pattern terrain used before painting existed; legacy worlds keep it
/// as their layer 0 so they look the same the day the format changes.
pub const LEGACY_STONE_TEXTURE: &str = "textures/terrain.png";

/// The world's layer list. Layer 0 always exists and is what unpainted tiles
/// show; its texture may be swapped but it can't be removed.
#[derive(Archive, Deserialize, Serialize, Clone, Debug, PartialEq)]
pub struct TerrainPalette {
    pub layers: Vec<TerrainLayerData>,
    /// Pixel size every layer texture is resampled to for the GPU array.
    pub texture_size: u32,
}

impl Default for TerrainPalette {
    fn default() -> Self {
        // A regular geometric pattern must tile plainly: the no-tile sampler
        // offsets each lattice cell randomly, which on triangles reads as
        // overlapping shards. It stays on for organic patterns (the stone).
        let mut default = TerrainLayerData::new("Default", DEFAULT_LAYER_TEXTURE);
        default.no_tile = false;
        default.tiling = 2.0;
        Self {
            layers: vec![default],
            texture_size: 256,
        }
    }
}

impl TerrainPalette {
    /// The palette a pre-v7 world is given on upgrade: the old hard-coded
    /// stone look — `terrain.png` at 3 m, darkening sRGB (0.81, 0.62, 0.60).
    pub fn legacy_stone() -> Self {
        let mut stone = TerrainLayerData::new("Stone", LEGACY_STONE_TEXTURE);
        let tint = LinearRgba::from(Color::srgb(0.81, 0.62, 0.60));
        stone.tint = [tint.red, tint.green, tint.blue];
        stone.bake_albedo = stone.tint;
        Self {
            layers: vec![stone],
            texture_size: 256,
        }
    }

    pub fn layer(&self, id: u8) -> Option<&TerrainLayerData> {
        self.layers.get(id as usize)
    }

    /// No room for another layer: every slot is live.
    pub fn is_full(&self) -> bool {
        self.layers.len() >= MAX_LAYERS && self.layers.iter().all(|l| !l.removed)
    }

    /// `(id, layer)` for every live (not removed) layer.
    pub fn live(&self) -> impl Iterator<Item = (u8, &TerrainLayerData)> {
        self.layers
            .iter()
            .enumerate()
            .filter(|(_, l)| !l.removed)
            .map(|(i, l)| (i as u8, l))
    }

    /// What id `id` displays as: itself, or a removed slot's fallback
    /// (followed once; a fallback that is itself removed falls to 0).
    pub fn resolve(&self, id: u8) -> u8 {
        match self.layers.get(id as usize) {
            Some(l) if l.removed => match self.layers.get(l.fallback as usize) {
                Some(f) if !f.removed => l.fallback,
                _ => 0,
            },
            Some(_) => id,
            None => 0,
        }
    }

    pub fn is_locked(&self, id: u8) -> bool {
        self.layers
            .get(id as usize)
            .is_some_and(|l| l.locked && !l.removed)
    }

    /// Add a layer: into the first removed slot if any, else appended.
    /// Returns its id, or `None` when full.
    pub fn add(&mut self, layer: TerrainLayerData) -> Option<u8> {
        if let Some(i) = self.layers.iter().position(|l| l.removed) {
            self.layers[i] = layer;
            return Some(i as u8);
        }
        if self.layers.len() >= MAX_LAYERS {
            return None;
        }
        self.layers.push(layer);
        Some((self.layers.len() - 1) as u8)
    }

    /// Retire `id`, displaying as `fallback` from now on. Layer 0 and the
    /// fallback itself are refused.
    pub fn remove(&mut self, id: u8, fallback: u8) -> bool {
        if id == 0 || id == fallback || self.resolve(fallback) != fallback {
            return false;
        }
        let Some(l) = self.layers.get_mut(id as usize) else {
            return false;
        };
        l.removed = true;
        l.locked = false;
        l.fallback = fallback;
        true
    }
}

// ============================================================================
// Runtime grid
// ============================================================================

/// One loaded chunk's paint: the dense grid plus the `Image` the chunk's
/// material samples (kept so a brush can rewrite texels in place).
pub struct ChunkPaint {
    pub samples: Vec<PaintSample>,
    /// The brush rewrites its texels in place.
    pub image: Handle<Image>,
}

/// Paint for every loaded chunk, the counterpart of `HeightmapData`.
/// Dense per chunk rather than a sparse tile map: a painted chunk is painted
/// everywhere, and the per-chunk grid is what the GPU needs anyway.
#[derive(Resource, Default)]
pub struct PaintmapData {
    chunks: HashMap<(i32, i32), ChunkPaint>,
}

impl PaintmapData {
    pub fn insert(&mut self, chunk: (i32, i32), paint: ChunkPaint) {
        self.chunks.insert(chunk, paint);
    }

    pub fn remove(&mut self, chunk: (i32, i32)) -> Option<ChunkPaint> {
        self.chunks.remove(&chunk)
    }

    pub fn is_loaded(&self, chunk: (i32, i32)) -> bool {
        self.chunks.contains_key(&chunk)
    }

    /// The grid to save for `chunk` — the default everywhere if it isn't
    /// loaded (a dirty chunk is always loaded; this is the safe fallback).
    pub fn encode(&self, chunk: (i32, i32)) -> PaintData {
        self.chunks
            .get(&chunk)
            .map(|c| PaintData::encode(&c.samples))
            .unwrap_or_default()
    }

    /// The sample at global tile `(tx, ty)`, from whichever loaded chunk
    /// holds it (every chunk sharing an edge tile holds the same value).
    pub fn sample_at(&self, tx: i32, ty: i32) -> Option<PaintSample> {
        chunks_touching_tile(tx, ty).find_map(|chunk| {
            let (lx, ly) = local_tile(chunk, tx, ty);
            self.chunks
                .get(&chunk)
                .map(|c| c.samples[paint_index(lx, ly)])
        })
    }

    /// Write one global tile into every loaded chunk that holds it, grid and
    /// GPU image both. Returns the chunks written (to mark dirty).
    pub fn write_tile(
        &mut self,
        images: &mut Assets<Image>,
        tx: i32,
        ty: i32,
        sample: PaintSample,
    ) -> impl Iterator<Item = (i32, i32)> + '_ {
        let touched: Vec<(i32, i32)> = chunks_touching_tile(tx, ty)
            .filter(|chunk| {
                let Some(c) = self.chunks.get_mut(chunk) else {
                    return false;
                };
                let (lx, ly) = local_tile(*chunk, tx, ty);
                c.samples[paint_index(lx, ly)] = sample;
                if let Some(mut image) = images.get_mut(&c.image) {
                    write_paint_texel(&mut image, lx, ly, sample);
                }
                true
            })
            .collect();
        touched.into_iter()
    }

    /// Every loaded chunk id, for whole-world operations.
    pub fn loaded(&self) -> impl Iterator<Item = (i32, i32)> + '_ {
        self.chunks.keys().copied()
    }

    /// `from` → `to` in every sample of a loaded chunk's grid (base and
    /// overlay; a pair that collapses to one layer drops its blend). Returns
    /// the new grid if anything changed.
    pub fn remapped(&self, chunk: (i32, i32), from: u8, to: u8) -> Option<Vec<PaintSample>> {
        let c = self.chunks.get(&chunk)?;
        let mut changed = false;
        let out: Vec<PaintSample> = c
            .samples
            .iter()
            .map(|&s| {
                let n = remap_sample(s, from, to);
                changed |= n != s;
                n
            })
            .collect();
        changed.then_some(out)
    }

    /// Replace a loaded chunk's whole grid (undo/redo, fills). Rebuilds its
    /// image in one go.
    pub fn replace_chunk(
        &mut self,
        images: &mut Assets<Image>,
        chunk: (i32, i32),
        samples: Vec<PaintSample>,
    ) -> bool {
        let Some(c) = self.chunks.get_mut(&chunk) else {
            return false;
        };
        debug_assert_eq!(samples.len(), PAINT_SIZE * PAINT_SIZE);
        if let Some(mut image) = images.get_mut(&c.image) {
            *image = paint_image(&samples);
        }
        c.samples = samples;
        true
    }
}

/// One sample with layer `from` replaced by `to`. A pair that becomes one
/// layer is normalised to `(to, to, 0)`; flags are kept.
pub fn remap_sample(s: PaintSample, from: u8, to: u8) -> PaintSample {
    let base = paint_base(s);
    let overlay = paint_overlay(s);
    if base != from && overlay != from {
        return s;
    }
    let nb = if base == from { to } else { base };
    let no = if overlay == from { to } else { overlay };
    if nb == no {
        paint_sample(nb, no, 0, paint_flags(s))
    } else {
        paint_sample(nb, no, paint_blend(s), paint_flags(s))
    }
}

/// Global tile → local tile within `chunk` (caller guarantees it's inside).
pub fn local_tile(chunk: (i32, i32), tx: i32, ty: i32) -> (usize, usize) {
    let cs = (PAINT_SIZE - 1) as i32;
    (
        (tx - chunk.0 * cs).clamp(0, cs) as usize,
        (ty - chunk.1 * cs).clamp(0, cs) as usize,
    )
}

/// Index of local tile `(lx, ly)` in a chunk grid — the heights' order.
pub const fn paint_index(lx: usize, ly: usize) -> usize {
    lx * PAINT_SIZE + ly
}

// ============================================================================
// GPU images
// ============================================================================

/// The chunk's paint map as the `Rgba8Unorm` texture its material binds.
/// Texel `(x = lx, y = ly)`; the shader reads it with `textureLoad`, so no
/// sampler is involved and the bytes are exactly the packed samples.
pub fn paint_image(samples: &[PaintSample]) -> Image {
    debug_assert_eq!(samples.len(), PAINT_SIZE * PAINT_SIZE);
    let n = PAINT_SIZE as u32;
    let mut data = vec![0u8; samples.len() * 4];
    for ly in 0..PAINT_SIZE {
        for lx in 0..PAINT_SIZE {
            let s = samples[paint_index(lx, ly)];
            let o = (ly * PAINT_SIZE + lx) * 4;
            data[o..o + 4].copy_from_slice(&s.to_le_bytes());
        }
    }
    let mut image = Image::new(
        Extent3d {
            width: n,
            height: n,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        data,
        TextureFormat::Rgba8Unorm,
        // The main world keeps its copy: the brush rewrites texels in place.
        RenderAssetUsages::MAIN_WORLD | RenderAssetUsages::RENDER_WORLD,
    );
    image.sampler = ImageSampler::nearest();
    image
}

/// Write one sample into an existing paint image (the brush's path).
pub fn write_paint_texel(image: &mut Image, lx: usize, ly: usize, sample: PaintSample) {
    if let Some(data) = image.data.as_mut() {
        let o = (ly * PAINT_SIZE + lx) * 4;
        if o + 4 <= data.len() {
            data[o..o + 4].copy_from_slice(&sample.to_le_bytes());
        }
    }
}

/// The sampler every palette array uses: repeat, trilinear.
fn palette_sampler() -> ImageSampler {
    ImageSampler::Descriptor(ImageSamplerDescriptor {
        address_mode_u: ImageAddressMode::Repeat,
        address_mode_v: ImageAddressMode::Repeat,
        mag_filter: ImageFilterMode::Linear,
        min_filter: ImageFilterMode::Linear,
        mipmap_filter: ImageFilterMode::Linear,
        ..ImageSamplerDescriptor::default()
    })
}

/// A one-layer, one-texel white array, so a chunk material is complete
/// before any palette texture has loaded (and the shader's layer taps return
/// "no pattern", i.e. the plain tint).
pub fn placeholder_palette_array() -> Image {
    let mut image = Image::new(
        Extent3d {
            width: 1,
            height: 1,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        vec![255, 255, 255, 255],
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::RENDER_WORLD,
    );
    image.texture_view_descriptor = Some(TextureViewDescriptor {
        dimension: Some(TextureViewDimension::D2Array),
        ..default()
    });
    image.sampler = palette_sampler();
    image
}

/// Pack the palette's loaded textures into one `2d_array`, every layer
/// resampled to `size` × `size` with a full mip chain.
///
/// Resampling is bilinear and the mips are 2×2 box filters, both on the
/// stored (sRGB-encoded) bytes. Patterns are near-binary greys, so the
/// gamma error of averaging in sRGB space is invisible here; revisit if a
/// layer texture ever carries real colour.
///
/// `textures[i]` is layer `i`'s image in any 8-bit RGBA format; anything
/// else is converted first. A texture that can't be read becomes a flat
/// white layer (no pattern) and is reported.
pub fn build_palette_array(textures: &[&Image], size: u32) -> Image {
    let size = size.clamp(1, 4096).next_power_of_two();
    let layers = textures.len().max(1) as u32;
    let mip_count = 32 - size.leading_zeros(); // log2(size) + 1
    let mut data: Vec<u8> = Vec::new();

    for i in 0..layers as usize {
        let mut level = match textures.get(i) {
            Some(img) => resample_rgba8(img, size),
            None => vec![255u8; (size * size * 4) as usize],
        };
        let mut w = size;
        // Layer-major: every mip of this layer, then the next layer.
        data.extend_from_slice(&level);
        for _ in 1..mip_count {
            level = downsample_box(&level, w);
            w /= 2;
            data.extend_from_slice(&level);
        }
    }

    // `Image::new` asserts the data is exactly one mip's worth; this carries
    // the whole chain (layer-major, which is bevy's default `data_order`).
    let mut image = Image::new_uninit(
        Extent3d {
            width: size,
            height: size,
            depth_or_array_layers: layers,
        },
        TextureDimension::D2,
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::RENDER_WORLD,
    );
    image.data = Some(data);
    image.texture_descriptor.mip_level_count = mip_count;
    image.texture_view_descriptor = Some(TextureViewDescriptor {
        dimension: Some(TextureViewDimension::D2Array),
        ..default()
    });
    image.sampler = palette_sampler();
    image
}

/// `img` as `size`×`size` RGBA8, bilinear. Non-8-bit sources are converted
/// through bevy's `convert`; a source that can't be is flat white.
fn resample_rgba8(img: &Image, size: u32) -> Vec<u8> {
    let converted;
    let src = match img.texture_descriptor.format {
        TextureFormat::Rgba8Unorm | TextureFormat::Rgba8UnormSrgb => img,
        _ => match img.convert(TextureFormat::Rgba8UnormSrgb) {
            Some(c) => {
                converted = c;
                &converted
            }
            None => {
                warn!(
                    "palette texture in {:?} can't be converted to RGBA8; using a flat layer",
                    img.texture_descriptor.format
                );
                return vec![255u8; (size * size * 4) as usize];
            }
        },
    };
    let (sw, sh) = (src.width() as usize, src.height() as usize);
    let Some(sdata) = src.data.as_deref() else {
        return vec![255u8; (size * size * 4) as usize];
    };
    if sw == 0 || sh == 0 || sdata.len() < sw * sh * 4 {
        return vec![255u8; (size * size * 4) as usize];
    }
    let n = size as usize;
    let mut out = vec![0u8; n * n * 4];
    let texel = |x: usize, y: usize| -> [f32; 4] {
        let o = (y.min(sh - 1) * sw + x.min(sw - 1)) * 4;
        [
            sdata[o] as f32,
            sdata[o + 1] as f32,
            sdata[o + 2] as f32,
            sdata[o + 3] as f32,
        ]
    };
    for y in 0..n {
        // Texel centres map to texel centres; wrap so a tiling source stays
        // seamless at the edge.
        let fy = ((y as f32 + 0.5) / n as f32) * sh as f32 - 0.5;
        let y0 = fy.floor();
        let ty = fy - y0;
        let y0 = (y0 as isize).rem_euclid(sh as isize) as usize;
        let y1 = (y0 + 1) % sh;
        for x in 0..n {
            let fx = ((x as f32 + 0.5) / n as f32) * sw as f32 - 0.5;
            let x0 = fx.floor();
            let tx = fx - x0;
            let x0 = (x0 as isize).rem_euclid(sw as isize) as usize;
            let x1 = (x0 + 1) % sw;
            let (a, b, c, d) = (texel(x0, y0), texel(x1, y0), texel(x0, y1), texel(x1, y1));
            let o = (y * n + x) * 4;
            for k in 0..4 {
                let top = a[k] + (b[k] - a[k]) * tx;
                let bottom = c[k] + (d[k] - c[k]) * tx;
                out[o + k] = (top + (bottom - top) * ty).round().clamp(0.0, 255.0) as u8;
            }
        }
    }
    out
}

/// One mip step: `w`×`w` RGBA8 → `w/2`×`w/2`, 2×2 box.
fn downsample_box(level: &[u8], w: u32) -> Vec<u8> {
    let w = w as usize;
    let h = (w / 2).max(1);
    let mut out = vec![0u8; h * h * 4];
    for y in 0..h {
        for x in 0..h {
            let o = (y * h + x) * 4;
            for k in 0..4 {
                let mut sum = 0u32;
                for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                    let sx = (2 * x + dx).min(w - 1);
                    let sy = (2 * y + dy).min(w - 1);
                    sum += level[(sy * w + sx) * 4 + k] as u32;
                }
                out[o + k] = ((sum + 2) / 4) as u8;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sample_packing_round_trips() {
        let s = paint_sample(7, 31, 200, PAINT_FLAG_AUTO);
        assert_eq!(paint_base(s), 7);
        assert_eq!(paint_overlay(s), 31);
        assert_eq!(paint_blend(s), 200);
        assert_eq!(paint_flags(s), PAINT_FLAG_AUTO);
        // The texel bytes the shader sees are R, G, B, A in that order.
        assert_eq!(s.to_le_bytes(), [7, 31, 200, PAINT_FLAG_AUTO]);
    }

    #[test]
    fn paint_data_collapses_uniform_and_round_trips_runs() {
        let total = PAINT_SIZE * PAINT_SIZE;
        let uniform = vec![paint_sample(2, 0, 0, 0); total];
        assert_eq!(
            PaintData::encode(&uniform),
            PaintData::Uniform(paint_sample(2, 0, 0, 0))
        );
        assert_eq!(PaintData::encode(&uniform).decode(), uniform);

        let mut painted = uniform.clone();
        for i in (100..5000).step_by(3) {
            painted[i] = paint_sample(2, 5, (i % 256) as u8, 0);
        }
        let encoded = PaintData::encode(&painted);
        assert!(matches!(encoded, PaintData::Rle(_)));
        assert_eq!(encoded.decode(), painted);
    }

    #[test]
    fn short_or_long_run_lists_still_decode_to_a_full_grid() {
        let total = PAINT_SIZE * PAINT_SIZE;
        let short = PaintData::Rle(vec![PaintRun {
            count: 10,
            sample: 3,
        }]);
        let d = short.decode();
        assert_eq!(d.len(), total);
        assert_eq!(d[9], 3);
        assert_eq!(d[10], PAINT_DEFAULT);

        let long = PaintData::Rle(vec![PaintRun {
            count: (total + 500) as u32,
            sample: 4,
        }]);
        assert_eq!(long.decode().len(), total);
    }

    #[test]
    fn paint_image_texel_order_matches_the_shader_lookup() {
        let total = PAINT_SIZE * PAINT_SIZE;
        let mut samples = vec![PAINT_DEFAULT; total];
        samples[paint_index(3, 10)] = paint_sample(1, 2, 3, 4);
        let img = paint_image(&samples);
        // Texel (x = lx = 3, y = ly = 10).
        let bytes = img.pixel_bytes(UVec3::new(3, 10, 0)).unwrap();
        assert_eq!(bytes, &[1, 2, 3, 4]);
    }

    #[test]
    fn palette_array_has_a_full_mip_chain_per_layer() {
        let mut a = Image::new_fill(
            Extent3d {
                width: 4,
                height: 4,
                depth_or_array_layers: 1,
            },
            TextureDimension::D2,
            &[200, 100, 50, 255],
            TextureFormat::Rgba8UnormSrgb,
            RenderAssetUsages::MAIN_WORLD,
        );
        // One dark texel so the resample has something to interpolate.
        a.pixel_bytes_mut(UVec3::new(0, 0, 0))
            .unwrap()
            .copy_from_slice(&[0, 0, 0, 255]);
        let b = Image::new_fill(
            Extent3d {
                width: 2,
                height: 2,
                depth_or_array_layers: 1,
            },
            TextureDimension::D2,
            &[10, 20, 30, 255],
            TextureFormat::Rgba8Unorm,
            RenderAssetUsages::MAIN_WORLD,
        );
        let array = build_palette_array(&[&a, &b], 8);
        assert_eq!(array.texture_descriptor.size.depth_or_array_layers, 2);
        assert_eq!(array.texture_descriptor.mip_level_count, 4); // 8, 4, 2, 1
        let per_layer = (64 + 16 + 4 + 1) * 4;
        assert_eq!(array.data.as_ref().unwrap().len(), per_layer * 2);
        // Layer 1's base mip starts after layer 0's whole chain and is flat.
        let l1 = &array.data.as_ref().unwrap()[per_layer..per_layer + 4];
        assert_eq!(l1, &[10, 20, 30, 255]);
        // Layer 0's 1×1 mip is the average: mostly (200,100,50) with one
        // black texel in sixteen pulling it down.
        let tail = &array.data.as_ref().unwrap()[per_layer - 4..per_layer];
        assert!(tail[0] > 150 && tail[0] < 200, "{tail:?}");
    }

    #[test]
    fn edge_tiles_belong_to_every_chunk_sharing_them() {
        let cs = (PAINT_SIZE - 1) as i32;
        let mut mid: Vec<_> = chunks_touching_tile(10, 10).collect();
        mid.sort();
        assert_eq!(mid, vec![(0, 0)]);
        let mut edge: Vec<_> = chunks_touching_tile(cs, 10).collect();
        edge.sort();
        assert_eq!(edge, vec![(0, 0), (1, 0)]);
        let mut corner: Vec<_> = chunks_touching_tile(cs, cs).collect();
        corner.sort();
        assert_eq!(corner, vec![(0, 0), (0, 1), (1, 0), (1, 1)]);
        assert_eq!(local_tile((1, 0), cs, 10), (0, 10));
        assert_eq!(local_tile((0, 0), cs, 10), (cs as usize, 10));
        // Negative chunks too.
        let mut neg: Vec<_> = chunks_touching_tile(-1, 0).collect();
        neg.sort();
        assert_eq!(neg, vec![(-1, -1), (-1, 0)]);
    }

    #[test]
    fn remap_collapses_pairs_and_keeps_flags() {
        assert_eq!(
            remap_sample(paint_sample(1, 2, 100, 1), 2, 1),
            paint_sample(1, 1, 0, 1)
        );
        assert_eq!(
            remap_sample(paint_sample(1, 2, 100, 0), 1, 3),
            paint_sample(3, 2, 100, 0)
        );
        assert_eq!(
            remap_sample(paint_sample(1, 2, 100, 0), 5, 3),
            paint_sample(1, 2, 100, 0)
        );
    }

    #[test]
    fn removed_layers_resolve_to_their_fallback_and_free_their_slot() {
        let mut p = TerrainPalette::default();
        let a = p.add(TerrainLayerData::new("a", "x")).unwrap();
        let b = p.add(TerrainLayerData::new("b", "y")).unwrap();
        assert_eq!((a, b), (1, 2));
        assert!(!p.remove(0, 1), "layer 0 is permanent");
        assert!(!p.remove(a, a), "a layer can't fall back to itself");
        assert!(p.remove(a, b));
        assert_eq!(p.resolve(a), b);
        assert_eq!(p.live().map(|(i, _)| i).collect::<Vec<_>>(), vec![0, 2]);
        assert!(!p.remove(b, a), "can't fall back to a removed layer");
        // The slot is reused.
        assert_eq!(p.add(TerrainLayerData::new("c", "z")), Some(1));
        assert_eq!(p.resolve(1), 1);
    }

    #[test]
    fn legacy_palette_reproduces_the_old_stone_tint() {
        let p = TerrainPalette::legacy_stone();
        assert_eq!(p.layers.len(), 1);
        assert_eq!(p.layers[0].texture, LEGACY_STONE_TEXTURE);
        let tint = LinearRgba::from(Color::srgb(0.81, 0.62, 0.60));
        assert!((p.layers[0].tint[0] - tint.red).abs() < 1e-6);
    }
}
