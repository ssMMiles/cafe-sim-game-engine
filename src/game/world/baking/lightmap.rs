//! Shared HDR lightmap plumbing used by every bake path and every loader.
//!
//! A lightmap texel is *outgoing radiance in lightmap units*: linear RGB where
//! `1.0` means "this surface shows its full base colour". The runtime toon
//! shaders do nothing but sample this, band it, and multiply by base colour,
//! so every light source — the sun and ambient today, point/spot lights next —
//! has to be folded in here at bake time. Texels are HDR (f16) so overlapping
//! lights can exceed 1.0 without clipping on disk; the shader clamps when it
//! bands.
//!
//! On disk: OpenEXR, f16 RGB, ZIP16. [`LIGHTMAP_FORMAT_VERSION`] is part of
//! the file name, so a bake from an older lighting model is never loaded by
//! mistake — the surface just looks unbaked and [`warn_stale_lightmaps`]
//! says why.

use bevy::{
    asset::RenderAssetUsages,
    image::ImageSampler,
    prelude::*,
    render::render_resource::{Extent3d, TextureDimension, TextureFormat},
};
use half::f16;
use std::path::{Path, PathBuf};

use crate::game::world::objects::PlacedKind;
use crate::game::world::terrain::storage::EnvironmentData;

/// Bump whenever the *meaning* of a lightmap texel changes (new light types
/// folded in, different units, …). Old files are then ignored, not misread.
///
/// - v1: 8-bit single-channel PNG, sun occlusion only (never had a version).
/// - v2: f16 RGB EXR radiance: sun + ambient×AO (+ terrain bounce).
/// - v3: terrain texels placed on the true chunk footprint
///   (`CHUNK_WORLD_SIZE`); object lightmaps are per sub-mesh.
/// - v4: per-triangle atlas UVs replace the overlapping triplanar
///   projection, so object lightmap texels mean something different.
/// - v5: placed point and spot lights are baked in too.
/// - v6: object texels are positioned with each sub-mesh's own transform
///   (v5 used the placed root's, so scaled/offset glTF nodes baked wrong).
pub const LIGHTMAP_FORMAT_VERSION: u32 = 6;

pub(crate) fn lightmap_file_name(stem: &str) -> String {
    format!("{stem}_lightmap.v{LIGHTMAP_FORMAT_VERSION}.exr")
}

/// Path of a terrain chunk's lightmap inside a world directory.
pub fn terrain_lightmap_path(world_path: &Path, cx: i32, cy: i32) -> PathBuf {
    world_path
        .join("chunks")
        .join(lightmap_file_name(&format!("chunk_{cx}_{cy}")))
}

// ============================================================================
// Lighting model
// ============================================================================

/// One placed point or spot light, flattened for baking.
///
/// `#[repr(C)]` and POD because the GPU terrain bake uploads these verbatim —
/// the layout must match `BakedLight` in `lightmap_bake.wgsl`. 16 floats keeps
/// every field `vec4`-aligned.
#[repr(C)]
#[derive(Copy, Clone, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct BakedLight {
    pub position: [f32; 3],
    /// `1 / range²`, precomputed for the falloff.
    pub inv_range_sq: f32,
    /// Colour × intensity, in lightmap units.
    pub color: [f32; 3],
    pub range: f32,
    /// Spot axis (unit). Unused by point lights.
    pub direction: [f32; 3],
    /// Cone terms, precomputed so the cone test is branchless:
    /// `spot = clamp(cos_angle * spot_scale + spot_offset, 0, 1)`.
    /// A point light stores `scale = 0, offset = 1`, i.e. "always lit".
    pub spot_scale: f32,
    pub spot_offset: f32,
    pub _pad: [f32; 3],
}

impl BakedLight {
    pub fn point(position: Vec3, color: Vec3, intensity: f32, range: f32) -> Self {
        Self {
            position: position.into(),
            inv_range_sq: 1.0 / range.max(1e-4).powi(2),
            color: (color * intensity).into(),
            range: range.max(1e-4),
            direction: [0.0, -1.0, 0.0],
            spot_scale: 0.0,
            spot_offset: 1.0,
            _pad: [0.0; 3],
        }
    }

    pub fn spot(
        position: Vec3,
        direction: Vec3,
        color: Vec3,
        intensity: f32,
        range: f32,
        inner_angle: f32,
        outer_angle: f32,
    ) -> Self {
        let cos_outer = outer_angle.cos();
        // Guard the degenerate inner == outer cone, which would divide by zero.
        let scale = 1.0 / (inner_angle.cos() - cos_outer).max(1e-4);
        Self {
            direction: direction.normalize_or_zero().into(),
            spot_scale: scale,
            spot_offset: -cos_outer * scale,
            ..Self::point(position, color, intensity, range)
        }
    }

    /// Build from a placed entity, or `None` if it isn't a light.
    pub fn from_placed(kind: &PlacedKind, transform: &GlobalTransform) -> Option<Self> {
        match kind {
            PlacedKind::PointLight {
                color,
                intensity,
                range,
                ..
            } => Some(Self::point(
                transform.translation(),
                Vec3::from(*color),
                *intensity,
                *range,
            )),
            PlacedKind::SpotLight {
                color,
                intensity,
                range,
                inner_angle,
                outer_angle,
                ..
            } => Some(Self::spot(
                transform.translation(),
                transform.forward().as_vec3(),
                Vec3::from(*color),
                *intensity,
                *range,
                *inner_angle,
                *outer_angle,
            )),
            _ => None,
        }
    }

    /// Radiance this light delivers to a surface *before* shadowing, with the
    /// direction and distance a shadow ray would need.
    ///
    /// `None` when the surface is out of range, facing away, or outside the
    /// cone — which lets the caller skip casting a ray that cannot matter.
    #[inline]
    pub fn sample(&self, world_pos: Vec3, normal: Vec3) -> Option<(Vec3, Vec3, f32)> {
        let to_light = Vec3::from(self.position) - world_pos;
        let dist_sq = to_light.length_squared();
        if dist_sq >= self.range * self.range || dist_sq < 1e-8 {
            return None;
        }
        let dist = dist_sq.sqrt();
        let dir = to_light / dist;

        let ndotl = normal.dot(dir);
        if ndotl <= 0.0 {
            return None;
        }

        // Smooth range falloff in [0, 1] rather than physical inverse-square,
        // matching the bounded, art-directable look the toon shading wants.
        let falloff = (1.0 - dist_sq * self.inv_range_sq).max(0.0);
        let attenuation = falloff * falloff;

        let cos_angle = (-dir).dot(Vec3::from(self.direction));
        let spot = (cos_angle * self.spot_scale + self.spot_offset).clamp(0.0, 1.0);

        let radiance = Vec3::from(self.color) * (ndotl * attenuation * spot);
        if radiance.max_element() <= 0.0 {
            return None;
        }
        Some((radiance, dir, dist))
    }
}

/// Everything the bakers need to know about the lights, resolved once per
/// bake so the CPU terrain, CPU object, and GPU terrain paths all shade with
/// identical numbers.
#[derive(Clone, Debug)]
pub struct BakeLighting {
    /// Unit vector pointing from a surface *toward* the sun.
    pub sun_dir: Vec3,
    /// Sun colour × intensity, in lightmap units.
    pub sun_rgb: Vec3,
    /// Ambient colour × intensity, in lightmap units.
    pub ambient_rgb: Vec3,
    /// Every placed point/spot light in the world — including ones outside
    /// the bake region, since a light just outside it still shines in.
    pub lights: Vec<BakedLight>,
}

impl BakeLighting {
    pub fn new(env: &EnvironmentData, sun_dir: Vec3, lights: Vec<BakedLight>) -> Self {
        Self {
            sun_dir,
            sun_rgb: env.sun_radiance(),
            ambient_rgb: env.ambient_radiance(),
            lights,
        }
    }

    /// Sun + ambient radiance for one texel.
    ///
    /// `shadow` is sun visibility (0 or 1), `ao` the unoccluded hemisphere
    /// fraction, and `bounce` indirect sun irradiance already scaled by
    /// `BakeConfig::bounce_strength`. AO shapes only the ambient term: direct
    /// sunlight is already attenuated by real shadow rays, and darkening it
    /// again with AO double-counts occlusion.
    #[inline]
    pub fn sun_and_ambient(&self, ndotl: f32, shadow: f32, ao: f32, bounce: f32) -> Vec3 {
        self.sun_rgb * (ndotl * shadow + bounce) + self.ambient_rgb * ao
    }

    /// Summed contribution of every placed light at a point.
    ///
    /// `visibility(dir_to_light, distance)` casts the shadow ray and returns
    /// 0 (occluded) or 1 (clear); it is only called for lights that actually
    /// reach the surface. Local lights do not contribute to indirect bounce.
    #[inline]
    pub fn local_lights(
        &self,
        world_pos: Vec3,
        normal: Vec3,
        mut visibility: impl FnMut(Vec3, f32) -> f32,
    ) -> Vec3 {
        let mut total = Vec3::ZERO;
        for light in &self.lights {
            if let Some((radiance, dir, dist)) = light.sample(world_pos, normal) {
                total += radiance * visibility(dir, dist);
            }
        }
        total
    }
}

// ============================================================================
// Texel buffer
// ============================================================================

/// A square HDR lightmap held as `f32` RGB while it is being baked, blurred,
/// or dilated. Converted to f16 only at the edges (GPU upload, EXR).
pub struct LightmapBuffer {
    pub res: usize,
    /// Row-major, `res × res`.
    pub texels: Vec<Vec3>,
}

impl LightmapBuffer {
    pub fn new(res: usize) -> Self {
        Self {
            res,
            texels: vec![Vec3::ZERO; res * res],
        }
    }

    /// Rebuild from `Rgba16Float` texel bytes (little-endian, 8 bytes per
    /// texel) — the layout the GPU bake writes and `to_rgba16_bytes` produces.
    pub fn from_rgba16_bytes(res: usize, bytes: &[u8]) -> Option<Self> {
        if bytes.len() != res * res * 8 {
            return None;
        }
        let texels = bytes
            .chunks_exact(8)
            .map(|px| {
                let ch = |i: usize| f16::from_le_bytes([px[i], px[i + 1]]).to_f32();
                Vec3::new(ch(0), ch(2), ch(4))
            })
            .collect();
        Some(Self { res, texels })
    }

    /// `Rgba16Float` texel bytes with alpha fixed at 1.0.
    pub fn to_rgba16_bytes(&self) -> Vec<u8> {
        let one = f16::ONE.to_le_bytes();
        let mut out = Vec::with_capacity(self.texels.len() * 8);
        for t in &self.texels {
            out.extend_from_slice(&f16::from_f32(t.x).to_le_bytes());
            out.extend_from_slice(&f16::from_f32(t.y).to_le_bytes());
            out.extend_from_slice(&f16::from_f32(t.z).to_le_bytes());
            out.extend_from_slice(&one);
        }
        out
    }

    /// GPU image ready to be bound as a material's lightmap.
    ///
    /// Every lightmap in the game — terrain CPU and GPU bakes, object bakes,
    /// and both load-from-EXR paths — is built here, so this is the one place
    /// the sampler has to be right.
    ///
    /// **The sampler is set explicitly and must stay that way.** `Image::new`
    /// leaves it as `ImageSampler::Default`, which resolves to whatever
    /// `ImagePlugin` was configured with, and `lib.rs` sets
    /// `ImagePlugin::default_nearest()` for the game's pixel-art look. A
    /// point-sampled lightmap returns a piecewise-*constant* value, so every
    /// toon band edge snaps to square texel blocks — and nothing downstream
    /// can recover from it: the shader's `fwidth` band antialiasing measures a
    /// rate of change that is zero inside every texel, the bake-side blur just
    /// produces a staircase of flat blocks instead of one hard edge, and extra
    /// resolution only makes the blocks smaller. The gutter-and-dilation
    /// machinery in `uv_unwrap` is likewise written for bilinear taps at
    /// triangle edges and is pointless without them.
    pub fn into_image(self) -> Image {
        let res = self.res as u32;
        let mut image = Image::new(
            Extent3d {
                width: res,
                height: res,
                depth_or_array_layers: 1,
            },
            TextureDimension::D2,
            self.to_rgba16_bytes(),
            TextureFormat::Rgba16Float,
            RenderAssetUsages::RENDER_WORLD,
        );
        image.sampler = ImageSampler::linear();
        image
    }

    /// Separable box blur. Removes per-texel Monte Carlo variance so the toon
    /// band quantiser sees smooth gradients instead of "lines of light".
    /// `radius == 0` returns a copy.
    ///
    /// For a terrain chunk the whole texture is one continuous surface, so
    /// the kernel may run anywhere. Object atlases are not — see
    /// [`Self::blur_cells`].
    pub fn blur(&self, radius: u32) -> Self {
        self.blur_impl(radius, self.res, None)
    }

    /// Box blur confined to `cell` × `cell` blocks, and to texels some
    /// triangle actually covered.
    ///
    /// An object atlas gives every triangle its own cell, so neighbouring
    /// texels in the atlas are usually *not* neighbours on the surface.
    /// [`Self::blur`] would smear one triangle's lighting into an unrelated
    /// one across the cell boundary. Confining the kernel keeps the blur
    /// within a single triangle, where texel adjacency does mean surface
    /// adjacency.
    ///
    /// `written` masks out the gutter: it is still empty at this point
    /// (dilation runs afterwards), and averaging black into a triangle's
    /// edge texels would draw a dark rim around every triangle.
    pub fn blur_cells(&self, radius: u32, cell: usize, written: &[bool]) -> Self {
        debug_assert_eq!(written.len(), self.texels.len());
        self.blur_impl(radius, cell, Some(written))
    }

    fn blur_impl(&self, radius: u32, cell: usize, written: Option<&[bool]>) -> Self {
        let n = self.res;
        if radius == 0 || cell == 0 {
            return Self {
                res: n,
                texels: self.texels.clone(),
            };
        }
        let r = radius as usize;
        let covered = |i: usize| written.is_none_or(|w| w[i]);

        // Window around `i` clamped to both the kernel radius and `i`'s cell.
        let window = |i: usize| {
            let cell_lo = (i / cell) * cell;
            let cell_hi = (cell_lo + cell).min(n);
            (i.saturating_sub(r).max(cell_lo), (i + r + 1).min(cell_hi))
        };

        // Horizontal pass, then vertical. Each accumulates only over covered
        // texels and leaves uncovered ones untouched, so the mask survives
        // both passes.
        let mut temp = self.texels.clone();
        for row in 0..n {
            for col in 0..n {
                let idx = row * n + col;
                if !covered(idx) {
                    continue;
                }
                let (lo, hi) = window(col);
                let (mut sum, mut count) = (Vec3::ZERO, 0u32);
                for c in lo..hi {
                    let j = row * n + c;
                    if covered(j) {
                        sum += self.texels[j];
                        count += 1;
                    }
                }
                if count > 0 {
                    temp[idx] = sum / count as f32;
                }
            }
        }

        let mut out = temp.clone();
        for col in 0..n {
            for row in 0..n {
                let idx = row * n + col;
                if !covered(idx) {
                    continue;
                }
                let (lo, hi) = window(row);
                let (mut sum, mut count) = (Vec3::ZERO, 0u32);
                for r2 in lo..hi {
                    let j = r2 * n + col;
                    if covered(j) {
                        sum += temp[j];
                        count += 1;
                    }
                }
                if count > 0 {
                    out[idx] = sum / count as f32;
                }
            }
        }

        Self {
            res: n,
            texels: out,
        }
    }

    /// Flood lit values outward into texels no triangle covered, `passes`
    /// texels at a time, so bilinear sampling near a triangle's edge can't
    /// pull in the black of an empty texel.
    ///
    /// Each pass takes the per-channel max of already-written neighbours and
    /// marks what it fills as written, so successive passes keep spreading.
    /// `passes` should cover the UV gutter — see `uv_unwrap::GUTTER_FRAC`.
    pub fn dilate(&mut self, written: &mut [bool], passes: u32) {
        debug_assert_eq!(written.len(), self.texels.len());
        let n = self.res as i32;

        for _ in 0..passes {
            let src = self.texels.clone();
            // Snapshot: a texel filled by this pass must not seed this same
            // pass, or filling would run further in +x/+y than elsewhere.
            let was_written = written.to_vec();
            let mut filled_any = false;

            for y in 0..n {
                for x in 0..n {
                    let idx = (y * n + x) as usize;
                    if was_written[idx] {
                        continue;
                    }
                    let mut best = Vec3::ZERO;
                    let mut any = false;
                    for dy in -1..=1 {
                        for dx in -1..=1 {
                            let (nx, ny) = (x + dx, y + dy);
                            if nx < 0 || ny < 0 || nx >= n || ny >= n {
                                continue;
                            }
                            let ni = (ny * n + nx) as usize;
                            if was_written[ni] {
                                best = best.max(src[ni]);
                                any = true;
                            }
                        }
                    }
                    if any {
                        self.texels[idx] = best;
                        written[idx] = true;
                        filled_any = true;
                    }
                }
            }

            if !filled_any {
                break;
            }
        }
    }

    /// Write as OpenEXR (f16 RGB, ZIP16). Creates parent directories.
    pub fn save_exr(&self, path: &Path) -> Result<(), Box<dyn std::error::Error>> {
        use exr::prelude::{Encoding, Image as ExrImage, SpecificChannels, WritableImage};

        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let res = self.res;
        let channels = SpecificChannels::rgb(|pos: exr::math::Vec2<usize>| {
            let t = self.texels[pos.1 * res + pos.0];
            (f16::from_f32(t.x), f16::from_f32(t.y), f16::from_f32(t.z))
        });
        ExrImage::from_encoded_channels((res, res), Encoding::SMALL_LOSSLESS, channels)
            .write()
            .to_file(path)?;
        Ok(())
    }

    /// Read a square EXR written by [`Self::save_exr`].
    pub fn load_exr(path: &Path) -> Result<Self, Box<dyn std::error::Error>> {
        let image = exr::prelude::read_first_rgba_layer_from_file(
            path,
            |size: exr::math::Vec2<usize>, _channels| LightmapBuffer {
                res: size.0,
                texels: vec![Vec3::ZERO; size.0 * size.1],
            },
            |buf: &mut LightmapBuffer,
             pos: exr::math::Vec2<usize>,
             (r, g, b, _a): (f32, f32, f32, f32)| {
                buf.texels[pos.1 * buf.res + pos.0] = Vec3::new(r, g, b);
            },
        )?;
        let buf = image.layer_data.channel_data.pixels;
        if buf.texels.len() != buf.res * buf.res {
            return Err(format!(
                "lightmap {} is not square ({} texels for res {})",
                path.display(),
                buf.texels.len(),
                buf.res
            )
            .into());
        }
        Ok(buf)
    }
}

// ============================================================================
// Loading helpers (native only — wasm has no synchronous file access yet)
// ============================================================================

/// Load a saved lightmap as a GPU-ready `Image`. `None` if the file is
/// absent; decode failures are logged and also yield `None` so the surface
/// falls back to the default (unbaked) lightmap. Safe to call off the main
/// thread — the terrain streamer decodes chunk lightmaps in its compute task.
#[cfg(not(target_arch = "wasm32"))]
pub fn try_load_lightmap(path: &Path) -> Option<Image> {
    if !path.exists() {
        return None;
    }
    match LightmapBuffer::load_exr(path) {
        Ok(buf) => Some(buf.into_image()),
        Err(e) => {
            warn!("Ignoring unreadable lightmap {}: {e}", path.display());
            None
        }
    }
}

/// [`try_load_lightmap`] plus registration in `Assets<Image>`.
#[cfg(not(target_arch = "wasm32"))]
pub fn try_load_lightmap_image(path: &Path, images: &mut Assets<Image>) -> Option<Handle<Image>> {
    try_load_lightmap(path).map(|image| images.add(image))
}

/// Count lightmap files in a world that don't match the current format
/// version and warn once. Stale bakes are silently skipped by the loaders;
/// this is what tells the user *why* the world looks unbaked.
#[cfg(not(target_arch = "wasm32"))]
pub fn warn_stale_lightmaps(world_path: &Path) {
    let suffix = format!("_lightmap.v{LIGHTMAP_FORMAT_VERSION}.exr");
    let mut stale = 0usize;
    for dir in ["chunks", "objects"] {
        let Ok(entries) = std::fs::read_dir(world_path.join(dir)) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.contains("_lightmap") && !name.ends_with(&suffix) {
                stale += 1;
            }
        }
    }
    if stale > 0 {
        warn!(
            "World '{}' has {stale} lightmap file(s) from an older bake format; they are ignored. \
             Press Shift+B to re-bake.",
            world_path.display()
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gradient(res: usize) -> LightmapBuffer {
        let mut buf = LightmapBuffer::new(res);
        for y in 0..res {
            for x in 0..res {
                // Includes values > 1.0 to prove HDR survives the round trip.
                buf.texels[y * res + x] = Vec3::new(x as f32 * 0.5, y as f32 * 0.25, 1.5);
            }
        }
        buf
    }

    #[test]
    fn rgba16_bytes_round_trip() {
        let buf = gradient(4);
        let bytes = buf.to_rgba16_bytes();
        assert_eq!(bytes.len(), 4 * 4 * 8);
        let back = LightmapBuffer::from_rgba16_bytes(4, &bytes).unwrap();
        for (a, b) in buf.texels.iter().zip(&back.texels) {
            assert!((*a - *b).abs().max_element() < 1e-2, "{a} vs {b}");
        }
        assert!(LightmapBuffer::from_rgba16_bytes(4, &bytes[1..]).is_none());
    }

    #[test]
    fn exr_round_trip_preserves_hdr() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("lm.v2.exr");
        let buf = gradient(8);
        buf.save_exr(&path).unwrap();
        let back = LightmapBuffer::load_exr(&path).unwrap();
        assert_eq!(back.res, 8);
        for (a, b) in buf.texels.iter().zip(&back.texels) {
            assert!((*a - *b).abs().max_element() < 1e-2, "{a} vs {b}");
        }
    }

    /// Lightmaps must carry their own sampler rather than inheriting the
    /// app-wide one. `lib.rs` sets `ImagePlugin::default_nearest()`, and a
    /// point-sampled lightmap is piecewise-constant: band edges land on
    /// square texel blocks, and no amount of bake resolution, blur, or
    /// shader-side antialiasing can smooth a signal that does not vary.
    /// This is the regression that made every shadow edge pixelated.
    #[test]
    fn baked_lightmaps_carry_an_explicit_linear_sampler() {
        use bevy::image::{ImageFilterMode, ImageSampler};

        let image = LightmapBuffer::new(4).into_image();
        match &image.sampler {
            ImageSampler::Default => panic!(
                "lightmap inherited the app default sampler — that is \
                 `default_nearest()`, which point-samples it"
            ),
            ImageSampler::Descriptor(d) => {
                assert_eq!(d.mag_filter, ImageFilterMode::Linear, "mag filter");
                assert_eq!(d.min_filter, ImageFilterMode::Linear, "min filter");
            }
        }
    }

    #[test]
    fn blur_zero_is_identity_and_blur_preserves_flat() {
        let buf = gradient(4);
        assert_eq!(buf.blur(0).texels, buf.texels);

        let mut flat = LightmapBuffer::new(5);
        flat.texels.fill(Vec3::splat(0.3));
        for t in flat.blur(2).texels {
            assert!((t - Vec3::splat(0.3)).abs().max_element() < 1e-6);
        }
    }

    /// An object atlas puts unrelated triangles in adjacent cells, so a blur
    /// that crosses a cell boundary smears one triangle's lighting onto
    /// another's. Two 2×2 cells, one fully lit and one fully dark: neither
    /// may move.
    #[test]
    fn blur_cells_does_not_leak_across_cell_boundaries() {
        let mut buf = LightmapBuffer::new(4);
        for row in 0..4 {
            for col in 0..4 {
                // Left half is cell column 0, right half cell column 1.
                buf.texels[row * 4 + col] = if col < 2 {
                    Vec3::splat(1.0)
                } else {
                    Vec3::ZERO
                };
            }
        }
        let written = vec![true; 16];
        let out = buf.blur_cells(2, 2, &written);

        for row in 0..4 {
            for col in 0..4 {
                let expected = if col < 2 { 1.0 } else { 0.0 };
                let got = out.texels[row * 4 + col].x;
                assert!(
                    (got - expected).abs() < 1e-6,
                    "texel ({row},{col}) = {got}, expected {expected} — blur crossed a cell edge"
                );
            }
        }
    }

    /// The gutter is still empty when the blur runs (dilation comes after),
    /// so uncovered texels must neither contribute to nor receive an average
    /// — otherwise every triangle picks up a dark rim.
    #[test]
    fn blur_cells_ignores_uncovered_texels() {
        let mut buf = LightmapBuffer::new(4);
        buf.texels.fill(Vec3::splat(1.0));
        let mut written = vec![true; 16];
        // Mark the last column of each 2-wide cell as gutter, and zero it.
        for row in 0..4 {
            for col in [1usize, 3] {
                written[row * 4 + col] = false;
                buf.texels[row * 4 + col] = Vec3::ZERO;
            }
        }

        let out = buf.blur_cells(2, 2, &written);

        for row in 0..4 {
            for col in 0..4 {
                let i = row * 4 + col;
                if written[i] {
                    assert!(
                        (out.texels[i].x - 1.0).abs() < 1e-6,
                        "covered texel ({row},{col}) was darkened to {} by the empty gutter",
                        out.texels[i].x
                    );
                } else {
                    assert_eq!(
                        out.texels[i],
                        Vec3::ZERO,
                        "uncovered texel ({row},{col}) must be left for dilation"
                    );
                }
            }
        }
    }

    /// With one cell covering the whole atlas and no mask, the cell-confined
    /// blur is the plain blur.
    #[test]
    fn blur_cells_matches_plain_blur_for_a_single_cell() {
        let buf = gradient(8);
        let written = vec![true; 64];
        assert_eq!(buf.blur_cells(2, 8, &written).texels, buf.blur(2).texels);
    }

    #[test]
    fn dilate_fills_only_unwritten_neighbours() {
        let mut buf = LightmapBuffer::new(3);
        let mut written = vec![false; 9];
        buf.texels[4] = Vec3::new(1.0, 0.5, 0.0);
        written[4] = true;
        buf.texels[0] = Vec3::splat(9.0); // unwritten garbage that must be overwritten
        buf.dilate(&mut written, 1);
        assert_eq!(buf.texels[4], Vec3::new(1.0, 0.5, 0.0));
        for i in 0..9 {
            assert_eq!(buf.texels[i], Vec3::new(1.0, 0.5, 0.0), "texel {i}");
        }
        assert!(written.iter().all(|w| *w));
    }

    /// One pass reaches one texel; more passes keep spreading, which is what
    /// lets a lit triangle fill the whole gutter around its atlas cell.
    #[test]
    fn dilate_spreads_one_texel_per_pass() {
        let lit = Vec3::new(0.0, 2.0, 0.0);
        let make = || {
            let mut buf = LightmapBuffer::new(7);
            let mut written = vec![false; 49];
            buf.texels[3 * 7 + 3] = lit;
            written[3 * 7 + 3] = true;
            (buf, written)
        };

        let (mut one, mut w1) = make();
        one.dilate(&mut w1, 1);
        assert_eq!(one.texels[3 * 7 + 4], lit, "adjacent filled");
        assert_eq!(one.texels[3 * 7 + 5], Vec3::ZERO, "two away untouched");

        let (mut three, mut w3) = make();
        three.dilate(&mut w3, 3);
        assert_eq!(three.texels[3 * 7 + 6], lit, "three away filled");

        // Dilation stops early once everything is written.
        let (mut many, mut wm) = make();
        many.dilate(&mut wm, 99);
        assert!(many.texels.iter().all(|t| *t == lit));
    }

    /// The GPU terrain bake uploads `BakedLight` verbatim, so its layout must
    /// match the WGSL `BakedLight` exactly. WGSL aligns `vec3` to 16 bytes,
    /// which is why the padding is three scalars rather than a `vec3`.
    #[test]
    fn baked_light_matches_the_wgsl_layout() {
        use std::mem::{align_of, size_of};
        assert_eq!(size_of::<BakedLight>(), 64);
        assert_eq!(align_of::<BakedLight>(), 4);

        let l = BakedLight::point(Vec3::ZERO, Vec3::ONE, 1.0, 1.0);
        let base = &l as *const _ as usize;
        let offset = |p: *const f32| p as usize - base;
        // Every vec3 member must start on a 16-byte boundary.
        assert_eq!(offset(l.position.as_ptr()), 0);
        assert_eq!(offset(&l.inv_range_sq), 12);
        assert_eq!(offset(l.color.as_ptr()), 16);
        assert_eq!(offset(&l.range), 28);
        assert_eq!(offset(l.direction.as_ptr()), 32);
        assert_eq!(offset(&l.spot_scale), 44);
        assert_eq!(offset(&l.spot_offset), 48);
    }

    #[test]
    fn point_light_falls_off_to_nothing_at_its_range() {
        let light = BakedLight::point(Vec3::new(0.0, 5.0, 0.0), Vec3::ONE, 2.0, 10.0);
        let up = Vec3::Y;

        // Directly below and close: bright, pointing straight at the light.
        let (near, dir, dist) = light.sample(Vec3::new(0.0, 4.0, 0.0), up).unwrap();
        assert_eq!(dir, Vec3::Y);
        assert!((dist - 1.0).abs() < 1e-5);
        assert!(near.x > 1.0, "{near}");

        // Further away is dimmer.
        let (far, _, _) = light.sample(Vec3::new(0.0, 0.0, 0.0), up).unwrap();
        assert!(far.x < near.x, "{far} should be dimmer than {near}");

        // Beyond the range, and facing away, contribute nothing at all.
        assert!(light.sample(Vec3::new(0.0, -6.0, 0.0), up).is_none());
        assert!(light.sample(Vec3::new(0.0, 4.0, 0.0), -up).is_none());
    }

    #[test]
    fn spot_light_is_bounded_by_its_cone() {
        // Points straight down from 5 units up, 0.4 rad inner / 0.7 outer.
        let light = BakedLight::spot(
            Vec3::new(0.0, 5.0, 0.0),
            Vec3::NEG_Y,
            Vec3::ONE,
            1.0,
            20.0,
            0.4,
            0.7,
        );
        let up = Vec3::Y;
        let on_axis = light.sample(Vec3::ZERO, up).unwrap().0;

        // Just inside the inner cone is at full cone strength; between inner
        // and outer is attenuated; outside the outer cone is dark.
        let inner_edge = Vec3::new(5.0 * 0.35_f32.tan(), 0.0, 0.0);
        let between = Vec3::new(5.0 * 0.55_f32.tan(), 0.0, 0.0);
        let outside = Vec3::new(5.0 * 0.9_f32.tan(), 0.0, 0.0);

        assert!(light.sample(inner_edge, up).unwrap().0.x > 0.0);
        let mid = light.sample(between, up).map(|s| s.0.x).unwrap_or(0.0);
        assert!(mid > 0.0 && mid < on_axis.x, "mid={mid} on_axis={on_axis}");
        assert!(light.sample(outside, up).is_none());
    }

    /// A point light must be lit in every direction — the cone terms are
    /// chosen so the same branchless spot maths yields exactly 1.0.
    #[test]
    fn point_light_has_no_cone() {
        let light = BakedLight::point(Vec3::ZERO, Vec3::ONE, 1.0, 10.0);
        for dir in [Vec3::X, Vec3::NEG_X, Vec3::Y, Vec3::NEG_Y, Vec3::Z] {
            let probe = dir * 2.0;
            let sample = light.sample(probe, -dir).expect("lit from {dir}");
            let expected = (1.0f32 - 4.0 / 100.0).powi(2);
            assert!(
                (sample.0.x - expected).abs() < 1e-5,
                "dir {dir}: {} vs {expected}",
                sample.0.x
            );
        }
    }

    #[test]
    fn local_lights_sums_and_applies_visibility() {
        let lighting = BakeLighting {
            sun_dir: Vec3::Y,
            sun_rgb: Vec3::ZERO,
            ambient_rgb: Vec3::ZERO,
            lights: vec![
                BakedLight::point(Vec3::new(0.0, 2.0, 0.0), Vec3::X, 1.0, 10.0),
                BakedLight::point(Vec3::new(0.0, 3.0, 0.0), Vec3::Y, 1.0, 10.0),
            ],
        };

        let lit = lighting.local_lights(Vec3::ZERO, Vec3::Y, |_, _| 1.0);
        assert!(lit.x > 0.0 && lit.y > 0.0, "both lights contribute: {lit}");

        // Shadowing every light kills the contribution entirely.
        let shadowed = lighting.local_lights(Vec3::ZERO, Vec3::Y, |_, _| 0.0);
        assert_eq!(shadowed, Vec3::ZERO);

        // The closure only runs for lights that actually reach the surface,
        // so a surface facing away costs no shadow rays.
        let mut calls = 0;
        lighting.local_lights(Vec3::ZERO, Vec3::NEG_Y, |_, _| {
            calls += 1;
            1.0
        });
        assert_eq!(calls, 0);
    }

    #[test]
    fn paths_carry_format_version() {
        let p = terrain_lightmap_path(Path::new("w"), -1, 2);
        assert!(p.ends_with(format!(
            "chunks/chunk_-1_2_lightmap.v{LIGHTMAP_FORMAT_VERSION}.exr"
        )));
        let p = super::super::object_lightmaps::object_lightmap_path(Path::new("w"), 7, 3);
        assert!(p.ends_with(format!(
            "objects/obj_7_3_lightmap.v{LIGHTMAP_FORMAT_VERSION}.exr"
        )));
    }
}
