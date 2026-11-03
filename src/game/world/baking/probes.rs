//! Irradiance probes: baked lighting for things that move.
//!
//! A lightmap texel answers "what light reaches *this surface*". A probe
//! answers "what light would reach a surface with normal `n` at *this point
//! in space*", for every `n` at once, so a character walking through the
//! point can be shaded with the same model as the static world around it —
//! sun, sky, bounce and every placed point/spot light, with the same shadows.
//!
//! Probes sit on a world-aligned lattice ([`PROBE_CELL_SIZE`], 1 m) and are baked
//! per chunk: each chunk's file holds the cells whose centres fall inside its
//! footprint, from a little below its lowest terrain to a fixed height above
//! its highest. The runtime assembles loaded chunks into one toroidal 3D
//! texture (`world::probe_volume`); this module only produces and stores the
//! data.
//!
//! Each probe is L1 spherical harmonics in the *linear* basis, pre-convolved
//! to irradiance in lightmap units, four RGB coefficients, plus one scalar:
//!
//! ```text
//! E(n) = reconstruct(c0, c1..c3, n)                   smooth terms
//!      + sun_rgb · sun_vis · max(n·sun_dir, 0)         direct sun, exact
//! ```
//!
//! The sun is deliberately **not** in the L1 part. Four coefficients cannot
//! hold a bright directional lobe and a dim opposite term at once: a lobe
//! that peaks at `L` in front is `−L/2` behind, and that negative cancels the
//! ground bounce a downward-facing surface should still receive (measured in
//! the tests: the underside of a probe over sunlit ground lost ~90% of its
//! light). So the bake stores only the sun's *visibility* at the probe, and
//! the shader evaluates the clamped cosine against the live sun — a hard
//! terminator, the exact peak, and no bleed into the other terms.
//!
//! Placed lights are written into the L1 part as peak-preserving lobes
//! (`c0 += L/4, c_dir += 3L/4·d`), and the environment — sky, sunlit and
//! shaded terrain, scenery — is fitted by least squares from a fixed set of
//! sphere rays. **The coefficients are not read back linearly.** A linear
//! read of a lamp lobe is `¼L + ¾L cos θ`: negative over the third of the
//! sphere facing away, which the shader's clamp turned into a hard, planar
//! black edge across every object near a lamp (measured in the cafe: a
//! 32x side-to-side ratio and a fifth of the sphere clamped). Instead
//! [`ProbeSh::eval_l1`] recognises how the data was built. Per channel, with
//! `a = |c_dir| / c0`:
//!
//! - `a ≤ ½`: the plain linear read. Broad light (sky, bounce) fits it
//!   exactly and it cannot go negative here.
//! - `a ≥ 1`: a split into an isotropic part and one clamped-cosine lobe —
//!   `¼ + ¾cos` has DC equal to a third of its directional length, so
//!   `iso = c0 − |c|/3` and the lobe has peak `4|c|/3` — drawn as `q³` with
//!   `q = ½(1 + n·d)`, which has the same mean and peak as the clamped
//!   cosine, is never negative, and reaches zero only directly opposite.
//! - between: a smoothstep of the two. Both preserve the mean, so the blend
//!   does.
//!
//! Against ground truth in a lamp-lit room this halves the error (rms 0.42
//! to 0.22, dark side 0.43 to 0.14) and matches the linear read to the
//! third decimal wherever the light is broad; `l1_fit_error_indoors` (an
//! ignored test) is the measurement. The shader's `probe_radiance` mirrors
//! it exactly — the two must agree or tests mean nothing.

use bevy::prelude::*;
use half::f16;
use rayon::prelude::*;
use rkyv::{Archive, Deserialize, Serialize, rancor::Error as RkyvError};
use std::path::{Path, PathBuf};

use super::BakeConfig;
use super::bvh::TriangleBvh;
use super::lightmap::{BakeLighting, LightmapLocation};
use super::raycast::{self, BakeHeightCache};
use crate::game::world::objects::CHUNK_WORLD_SIZE;
use crate::game::world::terrain::storage::StorageError;
pub use engine_api::toon_material::{PROBE_CELL_SIZE, probe_cell_centre, probe_cell_index};

/// Format version in every probe file's name and header. Old files are
/// skipped, never misread.
///
/// Shared `LIGHTMAP_FORMAT_VERSION` through v12, since a lighting-model change
/// invalidates both. It split off when [`PROBE_CELL_SIZE`] halved: that changes
/// what a stored cell index *means* without touching a single lightmap texel,
/// and re-baking every object atlas to retire the probe files would have been
/// the slow part of the bake for nothing. Bump this for lattice or layout
/// changes; a lighting-model change still bumps both.
///
/// - 13: 1 m lattice (was 2 m). Numbered past the lightmap version it left so
///   the two are never confused for each other on disk.
pub const PROBE_FORMAT_VERSION: u32 = 13;

/// Normals the per-probe radiance function is sampled at before projection.
const FIT_NORMALS: usize = 32;
const PROBE_MAGIC: &[u8; 4] = b"TPRB";

// ============================================================================
// Probe data
// ============================================================================

/// `f16`s stored per probe: four RGB coefficients and the sun visibility.
pub const PROBE_CHANNELS: usize = 13;

/// One probe. `c[0]` is the constant term, `c[1..4]` multiply `n.x`, `n.y`,
/// `n.z`; `sun_vis` is how much of the sun the probe sees (0 or 1 today).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ProbeSh {
    pub c: [Vec3; 4],
    pub sun_vis: f32,
}

impl ProbeSh {
    /// The smooth (L1) part of the irradiance for a surface normal, in
    /// lightmap units: sky, bounce and placed lights. See the module doc for
    /// why this is not a linear read of the coefficients; `probe_radiance`
    /// in `toon_lighting.wesl` is the same function.
    pub fn eval_l1(&self, n: Vec3) -> Vec3 {
        let mut out = Vec3::ZERO;
        for ch in 0..3 {
            let dir = Vec3::new(self.c[1][ch], self.c[2][ch], self.c[3][ch]);
            out[ch] = reconstruct_channel(self.c[0][ch], dir, n);
        }
        out
    }

    /// Full irradiance for a surface normal under a sun of `sun_rgb` shining
    /// from `sun_dir`. What the shader computes from the volume texture.
    pub fn eval(&self, n: Vec3, sun_dir: Vec3, sun_rgb: Vec3) -> Vec3 {
        self.eval_l1(n) + sun_rgb * (self.sun_vis * n.dot(sun_dir).max(0.0))
    }

    /// A directional lobe of peak radiance `l` from direction `d` (unit,
    /// pointing *toward* the light). Peak-preserving — see the module doc.
    pub fn lobe(l: Vec3, d: Vec3) -> Self {
        Self {
            c: [
                l * 0.25,
                l * (0.75 * d.x),
                l * (0.75 * d.y),
                l * (0.75 * d.z),
            ],
            sun_vis: 0.0,
        }
    }

    /// Least-squares L1 fit of a radiance function from its values at a set
    /// of sphere normals. Solves the 4×4 normal equations rather than
    /// assuming the normals are perfectly balanced — a Fibonacci set of a
    /// few dozen directions is a couple of percent off on the horizontal
    /// axes, which the direct projection would bake into every probe.
    pub fn fit(samples: &[(Vec3, Vec3)]) -> Self {
        // A = Σ b bᵀ with b = (1, x, y, z); rhs = Σ b f, one per channel.
        let mut a = [[0.0f32; 4]; 4];
        let mut rhs = [Vec3::ZERO; 4];
        for (n, f) in samples {
            let b = [1.0, n.x, n.y, n.z];
            for i in 0..4 {
                for j in 0..4 {
                    a[i][j] += b[i] * b[j];
                }
                rhs[i] += *f * b[i];
            }
        }
        let c = solve_4x4(a, rhs).unwrap_or_default();
        Self { c, sun_vis: 0.0 }
    }

    fn add(&mut self, other: Self) {
        for k in 0..4 {
            self.c[k] += other.c[k];
        }
        self.sun_vis += other.sun_vis;
    }

    pub fn to_f16(self) -> [u16; PROBE_CHANNELS] {
        let mut out = [0u16; PROBE_CHANNELS];
        for k in 0..4 {
            for a in 0..3 {
                out[k * 3 + a] = f16::from_f32(self.c[k][a]).to_bits();
            }
        }
        out[12] = f16::from_f32(self.sun_vis).to_bits();
        out
    }

    pub fn from_f16(bits: &[u16]) -> Self {
        let mut c = [Vec3::ZERO; 4];
        for k in 0..4 {
            for a in 0..3 {
                c[k][a] = f16::from_bits(bits[k * 3 + a]).to_f32();
            }
        }
        Self {
            c,
            sun_vis: f16::from_bits(bits[12]).to_f32(),
        }
    }
}

/// Gaussian elimination with partial pivoting on a symmetric 4×4 system
/// with a `Vec3` right-hand side. `None` if singular (fewer than four
/// independent normals, which the bake never produces).
/// Directionality below which a probe channel is read linearly, and above
/// which it is read as an isotropic part plus one `q³` lobe; smoothstep
/// between. Below `½` the linear read is exact for broad light and cannot be
/// negative; at `1` the linear read reaches zero directly opposite and any
/// higher it would ring negative, so the lobe takes over fully there.
pub const PROBE_LOBE_BLEND: (f32, f32) = (0.5, 1.0);

/// One channel of [`ProbeSh::eval_l1`]. Mirrored by `probe_channel` in
/// `toon_lighting.wesl`.
#[inline]
pub fn reconstruct_channel(c0: f32, dir: Vec3, n: Vec3) -> f32 {
    let c0 = c0.max(0.0);
    let len = dir.length();
    if len <= 1e-6 || c0 <= 0.0 {
        return c0;
    }
    let a = len / c0;
    let linear = (c0 + dir.dot(n)).max(0.0);
    let iso = (c0 - len / 3.0).max(0.0);
    let lobe_peak = (4.0 / 3.0) * len.min(3.0 * c0);
    let q = 0.5 * (1.0 + n.dot(dir) / len);
    let lobe = iso + lobe_peak * q * q * q;
    let (a0, a1) = PROBE_LOBE_BLEND;
    let t = ((a - a0) / (a1 - a0)).clamp(0.0, 1.0);
    let w = t * t * (3.0 - 2.0 * t);
    linear + (lobe - linear) * w
}

/// One axis of the runtime's cubic B-spline reconstruction of the probe
/// volume, folded into two hardware linear taps. Mirrors `bspline_axis` in
/// `toon_lighting.wesl`; see that function for why the volume is not read
/// with plain trilinear filtering.
///
/// Nothing on the CPU reads the probe volume, so this exists to be *tested*:
/// the two-tap pairing is the error-prone half of the shader, and an
/// off-by-half in a tap position silently shifts all dynamic lighting by
/// half a cell. Same arrangement as the other WESL mirrors in this file.
///
/// `u` is a texel coordinate (world position / [`PROBE_CELL_SIZE`]). Returns
/// `(tap0, tap1, w0, w1)` with the taps in texels and the weights summing
/// to 1.
#[allow(dead_code)]
pub fn bspline_axis(u: f32) -> (f32, f32, f32, f32) {
    let x = u - 0.5;
    let i = x.floor();
    let t = x - i;
    let (t2, t3) = (t * t, t * t * t);
    let w0 = (1.0 - 3.0 * t + 3.0 * t2 - t3) / 6.0;
    let w1 = (4.0 - 6.0 * t2 + 3.0 * t3) / 6.0;
    let w2 = (1.0 + 3.0 * t + 3.0 * t2 - 3.0 * t3) / 6.0;
    let w3 = t3 / 6.0;
    let (g0, g1) = (w0 + w1, w2 + w3);
    (i - 1.0 + w1 / g0 + 0.5, i + 1.0 + w3 / g1 + 0.5, g0, g1)
}

/// The four cubic B-spline weights for samples `i-1 ..= i+2`, where `i =
/// floor(u - 0.5)`. The direct form the two-tap pairing has to reproduce.
#[allow(dead_code)]
pub fn bspline_weights(u: f32) -> (i32, [f32; 4]) {
    let x = u - 0.5;
    let i = x.floor();
    let t = x - i;
    let (t2, t3) = (t * t, t * t * t);
    (
        i as i32,
        [
            (1.0 - 3.0 * t + 3.0 * t2 - t3) / 6.0,
            (4.0 - 6.0 * t2 + 3.0 * t3) / 6.0,
            (1.0 + 3.0 * t + 3.0 * t2 - 3.0 * t3) / 6.0,
            t3 / 6.0,
        ],
    )
}

fn solve_4x4(mut a: [[f32; 4]; 4], mut b: [Vec3; 4]) -> Option<[Vec3; 4]> {
    for col in 0..4 {
        let pivot = (col..4).max_by(|&i, &j| a[i][col].abs().total_cmp(&a[j][col].abs()))?;
        if a[pivot][col].abs() < 1e-9 {
            return None;
        }
        a.swap(col, pivot);
        b.swap(col, pivot);
        for row in 0..4 {
            if row == col {
                continue;
            }
            let factor = a[row][col] / a[col][col];
            if factor == 0.0 {
                continue;
            }
            for k in col..4 {
                a[row][k] -= factor * a[col][k];
            }
            b[row] -= b[col] * factor;
        }
    }
    Some(std::array::from_fn(|i| b[i] / a[i][i]))
}

/// Which lattice cells a chunk's probe grid covers on one horizontal axis:
/// those whose centres fall inside `[chunk · CHUNK_WORLD_SIZE, (chunk + 1) ·
/// CHUNK_WORLD_SIZE)`. Returns `(first_cell, count)`. Adjacent chunks'
/// ranges are contiguous and disjoint, so loaded chunks tile the lattice
/// with no gaps or double coverage.
pub fn chunk_cell_range(chunk: i32) -> (i32, u32) {
    let lo = chunk as f32 * CHUNK_WORLD_SIZE;
    let hi = (chunk + 1) as f32 * CHUNK_WORLD_SIZE;
    // Centre (i + 0.5)·c ≥ lo  ⇔  i ≥ lo/c − 0.5
    let first = (lo / PROBE_CELL_SIZE - 0.5).ceil() as i32;
    // Centre < hi  ⇔  i < hi/c − 0.5
    let end = (hi / PROBE_CELL_SIZE - 0.5).ceil() as i32;
    (first, (end - first).max(0) as u32)
}

/// A chunk's baked probe grid: a box of lattice cells and one [`ProbeSh`] per
/// cell as twelve `f16`s. This is the on-disk record (rkyv) as well as the
/// in-memory one.
#[derive(Archive, Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct ChunkProbes {
    pub chunk_x: i32,
    pub chunk_y: i32,
    /// Lattice index of the first cell on each axis.
    pub x0: i32,
    pub y0: i32,
    pub z0: i32,
    /// Cells on each axis.
    pub nx: u32,
    pub ny: u32,
    pub nz: u32,
    /// `PROBE_CHANNELS · nx · ny · nz` f16 bit patterns, probe `(x, y, z)`
    /// at `((y · nz + z) · nx + x) · PROBE_CHANNELS`.
    pub data: Vec<u16>,
}

impl ChunkProbes {
    #[inline]
    fn index(&self, x: u32, y: u32, z: u32) -> usize {
        (((y * self.nz) + z) * self.nx + x) as usize * PROBE_CHANNELS
    }

    /// The probe at lattice cell `(cx, cy, cz)`, if this grid covers it.
    pub fn get(&self, cx: i32, cy: i32, cz: i32) -> Option<ProbeSh> {
        let (x, y, z) = (cx - self.x0, cy - self.y0, cz - self.z0);
        if x < 0
            || y < 0
            || z < 0
            || x >= self.nx as i32
            || y >= self.ny as i32
            || z >= self.nz as i32
        {
            return None;
        }
        let i = self.index(x as u32, y as u32, z as u32);
        Some(ProbeSh::from_f16(&self.data[i..i + PROBE_CHANNELS]))
    }

    /// Like [`Self::get`] but with the vertical index clamped into the
    /// grid, for columns this grid covers: a point above the highest probe
    /// gets the highest, below the lowest gets the lowest. `None` only when
    /// the column itself is outside the grid.
    pub fn get_clamped_y(&self, cx: i32, cy: i32, cz: i32) -> Option<ProbeSh> {
        let y = cy.clamp(self.y0, self.y0 + self.ny as i32 - 1);
        self.get(cx, y, cz)
    }

    /// Inclusive lattice range on Y.
    pub fn y_range(&self) -> (i32, i32) {
        (self.y0, self.y0 + self.ny as i32 - 1)
    }
}

// ============================================================================
// Baking
// ============================================================================

/// Deterministic, evenly spread unit vectors over the sphere (Fibonacci
/// lattice). The same set is used for every probe, so neighbouring probes
/// share any sampling bias and interpolate smoothly rather than flickering.
fn fibonacci_sphere(n: usize) -> Vec<Vec3> {
    let golden = std::f32::consts::PI * (3.0 - 5.0_f32.sqrt());
    (0..n)
        .map(|i| {
            let y = 1.0 - (i as f32 + 0.5) / n as f32 * 2.0;
            let r = (1.0 - y * y).max(0.0).sqrt();
            let a = golden * i as f32;
            Vec3::new(r * a.cos(), y, r * a.sin())
        })
        .collect()
}

/// Everything per-bake and shared by every probe in a chunk.
struct ProbeContext<'a> {
    cache: &'a BakeHeightCache,
    bvh: &'a TriangleBvh,
    lighting: &'a BakeLighting,
    config: &'a BakeConfig,
    env_dirs: Vec<Vec3>,
    normals: Vec<Vec3>,
}

impl ProbeContext<'_> {
    /// Radiance a surface the probe can see sends back, through the shared
    /// [`BakeLighting::reemitted`] — the same definition the lightmap bounce
    /// uses, so a wall is exactly as bright to a passing character as it is
    /// to the ground beneath it.
    ///
    /// `dda` is the origin for the terrain march, lifted off the surface;
    /// `surface` is the point itself, for the BVH.
    fn reemitted_at(&self, dda: Vec3, surface: Vec3, normal: Vec3, albedo: f32) -> Vec3 {
        let vis = raycast::cast_shadow_ray(
            dda,
            surface,
            self.lighting.sun_dir,
            self.cache,
            self.bvh,
            self.config.ao_radius * 4.0,
            self.config.shadow_steps,
        );
        self.lighting
            .reemitted(surface, normal, albedo, vis, |dir, dist| {
                raycast::cast_shadow_ray_segment(
                    dda,
                    surface,
                    dir,
                    dist,
                    self.cache,
                    self.bvh,
                    self.config.light_shadow_steps(dist),
                )
            })
    }

    /// Radiance a terrain hit at `t` along `dir` from `p` sends back.
    fn terrain_radiance(&self, p: Vec3, dir: Vec3, t: f32) -> Vec3 {
        let q = p + dir * t;
        let h = self.cache.sample_height(q.x, q.z);
        let ground = Vec3::new(q.x, h, q.z);
        let (normal, _) = self.cache.sample_normal(q.x, q.z);
        self.reemitted_at(
            ground + Vec3::Y * 0.5,
            ground + normal * 0.02,
            normal,
            self.config.terrain_albedo,
        )
    }

    /// Radiance a scenery hit at `t` along `dir` sends back. The same model
    /// as [`Self::terrain_radiance`], against the triangle's own normal.
    ///
    /// This used to be a flat `ambient x 0.5` for every scenery hit, which
    /// is what made interiors unshaded: inside a building nearly every probe
    /// ray hits the building, so the gathered environment was the *same
    /// constant in every direction*, and an L1 fit of a constant has no
    /// directional term at all. Objects lit from such a probe render the
    /// same whichever way they face — flat, with a seam wherever the
    /// constant drifts across a toon band.
    fn scenery_radiance(&self, p: Vec3, dir: Vec3, t: f32, normal: Vec3) -> Vec3 {
        let q = p + dir * t + normal * 0.02;
        // Lift the terrain march as a terrain hit does: a wall's base sits
        // at ground level, where the bilinear heightmap can read as above it
        // and kill the ray outright.
        self.reemitted_at(q + Vec3::Y * 0.5, q, normal, self.config.scenery_albedo)
    }

    /// Nearest terrain hit along `dir` within `reach`, marching from `p`.
    fn terrain_hit(&self, p: Vec3, dir: Vec3, reach: f32, steps: u32) -> Option<f32> {
        let step = reach / steps.max(1) as f32;
        let climbing = dir.y >= 0.0;
        for s in 1..=steps {
            let t = s as f32 * step;
            let q = p + dir * t;
            if climbing && self.cache.above_terrain(q.y) {
                return None;
            }
            if self.cache.sample_height(q.x, q.z) > q.y {
                return Some(t);
            }
        }
        None
    }

    /// Bake one probe at world position `p` (already known to be in open
    /// air).
    fn shade(&self, p: Vec3) -> ProbeSh {
        let lighting = self.lighting;
        let reach = self.config.ao_radius;

        // Direct sun: one shadow ray. Stored as visibility, evaluated
        // exactly in the shader — see the module doc for why it is not a
        // lobe in the L1 part.
        let mut sh = ProbeSh {
            sun_vis: raycast::cast_shadow_ray(
                p + Vec3::Y * 0.5,
                p,
                lighting.sun_dir,
                self.cache,
                self.bvh,
                self.config.ao_radius * 4.0,
                self.config.shadow_steps,
            ),
            ..Default::default()
        };

        // Placed lights: `sample` with the normal set to the light direction
        // gives the radiance with `ndotl = 1`; the lobe supplies the cosine.
        for light in &lighting.lights {
            let to_light = Vec3::from(light.position) - p;
            let Some(dir) = to_light.try_normalize() else {
                continue;
            };
            let Some((radiance, dir, dist)) = light.sample(p, dir) else {
                continue;
            };
            let vis = raycast::cast_shadow_ray_segment(
                p,
                p,
                dir,
                dist,
                self.cache,
                self.bvh,
                self.config.light_shadow_steps(dist),
            );
            if vis > 0.0 {
                sh.add(ProbeSh::lobe(radiance * vis, dir));
            }
        }

        // Environment: what each sphere ray sees. Terrain is marched first
        // (ordered along the ray), scenery searched inside that distance.
        let env: Vec<(Vec3, Vec3)> = self
            .env_dirs
            .iter()
            .map(|&d| {
                let terrain_t = self.terrain_hit(p, d, reach, self.config.ao_steps);
                let bound = terrain_t.unwrap_or(reach);
                let radiance = match self.bvh.nearest_hit_normal(p, d, bound) {
                    Some((t, n)) => self.scenery_radiance(p, d, t, n),
                    None => match terrain_t {
                        Some(t) => self.terrain_radiance(p, d, t),
                        None => lighting.ambient_rgb,
                    },
                };
                (d, radiance)
            })
            .collect();

        // Irradiance for each fit normal from those rays. Uniform sphere
        // sampling: E(n) = (4π/M) Σ L_j max(n·d_j, 0); dividing by π puts it
        // in lightmap units, so an open sky of radiance A gives exactly A to
        // an upward normal.
        let scale = 4.0 / env.len().max(1) as f32;
        let samples: Vec<(Vec3, Vec3)> = self
            .normals
            .iter()
            .map(|&n| {
                let mut e = Vec3::ZERO;
                for (d, l) in &env {
                    e += *l * n.dot(*d).max(0.0);
                }
                (n, e * scale)
            })
            .collect();
        sh.add(ProbeSh::fit(&samples));
        sh
    }
}

/// Bake every probe of chunk `(chunk_x, chunk_y)`.
///
/// `cache` must cover the chunk plus `config.ao_radius × 4` on every side.
/// Vertical extent is `probe_depth_below` under the chunk's lowest terrain to
/// `probe_height_above` over its highest. A probe below the ground is not
/// baked; it takes the value of the first probe above it in its column so
/// trilinear sampling near the surface never blends in a buried black probe.
pub fn bake_chunk_probes(
    cache: &BakeHeightCache,
    chunk_x: i32,
    chunk_y: i32,
    config: &BakeConfig,
    lighting: &BakeLighting,
    bvh: &TriangleBvh,
) -> ChunkProbes {
    let (x0, nx) = chunk_cell_range(chunk_x);
    let (z0, nz) = chunk_cell_range(chunk_y);

    // Ground height under every column, and the chunk's vertical span.
    let ground: Vec<f32> = (0..nx * nz)
        .map(|i| {
            let (x, z) = (x0 + (i % nx) as i32, z0 + (i / nx) as i32);
            cache.sample_height(probe_cell_centre(x), probe_cell_centre(z))
        })
        .collect();
    let (min_h, max_h) = ground
        .iter()
        .fold((f32::MAX, f32::MIN), |(lo, hi), &h| (lo.min(h), hi.max(h)));
    let y0 = probe_cell_index(min_h - config.probe_depth_below);
    let y1 = probe_cell_index(max_h + config.probe_height_above);
    let ny = (y1 - y0 + 1).max(1) as u32;

    let ctx = ProbeContext {
        cache,
        bvh,
        lighting,
        config,
        env_dirs: fibonacci_sphere(config.probe_rays.max(4) as usize),
        normals: fibonacci_sphere(FIT_NORMALS),
    };

    // One column per task: bake top-down so the fill for buried probes is a
    // running copy of the last open-air one.
    let columns: Vec<Vec<ProbeSh>> = (0..nx * nz)
        .into_par_iter()
        .map(|i| {
            let (x, z) = (x0 + (i % nx) as i32, z0 + (i / nx) as i32);
            let (wx, wz) = (probe_cell_centre(x), probe_cell_centre(z));
            let h = ground[i as usize];
            let mut column = vec![ProbeSh::default(); ny as usize];
            let mut last_open: Option<ProbeSh> = None;
            for yi in (0..ny as usize).rev() {
                let wy = probe_cell_centre(y0 + yi as i32);
                let probe = if wy > h + 0.05 {
                    let sh = ctx.shade(Vec3::new(wx, wy, wz));
                    last_open = Some(sh);
                    sh
                } else {
                    last_open.unwrap_or_default()
                };
                column[yi] = probe;
            }
            column
        })
        .collect();

    let mut data = vec![0u16; (nx * ny * nz) as usize * PROBE_CHANNELS];
    for (i, column) in columns.iter().enumerate() {
        let (x, z) = ((i as u32) % nx, (i as u32) / nx);
        for (y, sh) in column.iter().enumerate() {
            let at = (((y as u32 * nz) + z) * nx + x) as usize * PROBE_CHANNELS;
            data[at..at + PROBE_CHANNELS].copy_from_slice(&sh.to_f16());
        }
    }

    ChunkProbes {
        chunk_x,
        chunk_y,
        x0,
        y0,
        z0,
        nx,
        ny,
        nz,
        data,
    }
}

// ============================================================================
// Storage
// ============================================================================

fn probes_file_name(cx: i32, cy: i32) -> String {
    format!("chunk_{cx}_{cy}_probes.v{PROBE_FORMAT_VERSION}.bin")
}

/// Path of a chunk's probe file inside a world directory. Versioned by
/// [`PROBE_FORMAT_VERSION`], in the file name like the lightmaps.
pub fn chunk_probes_path(world_path: &Path, cx: i32, cy: i32) -> PathBuf {
    world_path.join("chunks").join(probes_file_name(cx, cy))
}

/// OPFS key of a chunk's probe file (see `terrain_lightmap_key`).
#[allow(dead_code)]
pub fn chunk_probes_key(world_id: &str, cx: i32, cy: i32) -> String {
    format!("worlds/{world_id}/chunks/{}", probes_file_name(cx, cy))
}

/// Storage location for this platform.
pub fn chunk_probes_location(
    world: &crate::game::world::terrain::storage::TerrainWorld,
    cx: i32,
    cy: i32,
) -> LightmapLocation {
    #[cfg(not(target_arch = "wasm32"))]
    {
        chunk_probes_path(&world.world_path, cx, cy)
    }
    #[cfg(target_arch = "wasm32")]
    {
        chunk_probes_key(&world.metadata.world_id, cx, cy)
    }
}

/// Frame: magic, format version, rkyv payload length, payload, crc32.
pub fn serialize_probes(probes: &ChunkProbes) -> Result<Vec<u8>, StorageError> {
    let payload = rkyv::to_bytes::<RkyvError>(probes)
        .map_err(|e| StorageError::SerializationError(e.to_string()))?;
    let mut out = Vec::with_capacity(16 + payload.len());
    out.extend_from_slice(PROBE_MAGIC);
    out.extend_from_slice(&PROBE_FORMAT_VERSION.to_le_bytes());
    out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    out.extend_from_slice(&payload);
    out.extend_from_slice(&crc32fast::hash(&payload).to_le_bytes());
    Ok(out)
}

pub fn parse_probes(bytes: &[u8]) -> Result<ChunkProbes, StorageError> {
    let u32_at = |at: usize| -> Result<u32, StorageError> {
        bytes
            .get(at..at + 4)
            .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
            .ok_or(StorageError::InvalidMagic)
    };
    if bytes.get(0..4) != Some(PROBE_MAGIC.as_slice()) {
        return Err(StorageError::InvalidMagic);
    }
    let version = u32_at(4)?;
    if version != PROBE_FORMAT_VERSION {
        return Err(StorageError::VersionMismatch {
            expected: PROBE_FORMAT_VERSION,
            found: version,
        });
    }
    let len = u32_at(8)? as usize;
    let payload = bytes.get(12..12 + len).ok_or(StorageError::InvalidMagic)?;
    if u32_at(12 + len)? != crc32fast::hash(payload) {
        return Err(StorageError::ChecksumMismatch);
    }
    // rkyv wants its payload aligned; the frame header is 12 bytes, so copy.
    let mut aligned = rkyv::util::AlignedVec::<16>::with_capacity(payload.len());
    aligned.extend_from_slice(payload);
    let archived = rkyv::access::<ArchivedChunkProbes, RkyvError>(&aligned)
        .map_err(|e| StorageError::SerializationError(e.to_string()))?;
    let probes = rkyv::deserialize::<ChunkProbes, RkyvError>(archived)
        .map_err(|e| StorageError::SerializationError(e.to_string()))?;
    if probes.data.len() != (probes.nx * probes.ny * probes.nz) as usize * PROBE_CHANNELS {
        return Err(StorageError::SerializationError(
            "probe grid data length does not match its dimensions".into(),
        ));
    }
    Ok(probes)
}

/// Persist a chunk's probes — same platform bargain as `store_lightmap`:
/// synchronous natively, fire-and-forget OPFS on wasm.
pub fn store_probes(location: &LightmapLocation, probes: &ChunkProbes) -> Result<(), StorageError> {
    let bytes = serialize_probes(probes)?;
    #[cfg(not(target_arch = "wasm32"))]
    {
        if let Some(dir) = location.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(location, bytes)?;
    }
    #[cfg(target_arch = "wasm32")]
    {
        let key = location.clone();
        wasm_bindgen_futures::spawn_local(async move {
            if let Err(e) = crate::storage::wasm::WasmStorage.save(&key, &bytes).await {
                bevy::log::error!("OPFS probe save failed for {key}: {e}");
            }
        });
    }
    Ok(())
}

/// What a loader needs: a path natively, already-fetched bytes on wasm
/// (mirrors `LightmapSource`).
pub type ProbeSource = super::lightmap::LightmapSource;

/// Decode a saved probe grid, or `None` if there isn't one / it is
/// unreadable (logged). Safe off the main thread.
pub fn try_load_probes(src: &ProbeSource) -> Option<ChunkProbes> {
    #[cfg(not(target_arch = "wasm32"))]
    let decoded = {
        if !src.exists() {
            return None;
        }
        std::fs::read(src)
            .map_err(StorageError::from)
            .and_then(|b| parse_probes(&b))
    };
    #[cfg(target_arch = "wasm32")]
    let decoded = parse_probes(src);

    match decoded {
        Ok(p) => Some(p),
        Err(e) => {
            warn!("Ignoring unreadable probe grid: {e}");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::world::terrain::{HeightmapData, storage::EnvironmentData};

    pub(super) fn flat_world() -> HeightmapData {
        let mut hm = HeightmapData::new((0.25, 0.25));
        for tx in -400..800 {
            for tz in -400..800 {
                hm.set_tile(tx, tz, 0.0);
            }
        }
        hm
    }

    /// Scenery must re-emit according to its *own* normal and sun
    /// visibility, not a flat constant.
    ///
    /// Every scenery hit used to return `ambient x 0.5` whatever the
    /// direction, the surface's orientation or whether the sun reached it.
    /// Indoors nearly every probe ray hits the building, so the gathered
    /// environment was the same constant in every direction — and an L1 fit
    /// of a constant has no directional term at all, which is why objects
    /// lit from an indoor probe shaded identically whichever way they faced.
    ///
    /// Note what this does *not* fix: a sealed room the sun never enters
    /// still gathers `ambient x albedo` from every direction, because
    /// re-emission carries sun and sky but not placed lights. Interiors need
    /// either daylight through an opening or the lamp-bounce that is still
    /// missing (see docs/LIGHTING.md 7.10).
    #[test]
    fn scenery_re_emits_by_its_own_normal_not_a_constant() {
        let hm = flat_world();
        let cache = BakeHeightCache::extract(&hm, -60.0, -60.0, 60.0, 60.0);
        let config = quick_config();
        let bvh = TriangleBvh::build(Vec::new());
        let lighting = BakeLighting {
            sun_dir: Vec3::Y,
            sun_rgb: Vec3::splat(3.0),
            ambient_rgb: Vec3::splat(0.3),
            lights: Vec::new(),
        };
        let ctx = ProbeContext {
            cache: &cache,
            bvh: &bvh,
            lighting: &lighting,
            config: &config,
            env_dirs: fibonacci_sphere(config.probe_rays as usize),
            normals: fibonacci_sphere(FIT_NORMALS),
        };

        let p = Vec3::new(0.0, 5.0, 0.0);
        // The same hit, seen as an upward face (the sun reaches it) and as a
        // downward one (it does not). A constant cannot tell them apart.
        let facing_sun = ctx.scenery_radiance(p, Vec3::X, 2.0, Vec3::Y);
        let facing_away = ctx.scenery_radiance(p, Vec3::X, 2.0, Vec3::NEG_Y);
        assert!(
            facing_sun.length() > facing_away.length() * 2.0,
            "a sunlit face must re-emit more than a shaded one: \
             {facing_sun:?} vs {facing_away:?}"
        );
        // The shaded face still carries sky, scaled by the albedo.
        let expected = lighting.ambient_rgb * config.scenery_albedo;
        assert!((facing_away - expected).length() < 1e-5);
    }

    /// The runtime reads the probe volume with a cubic B-spline folded into
    /// two hardware linear taps per axis. That pairing is the error-prone
    /// part — an off-by-half in the tap position is a silent half-cell shift
    /// of all dynamic lighting — so it is checked against the direct 4-tap
    /// weights on arbitrary data.
    #[test]
    fn bspline_two_tap_form_is_exact() {
        // An arbitrary, deliberately not-smooth 1-D sample set.
        let samples: Vec<f32> = (0..16)
            .map(|i| ((i * 37 % 11) as f32 * 0.31).sin() * 2.0 + i as f32 * 0.05)
            .collect();
        let fetch = |j: i32| samples[j.rem_euclid(samples.len() as i32) as usize];
        // What the hardware does for a linear tap at texel coordinate `c`.
        let linear_tap = |c: f32| {
            let x = c - 0.5;
            let i = x.floor();
            let f = x - i;
            fetch(i as i32) * (1.0 - f) + fetch(i as i32 + 1) * f
        };

        for k in 0..200 {
            let u = 3.0 + k as f32 * 0.037;
            let (i, w) = bspline_weights(u);
            let direct: f32 = (0..4).map(|j| w[j] * fetch(i - 1 + j as i32)).sum();
            let (tap0, tap1, g0, g1) = bspline_axis(u);
            let paired = g0 * linear_tap(tap0) + g1 * linear_tap(tap1);
            assert!(
                (direct - paired).abs() < 1e-5,
                "two-tap form differs at u={u}: {paired} vs {direct}"
            );
            assert!(
                (g0 + g1 - 1.0).abs() < 1e-6,
                "weights must sum to 1 at u={u}"
            );
        }
    }

    /// A probe sits at the centre of its cell, and the reconstruction must
    /// not shift the field off the lattice: with every sample equal, any
    /// interpolation of them is that value, and a ramp must come back as a
    /// ramp's mean rather than displaced.
    #[test]
    fn bspline_preserves_a_constant_and_does_not_shift_a_ramp() {
        for k in 0..100 {
            let u = 5.0 + k as f32 * 0.07;
            let (_, w) = bspline_weights(u);
            assert!((w.iter().sum::<f32>() - 1.0).abs() < 1e-6);
            // Centre of mass of the kernel sits exactly on `u`, so a linear
            // field is reproduced without a phase shift.
            let (i, w) = bspline_weights(u);
            let centre: f32 = (0..4)
                .map(|j| w[j] * (i - 1 + j as i32) as f32)
                .sum::<f32>()
                + 0.5;
            assert!(
                (centre - u).abs() < 1e-4,
                "kernel is off-centre at u={u}: {centre}"
            );
        }
    }

    fn quick_config() -> BakeConfig {
        BakeConfig {
            probe_rays: 64,
            ao_radius: 10.0,
            ao_steps: 16,
            shadow_steps: 16,
            probe_height_above: 4.0,
            probe_depth_below: 2.0,
            ..Default::default()
        }
    }

    #[test]
    fn chunk_cell_ranges_tile_the_lattice() {
        for chunk in -3..3 {
            let (a0, an) = chunk_cell_range(chunk);
            let (b0, _) = chunk_cell_range(chunk + 1);
            assert_eq!(
                a0 + an as i32,
                b0,
                "chunks {chunk} and {} must abut",
                chunk + 1
            );
            // Every covered centre is inside the chunk.
            for i in a0..a0 + an as i32 {
                let c = probe_cell_centre(i);
                assert!(
                    c >= chunk as f32 * CHUNK_WORLD_SIZE
                        && c < (chunk + 1) as f32 * CHUNK_WORLD_SIZE
                );
            }
        }
        // 63.75 m of chunk holds that many cells, give or take the one on
        // the boundary.
        let (_, n) = chunk_cell_range(0);
        let expect = CHUNK_WORLD_SIZE / PROBE_CELL_SIZE;
        assert!(
            (n as f32 - expect).abs() <= 1.0,
            "{n} cells for a {CHUNK_WORLD_SIZE} m chunk at {PROBE_CELL_SIZE} m"
        );
    }

    /// A pure lamp lobe reads back with the peak and mean of the clamped
    /// cosine it stands for, never negative, and zero only directly behind.
    #[test]
    fn lobe_peaks_at_its_direction_and_goes_dark_behind() {
        let l = Vec3::new(1.0, 0.8, 0.6);
        let d = Vec3::new(0.3, 0.9, 0.1).normalize();
        let sh = ProbeSh::lobe(l, d);
        assert!(
            (sh.eval_l1(d) - l).abs().max_element() < 1e-5,
            "peak {}",
            sh.eval_l1(d)
        );
        assert!(
            sh.eval_l1(-d).max_element() < 1e-6,
            "behind {}",
            sh.eval_l1(-d)
        );
        // Cubic in between: an eighth of the peak side-on, where the old
        // linear read gave a quarter and went negative a third of the way
        // round the back.
        let side = d.any_orthonormal_vector();
        assert!((sh.eval_l1(side) - l * 0.125).abs().max_element() < 1e-5);
        // Same energy as the clamped cosine: a quarter of the peak on average.
        let dirs = fibonacci_sphere(2048);
        let mean = dirs.iter().map(|&n| sh.eval_l1(n)).sum::<Vec3>() / dirs.len() as f32;
        assert!(
            (mean - l * 0.25).abs().max_element() < 0.01,
            "mean {mean} should be {}",
            l * 0.25
        );
        // And never negative anywhere.
        assert!(dirs.iter().all(|&n| sh.eval_l1(n).min_element() >= 0.0));
    }

    /// A function that is already linear in `n` must be recovered exactly.
    #[test]
    fn fit_reproduces_a_linear_function() {
        let truth = ProbeSh {
            c: [
                Vec3::splat(0.5),
                Vec3::new(0.1, 0.0, 0.0),
                Vec3::new(0.0, 0.3, 0.0),
                Vec3::ZERO,
            ],
            sun_vis: 0.0,
        };
        // A deliberately lopsided normal set: least squares must still get
        // the exact answer, where the balanced-set projection would not.
        let normals: Vec<Vec3> = fibonacci_sphere(40).into_iter().take(23).collect();
        let samples: Vec<(Vec3, Vec3)> = normals
            .iter()
            .map(|&n| {
                (
                    n,
                    truth.c[0] + truth.c[1] * n.x + truth.c[2] * n.y + truth.c[3] * n.z,
                )
            })
            .collect();
        let fit = ProbeSh::fit(&samples);
        for k in 0..4 {
            assert!(
                (fit.c[k] - truth.c[k]).abs().max_element() < 1e-4,
                "c{k}: {} vs {}",
                fit.c[k],
                truth.c[k]
            );
        }
    }

    /// Open flat ground, no scenery: a probe in the air must see exactly the
    /// lightmap's own answer for an upward surface — sun × ndotl + ambient —
    /// and a buried probe must be replaced by the one above it.
    #[test]
    fn open_air_probe_matches_the_lightmap_model() {
        let hm = flat_world();
        let config = quick_config();
        let reach = config.ao_radius * 4.0;
        let cache = BakeHeightCache::extract(
            &hm,
            -reach,
            -reach,
            CHUNK_WORLD_SIZE + reach,
            CHUNK_WORLD_SIZE + reach,
        );
        let sun_dir = Vec3::new(0.2, 0.9, 0.3).normalize();
        let env = EnvironmentData::default();
        let lighting = BakeLighting::new(&env, sun_dir, Vec::new());
        let bvh = TriangleBvh::build(Vec::new());

        let probes = bake_chunk_probes(&cache, 0, 0, &config, &lighting, &bvh);
        let (x0, nx) = chunk_cell_range(0);
        assert_eq!((probes.x0, probes.nx), (x0, nx));
        assert_eq!(probes.y0, probe_cell_index(-2.0));
        assert_eq!(probes.y0 + probes.ny as i32 - 1, probe_cell_index(4.0));

        let mid = probes
            .get(x0 + 5, probe_cell_index(3.0), probes.z0 + 5)
            .unwrap();
        assert_eq!(mid.sun_vis, 1.0, "open air sees the sun");
        let eval = |n: Vec3| mid.eval(n, sun_dir, lighting.sun_rgb);
        let expected_up = lighting.sun_rgb * sun_dir.y + lighting.ambient_rgb;
        let got_up = eval(Vec3::Y);
        assert!(
            (got_up - expected_up).abs().max_element() < 0.08,
            "up: {got_up} vs {expected_up}"
        );
        // Facing the ground: albedo × (sunlit ground + sky), no direct sun —
        // and crucially not cancelled by the sun, which is why the sun is
        // kept out of the L1 coefficients.
        let got_down = eval(Vec3::NEG_Y);
        let ground = (lighting.sun_rgb * sun_dir.y + lighting.ambient_rgb) * config.terrain_albedo;
        assert!(got_down.x > 0.0 && got_down.x < got_up.x, "down {got_down}");
        assert!(
            (got_down - ground).abs().max_element() < 0.15,
            "down: {got_down} vs {ground}"
        );
        // Facing the sun exactly gets the full sun.
        let facing = eval(sun_dir);
        assert!(facing.x > lighting.sun_rgb.x, "facing the sun: {facing}");

        // Below ground: a copy of the first open-air probe above. On flat
        // ground at height 0 that is cell 0, whose centre is half a cell up.
        let buried = probes
            .get(
                x0 + 5,
                probe_cell_index(-0.5 * PROBE_CELL_SIZE),
                probes.z0 + 5,
            )
            .unwrap();
        let lowest_open = probes.get(x0 + 5, 0, probes.z0 + 5).unwrap();
        assert_eq!(buried, lowest_open);
    }

    #[test]
    fn a_point_light_lights_the_side_facing_it() {
        let hm = flat_world();
        let config = quick_config();
        let reach = config.ao_radius * 4.0;
        let cache = BakeHeightCache::extract(
            &hm,
            -reach,
            -reach,
            CHUNK_WORLD_SIZE + reach,
            CHUNK_WORLD_SIZE + reach,
        );
        let sun_dir = Vec3::new(0.0, 1.0, 0.0);
        let env = EnvironmentData {
            sun_intensity: 0.0,
            ..Default::default()
        };
        let bvh = TriangleBvh::build(Vec::new());

        let dark = BakeLighting::new(&env, sun_dir, Vec::new());
        let probe_pos = Vec3::new(
            probe_cell_centre(5),
            probe_cell_centre(1),
            probe_cell_centre(5),
        );
        let light_pos = probe_pos + Vec3::X * 3.0;
        let light = super::super::lightmap::BakedLight::point(
            light_pos,
            Vec3::new(1.0, 0.5, 0.2),
            2.0,
            10.0,
        );
        let lit = BakeLighting::new(&env, sun_dir, vec![light]);

        let without = bake_chunk_probes(&cache, 0, 0, &config, &dark, &bvh)
            .get(5, 1, 5)
            .unwrap();
        let with = bake_chunk_probes(&cache, 0, 0, &config, &lit, &bvh)
            .get(5, 1, 5)
            .unwrap();

        let toward = with.eval_l1(Vec3::X) - without.eval_l1(Vec3::X);
        let away = with.eval_l1(Vec3::NEG_X) - without.eval_l1(Vec3::NEG_X);
        assert!(toward.x > 0.3, "facing the light gained {toward}");
        assert!(
            toward.x > toward.y && toward.y > toward.z,
            "and in the light's colour: {toward}"
        );

        // The side facing away. Two things can reach it: the lamp's light
        // bounced off the ground (a real gain, since v11), and the lobe's
        // negative tail (an artefact the reconstruction exists to remove).
        // Bake once more with no bounce to separate them.
        let no_bounce = BakeConfig {
            terrain_albedo: 0.0,
            scenery_albedo: 0.0,
            ..config.clone()
        };
        let without_nb = bake_chunk_probes(&cache, 0, 0, &no_bounce, &dark, &bvh)
            .get(5, 1, 5)
            .unwrap();
        let with_nb = bake_chunk_probes(&cache, 0, 0, &no_bounce, &lit, &bvh)
            .get(5, 1, 5)
            .unwrap();
        let away_nb = with_nb.eval_l1(Vec3::NEG_X) - without_nb.eval_l1(Vec3::NEG_X);
        println!("far side: with bounce {away}, without bounce {away_nb}");
        assert!(
            away_nb.abs().max_element() < 0.05,
            "with no bounce the far side must be untouched by the lamp, changed by {away_nb}"
        );
        assert!(
            away.min_element() > -0.05,
            "with bounce the far side may gain from the lit ground but never darken: {away}"
        );
        // Sun visibility is untouched by placed lights.
        assert_eq!(with.sun_vis, without.sun_vis);
    }

    #[test]
    fn probe_file_round_trips_and_rejects_garbage() {
        let probes = ChunkProbes {
            chunk_x: -1,
            chunk_y: 2,
            x0: -32,
            y0: -1,
            z0: 64,
            nx: 2,
            ny: 3,
            nz: 2,
            data: (0..2 * 3 * 2 * PROBE_CHANNELS)
                .map(|i| f16::from_f32(i as f32 * 0.1).to_bits())
                .collect(),
        };
        let bytes = serialize_probes(&probes).unwrap();
        assert_eq!(parse_probes(&bytes).unwrap(), probes);

        assert!(matches!(
            parse_probes(b"nope"),
            Err(StorageError::InvalidMagic)
        ));
        let mut bad = bytes.clone();
        let last = bad.len() - 1;
        bad[last] ^= 0xff;
        assert!(matches!(
            parse_probes(&bad),
            Err(StorageError::ChecksumMismatch)
        ));
        let mut old = bytes.clone();
        old[4..8].copy_from_slice(&(PROBE_FORMAT_VERSION - 1).to_le_bytes());
        assert!(matches!(
            parse_probes(&old),
            Err(StorageError::VersionMismatch { .. })
        ));

        // Clamped access.
        assert_eq!(probes.get_clamped_y(-32, 99, 64), probes.get(-32, 1, 64));
        assert_eq!(probes.get_clamped_y(-32, -99, 64), probes.get(-32, -1, 64));
        assert_eq!(probes.get_clamped_y(0, 0, 64), None);
        assert!(chunk_probes_path(Path::new("w"), 3, -4).ends_with(format!(
            "chunks/chunk_3_-4_probes.v{PROBE_FORMAT_VERSION}.bin"
        )));
    }
}

#[cfg(test)]
mod diag {
    use super::*;
    use crate::game::world::baking::lightmap::BakedLight;

    /// A room lit through its doorway: four walls, a ceiling, a 3 m gap in
    /// the +Z wall, and a low sun shining in through it.
    fn doorway_room() -> TriangleBvh {
        let (hx, hz, h) = (10.0f32, 6.0f32, 3.0f32);
        let mut tris: Vec<[Vec3; 3]> = Vec::new();
        let mut quad = |a: Vec3, b: Vec3, c: Vec3, d: Vec3| {
            tris.push([a, b, c]);
            tris.push([a, c, d]);
        };
        quad(
            Vec3::new(-hx, h, -hz),
            Vec3::new(hx, h, -hz),
            Vec3::new(hx, h, hz),
            Vec3::new(-hx, h, hz),
        );
        for (a, b) in [
            (Vec3::new(-hx, 0.0, -hz), Vec3::new(hx, 0.0, -hz)),
            (Vec3::new(-hx, 0.0, -hz), Vec3::new(-hx, 0.0, hz)),
            (Vec3::new(hx, 0.0, -hz), Vec3::new(hx, 0.0, hz)),
            (Vec3::new(1.5, 0.0, hz), Vec3::new(hx, 0.0, hz)),
            (Vec3::new(-hx, 0.0, hz), Vec3::new(-1.5, 0.0, hz)),
        ] {
            quad(a, b, b + Vec3::Y * h, a + Vec3::Y * h);
        }
        TriangleBvh::build(tris)
    }

    /// How well the stored L1 reproduces the true irradiance an object
    /// would receive at an indoor probe, with and without limiting the
    /// directional term. `cargo test l1_fit_error_indoors -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn l1_fit_error_indoors() {
        let hm = super::tests::flat_world();
        let cache = BakeHeightCache::extract(&hm, -60.0, -60.0, 60.0, 60.0);
        let bvh = doorway_room();
        let config = BakeConfig {
            probe_rays: 256,
            ao_steps: 32,
            shadow_steps: 48,
            ..Default::default()
        };
        // LAMP=intensity,range puts a point light a metre off the -X wall.
        let lamp: Option<(f32, f32)> =
            std::env::var("LAMP")
                .ok()
                .filter(|v| !v.is_empty())
                .map(|v| {
                    let p: Vec<f32> = v.split(',').map(|n| n.parse().unwrap()).collect();
                    (p[0], p[1])
                });
        let lighting = BakeLighting {
            sun_dir: Vec3::new(0.2, 0.35, 1.0).normalize(),
            sun_rgb: Vec3::splat(3.0),
            ambient_rgb: Vec3::splat(0.4),
            lights: lamp
                .map(|(i, r)| {
                    vec![BakedLight::point(
                        Vec3::new(-9.0, 2.0, 0.0),
                        Vec3::ONE,
                        i,
                        r,
                    )]
                })
                .unwrap_or_default(),
        };
        let ctx = ProbeContext {
            cache: &cache,
            bvh: &bvh,
            lighting: &lighting,
            config: &config,
            env_dirs: fibonacci_sphere(256),
            normals: fibonacci_sphere(FIT_NORMALS),
        };
        let lum = |v: Vec3| 0.2126 * v.x + 0.7152 * v.y + 0.0722 * v.z;
        let eval_normals = fibonacci_sphere(512);

        println!(
            " probe          |  k   | rms err | max err | lit-side err | dark-side err | clamped  (true E range)"
        );
        for p in [
            Vec3::new(-6.0, 1.0, 0.0),
            Vec3::new(-3.0, 1.0, 0.0),
            Vec3::new(0.0, 1.0, 2.0),
        ] {
            // Ground truth: the gathered environment integrated exactly for every eval normal.
            let env: Vec<(Vec3, Vec3)> = ctx
                .env_dirs
                .iter()
                .map(|&d| {
                    let terrain_t = ctx.terrain_hit(p, d, config.ao_radius, config.ao_steps);
                    let bound = terrain_t.unwrap_or(config.ao_radius);
                    let r = match ctx.bvh.nearest_hit_normal(p, d, bound) {
                        Some((t, n)) => ctx.scenery_radiance(p, d, t, n),
                        None => match terrain_t {
                            Some(t) => ctx.terrain_radiance(p, d, t),
                            None => lighting.ambient_rgb,
                        },
                    };
                    (d, r)
                })
                .collect();
            let scale = 4.0 / env.len() as f32;
            // Truth = environment integral + every lamp's exact clamped cosine.
            let truth: Vec<f32> = eval_normals
                .iter()
                .map(|&n| {
                    let mut e = Vec3::ZERO;
                    for (d, l) in &env {
                        e += *l * n.dot(*d).max(0.0);
                    }
                    e *= scale;
                    for light in &lighting.lights {
                        if let Some((rad, dir, dist)) = light.sample(p, n) {
                            let vis = raycast::cast_shadow_ray_segment(
                                p,
                                p,
                                dir,
                                dist,
                                &cache,
                                &bvh,
                                config.light_shadow_steps(dist),
                            );
                            e += rad * vis;
                        }
                    }
                    lum(e)
                })
                .collect();
            let (tmin, tmax) = truth
                .iter()
                .fold((f32::MAX, 0.0f32), |(lo, hi), &v| (lo.min(v), hi.max(v)));
            let brightest = eval_normals[truth
                .iter()
                .enumerate()
                .max_by(|a, b| a.1.total_cmp(b.1))
                .unwrap()
                .0];

            let sh = ctx.shade(p);
            let a_lum = Vec3::new(lum(sh.c[1]), lum(sh.c[2]), lum(sh.c[3])).length()
                / lum(sh.c[0]).max(1e-6);
            println!("   a = |c_dir|/c0 = {a_lum:.2}");
            let linear = |t: &ProbeSh, n: Vec3| {
                (t.c[0] + t.c[1] * n.x + t.c[2] * n.y + t.c[3] * n.z).max(Vec3::ZERO)
            };
            for (label, read) in [("linear", true), ("eval_l1", false)] {
                let mut se = 0.0;
                let mut mx: f32 = 0.0;
                let mut lit = 0.0;
                let mut dark = 0.0;
                let (mut nl, mut nd) = (0, 0);
                let mut clamped = 0;
                for (i, n) in eval_normals.iter().enumerate() {
                    let raw = sh.c[0] + sh.c[1] * n.x + sh.c[2] * n.y + sh.c[3] * n.z;
                    if read && lum(raw) < 0.0 {
                        clamped += 1;
                    }
                    let got = if read {
                        linear(&sh, *n)
                    } else {
                        sh.eval_l1(*n)
                    };
                    let e = (lum(got) - truth[i]).abs();
                    se += e * e;
                    mx = mx.max(e);
                    if n.dot(brightest) > 0.5 {
                        lit += e;
                        nl += 1;
                    } else if n.dot(brightest) < -0.5 {
                        dark += e;
                        nd += 1;
                    }
                }
                println!(
                    "({:5.1},{:3.1},{:5.1}) | {label:>8} | {:.4}  | {:.4}  |   {:.4}     |   {:.4}      |  {:3.0}%    ({tmin:.3}..{tmax:.3})",
                    p.x,
                    p.y,
                    p.z,
                    (se / eval_normals.len() as f32).sqrt(),
                    mx,
                    lit / nl.max(1) as f32,
                    dark / nd.max(1) as f32,
                    100.0 * clamped as f32 / eval_normals.len() as f32
                );
            }
        }
    }

    /// A probe next to a lamp must not read as black on the side facing
    /// away, and a probe under broad light must still read linearly.
    ///
    /// The linear read of a lamp lobe (`¼ + ¾cos`) is negative over a third
    /// of the sphere; clamped, that drew a hard planar edge across every
    /// object near a lamp.
    #[test]
    fn a_lamp_lit_probe_is_not_black_behind_and_a_sky_lit_one_stays_linear() {
        let hm = super::tests::flat_world();
        let cache = BakeHeightCache::extract(&hm, -60.0, -60.0, 60.0, 60.0);
        let bvh = doorway_room();
        let config = BakeConfig {
            probe_rays: 128,
            ao_steps: 16,
            shadow_steps: 32,
            ..Default::default()
        };
        let lighting = BakeLighting {
            sun_dir: Vec3::new(0.2, 0.35, 1.0).normalize(),
            sun_rgb: Vec3::splat(3.0),
            ambient_rgb: Vec3::splat(0.4),
            lights: vec![BakedLight::point(
                Vec3::new(-9.0, 2.0, 0.0),
                Vec3::ONE,
                4.0,
                8.0,
            )],
        };
        let ctx = ProbeContext {
            cache: &cache,
            bvh: &bvh,
            lighting: &lighting,
            config: &config,
            env_dirs: fibonacci_sphere(128),
            normals: fibonacci_sphere(FIT_NORMALS),
        };
        let lum = |v: Vec3| 0.2126 * v.x + 0.7152 * v.y + 0.0722 * v.z;
        let linear = |t: &ProbeSh, n: Vec3| {
            (t.c[0] + t.c[1] * n.x + t.c[2] * n.y + t.c[3] * n.z).max(Vec3::ZERO)
        };

        // Near the lamp (which is at -X): the side facing away from it.
        let near = ctx.shade(Vec3::new(-6.0, 1.0, 0.0));
        let away = Vec3::X;
        assert!(
            lum(linear(&near, away)) < 0.02,
            "precondition: the linear read is what used to be black here, got {:?}",
            linear(&near, away)
        );
        // What the walls send back is at least ambient x albedo, spread over
        // the half of the sphere they fill.
        let fill_floor = lum(lighting.ambient_rgb) * config.scenery_albedo * 0.5;
        assert!(
            lum(near.eval_l1(away)) > fill_floor * 0.5,
            "the side facing away from the lamp must keep its fill light: {:?} (floor {fill_floor:.3})",
            near.eval_l1(away)
        );
        assert!(
            (lum(near.eval_l1(-away)) - lum(linear(&near, -away))).abs() < 0.5,
            "the lit side should not move far: {:?} vs linear {:?}",
            near.eval_l1(-away),
            linear(&near, -away)
        );

        // Far from the lamp, under broad light: identical to the linear read.
        let far = ctx.shade(Vec3::new(6.0, 1.0, 4.0));
        for n in fibonacci_sphere(64) {
            let (a, b) = (far.eval_l1(n), linear(&far, n));
            assert!(
                (a - b).length() < 1e-5,
                "broad light must read linearly: {a:?} vs {b:?}"
            );
        }
    }

    /// Read a baked probe grid and report, per column at a given height,
    /// how the L1 part behaves on an object: how directional it is and how
    /// much of the sphere the `max(0, …)` clamp cuts off. Diagnostic only:
    /// `PROBES=path [Y=metres] cargo test dump_probes -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn dump_probes() {
        let path = std::env::var("PROBES").expect("set PROBES=<chunk_X_Y_probes.vN.bin>");
        let y_m: f32 = std::env::var("Y")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(1.0);
        let grid = parse_probes(&std::fs::read(&path).unwrap()).unwrap();
        let lum = |v: Vec3| 0.2126 * v.x + 0.7152 * v.y + 0.0722 * v.z;
        let dirs = fibonacci_sphere(256);
        println!(
            "chunk ({}, {}) cells x0={} y0={} z0={} n=({}, {}, {})  y range {:?}",
            grid.chunk_x,
            grid.chunk_y,
            grid.x0,
            grid.y0,
            grid.z0,
            grid.nx,
            grid.ny,
            grid.nz,
            grid.y_range()
        );
        let cy = probe_cell_index(y_m);
        println!(
            "row at y = {y_m} m (cell {cy}); per column: c0 lum | dir ratio | clamped % | sun_vis"
        );
        println!("legend map A: dir ratio |c_dir|/c0  (.<0.25 :<0.5 -<0.75 =<1 #>=1)");
        println!("legend map B: clamped sphere fraction  (. 0  : <10%  - <25%  = <40%  # >=40%)");
        let mut map_a = Vec::new();
        let mut map_b = Vec::new();
        let mut map_s = Vec::new();
        let mut worst: Vec<(f32, i32, i32, ProbeSh)> = Vec::new();
        for z in 0..grid.nz as i32 {
            let (mut a, mut b, mut s) = (String::new(), String::new(), String::new());
            for x in 0..grid.nx as i32 {
                let Some(p) = grid.get_clamped_y(grid.x0 + x, cy, grid.z0 + z) else {
                    a.push('?');
                    b.push('?');
                    s.push('?');
                    continue;
                };
                let c0 = lum(p.c[0]).max(1e-6);
                let cd = Vec3::new(lum(p.c[1]), lum(p.c[2]), lum(p.c[3])).length();
                let ratio = cd / c0;
                let clamped = dirs
                    .iter()
                    .filter(|n| lum(p.c[0] + p.c[1] * n.x + p.c[2] * n.y + p.c[3] * n.z) < 0.0)
                    .count() as f32
                    / dirs.len() as f32;
                a.push(match ratio {
                    r if r < 0.25 => '.',
                    r if r < 0.5 => ':',
                    r if r < 0.75 => '-',
                    r if r < 1.0 => '=',
                    _ => '#',
                });
                b.push(match clamped {
                    0.0 => '.',
                    c if c < 0.10 => ':',
                    c if c < 0.25 => '-',
                    c if c < 0.40 => '=',
                    _ => '#',
                });
                s.push(if p.sun_vis > 0.5 { '1' } else { '0' });
                worst.push((clamped, grid.x0 + x, grid.z0 + z, p));
            }
            map_a.push(a);
            map_b.push(b);
            map_s.push(s);
        }
        println!("--- A: directionality ---");
        for l in &map_a {
            println!("{l}");
        }
        println!("--- B: clamped fraction ---");
        for l in &map_b {
            println!("{l}");
        }
        println!("--- S: sun_vis ---");
        for l in &map_s {
            println!("{l}");
        }
        worst.sort_by(|a, b| b.0.total_cmp(&a.0));
        println!("--- most clamped probes ---");
        for (c, x, z, p) in worst.iter().take(6) {
            let world = (probe_cell_centre(*x), probe_cell_centre(*z));
            println!(
                "cell ({x},{z}) world ({:.1},{:.1}) clamped {:.0}%  c0={:.3} |cdir|={:.3} sun_vis={}  E(+X)={:.3} E(-X)={:.3} E(+Y)={:.3} E(-Y)={:.3}",
                world.0,
                world.1,
                c * 100.0,
                lum(p.c[0]),
                Vec3::new(lum(p.c[1]), lum(p.c[2]), lum(p.c[3])).length(),
                p.sun_vis,
                lum(p.eval_l1(Vec3::X)),
                lum(p.eval_l1(Vec3::NEG_X)),
                lum(p.eval_l1(Vec3::Y)),
                lum(p.eval_l1(Vec3::NEG_Y)),
            );
        }
    }

    /// The shape a lamp's pool of light takes on the probe lattice, and how
    /// much of the damage is the lattice's resolution versus the *order* of
    /// interpolation and reconstruction.
    ///
    /// Reported symptom: the lamp's pool on the tops of cafe furniture reads
    /// as a diamond with a sharp point along each axis. The cafe's lamp (id
    /// 79) sits 2.5 m up with range 16.8 and a tabletop is ~1.0-1.2 m, so
    /// the lit pool is only a couple of metres across — about *one*
    /// `PROBE_CELL_SIZE`. Whatever the lattice does to a feature that small
    /// is what you see.
    ///
    /// Three fields are compared on a horizontal plane at tabletop height
    /// with `n = +Y`:
    ///
    /// * `exact` - the probe representation evaluated at the shading point
    ///   itself. No lattice; isolates the lobe fit.
    /// * `coeff` - trilinear over the four coefficient *vectors*, then
    ///   `eval_l1`. What the runtime did before the B-spline.
    /// * `value` - `eval_l1` at each of the eight corners, then trilinear
    ///   over the eight scalars. Same data, opposite order.
    ///
    /// "Diamondness" is the radius of a half-peak contour measured along +X
    /// (a lattice axis) divided by the radius along the X=Z diagonal. 1.0 is
    /// a circle; the further from 1.0, the more the lattice's own geometry
    /// is showing through.
    ///
    /// `cargo test lamp_pool_shape -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn lamp_pool_shape() {
        let hm = super::tests::flat_world();
        let cache = BakeHeightCache::extract(&hm, -60.0, -60.0, 60.0, 60.0);
        let bvh = TriangleBvh::build(Vec::new());
        let config = BakeConfig {
            probe_rays: 64,
            ao_steps: 16,
            shadow_steps: 32,
            ..Default::default()
        };
        // The cafe's own lamp (placed id 79), at its real world position,
        // because **the lamp's phase against the lattice is the whole
        // story**: -21.19 is 0.19 from the probe column at -21 and 25.25 is
        // 0.25 from the one at 25, so the lamp sits almost exactly on top of
        // a single probe. `LAMP=x,y,z` moves it.
        let pos = std::env::var("LAMP")
            .ok()
            .filter(|v| !v.is_empty())
            .map(|v| {
                let c: Vec<f32> = v.split(',').map(|n| n.parse().unwrap()).collect();
                Vec3::new(c[0], c[1], c[2])
            })
            .unwrap_or(Vec3::new(-21.19, 2.50, 25.25));
        let lamp = BakedLight::point(pos, Vec3::new(1.0, 0.85, 0.6), 1.28, 16.8);
        let lighting = BakeLighting {
            sun_dir: Vec3::Y,
            sun_rgb: Vec3::ZERO,
            ambient_rgb: Vec3::ZERO,
            lights: vec![lamp],
        };
        let ctx = ProbeContext {
            cache: &cache,
            bvh: &bvh,
            lighting: &lighting,
            config: &config,
            env_dirs: fibonacci_sphere(config.probe_rays as usize),
            normals: fibonacci_sphere(FIT_NORMALS),
        };
        let lum = |v: Vec3| 0.2126 * v.x + 0.7152 * v.y + 0.0722 * v.z;

        let height: f32 = std::env::var("H")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(1.1);
        let n = Vec3::Y;

        // Bake the lattice around the lamp, exactly as `bake_chunk_probes`
        // would: one probe per cell, at the cell centre.
        let span = 6i32;
        let (cx, cz) = (probe_cell_index(pos.x), probe_cell_index(pos.z));
        let yi0 = probe_cell_index(height) - 1;
        let grid: Vec<ProbeSh> = (0..(2 * span + 1) * (2 * span + 1) * 3)
            .into_par_iter()
            .map(|i| {
                let w = 2 * span + 1;
                let (xi, zi, yi) = (i % w - span, (i / w) % w - span, i / (w * w));
                ctx.shade(Vec3::new(
                    probe_cell_centre(cx + xi),
                    probe_cell_centre(yi0 + yi),
                    probe_cell_centre(cz + zi),
                ))
            })
            .collect();
        let at = |xi: i32, yi: i32, zi: i32| -> ProbeSh {
            let w = 2 * span + 1;
            grid[((yi * w + (zi - cz + span)) * w + (xi - cx + span)) as usize]
        };

        // Trilinear weights for a world point, over cells `i` whose probe
        // sits at `(i + 0.5) · cell` — the mapping the shader's repeat
        // sampler performs in hardware.
        let corners = |p: Vec3| {
            let t = p / PROBE_CELL_SIZE - Vec3::splat(0.5);
            let base = t.floor();
            let f = t - base;
            (base.as_ivec3(), f)
        };

        let plane = |p: Vec3| Vec3::new(p.x, height, p.z);

        let sample_coeff = |p: Vec3| {
            let (b, f) = corners(plane(p));
            let mut sh = ProbeSh::default();
            for k in 0..8 {
                let (dx, dy, dz) = (k & 1, (k >> 1) & 1, (k >> 2) & 1);
                let w = (if dx == 1 { f.x } else { 1.0 - f.x })
                    * (if dy == 1 { f.y } else { 1.0 - f.y })
                    * (if dz == 1 { f.z } else { 1.0 - f.z });
                let c = at(b.x + dx, b.y + dy - yi0, b.z + dz);
                for j in 0..4 {
                    sh.c[j] += c.c[j] * w;
                }
            }
            lum(sh.eval_l1(n))
        };
        let sample_value = |p: Vec3| {
            let (b, f) = corners(plane(p));
            let mut out = 0.0;
            for k in 0..8 {
                let (dx, dy, dz) = (k & 1, (k >> 1) & 1, (k >> 2) & 1);
                let w = (if dx == 1 { f.x } else { 1.0 - f.x })
                    * (if dy == 1 { f.y } else { 1.0 - f.y })
                    * (if dz == 1 { f.z } else { 1.0 - f.z });
                out += lum(at(b.x + dx, b.y + dy - yi0, b.z + dz).eval_l1(n)) * w;
            }
            out
        };
        let sample_exact = |p: Vec3| lum(ctx.shade(plane(p)).eval_l1(n));
        // The answer a lightmap would bake for this surface: the lamp's
        // clamped cosine, exactly.
        let sample_truth = |p: Vec3| {
            lum(lamp
                .sample(plane(p), n)
                .map(|(r, _, _)| r)
                .unwrap_or(Vec3::ZERO))
        };

        // Radius of the half-peak contour along a direction, by bisection.
        let radius = |f: &dyn Fn(Vec3) -> f32, dir: Vec3, peak: f32| {
            let target = peak * 0.5;
            let (mut lo, mut hi) = (0.0f32, 12.0f32);
            for _ in 0..40 {
                let mid = 0.5 * (lo + hi);
                if f(pos + Vec3::new(dir.x * mid, 0.0, dir.z * mid)).max(0.0) > target {
                    lo = mid;
                } else {
                    hi = mid;
                }
            }
            0.5 * (lo + hi)
        };

        let axis = Vec3::X;
        let diag = Vec3::new(1.0, 0.0, 1.0).normalize();
        println!("\nlamp pool at y={height}, n=+Y, PROBE_CELL_SIZE={PROBE_CELL_SIZE}");
        println!(
            "{:<10} {:>8} {:>9} {:>9} {:>11}",
            "field", "peak", "r(axis)", "r(diag)", "axis/diag"
        );
        for (name, f) in [
            ("truth", &sample_truth as &dyn Fn(Vec3) -> f32),
            ("exact", &sample_exact),
            ("coeff", &sample_coeff),
            ("value", &sample_value),
        ] {
            let peak = f(pos);
            let (ra, rd) = (radius(f, axis, peak), radius(f, diag, peak));
            println!(
                "{name:<10} {peak:>8.4} {ra:>9.3} {rd:>9.3} {:>11.3}",
                ra / rd.max(1e-6)
            );
        }

        // An ASCII slice of each lattice field, so the shape is visible.
        for (name, f) in [
            ("coeff", &sample_coeff as &dyn Fn(Vec3) -> f32),
            ("value", &sample_value),
            ("exact", &sample_exact),
        ] {
            let peak = f(pos);
            println!("\n{name} (rows = z, cols = x, 0-9 = fraction of peak {peak:.3}):");
            for zi in -24..=24 {
                let mut line = String::new();
                for xi in -24..=24 {
                    let v = f(pos + Vec3::new(xi as f32 * 0.25, 0.0, zi as f32 * 0.25));
                    let d = ((v / peak.max(1e-6)) * 9.0).clamp(0.0, 9.0) as usize;
                    line.push(b"0123456789"[d] as char);
                }
                println!("{line}");
            }
        }
    }

    /// Reconstruct the probe volume exactly as the shader does, over a
    /// horizontal plane, from a world's **baked** probe files — and print
    /// both the smooth field and the toon band index a surface there would
    /// land on, for the trilinear read and the B-spline one.
    ///
    /// The band map is the one that matters: the field can be perfectly
    /// smooth and still read as a hard-edged shape, because the quantiser
    /// draws one contour per boundary. This is the tool for "the light pool
    /// on the furniture is the wrong shape".
    ///
    /// `kink%` is the rms second difference of the contour radius `r(theta)`
    /// as a percentage of its mean — a circle scores ~0, every corner of a
    /// polygon is a spike. Measured on the cafe's lamp at tabletop height,
    /// over the four reconstructions that were tried:
    ///
    /// | read | centre lum | kink% b1 | b2 | b3 | axis/diag b1 |
    /// |---|---|---|---|---|---|
    /// | trilinear | 0.99 | 1.34 | 4.04 | 1.64 | 0.896 |
    /// | smoothstep-warped trilinear | 1.08 | 1.45 | 3.11 | 1.24 | 0.906 |
    /// | Catmull-Rom | 1.07 | 1.17 | 4.94 | 1.49 | 0.900 |
    /// | **cubic B-spline** | 0.73 | **0.09** | **1.10** | **0.03** | **0.989** |
    ///
    /// Only the B-spline actually removes the corners. Smoothstep warping
    /// trades the slope break for a plateau at every probe plane — a
    /// staircase instead of a kink — and Catmull-Rom, being interpolating
    /// and only C1, keeps the peak but leaves the contours as kinked as
    /// trilinear (its negative lobes make one band worse). Pre-smoothing the
    /// baked data with a binomial kernel scored well (0.52 / 0.54 / 0.04)
    /// and costs nothing at runtime, but it dims more than the B-spline,
    /// needs a format bump, and would need a margin per chunk to avoid
    /// seams, since probe grids are baked one chunk at a time.
    ///
    /// `WORLD=worlds/cafe V=12 AT=-21.19,25.25 Y=1.1 cargo test
    ///  dump_probe_plane -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn dump_probe_plane() {
        use crate::game::world::terrain::storage::parse_world_meta_from_bytes;
        use engine_api::toon_curve::luminance;

        let world = std::env::var("WORLD").unwrap_or_else(|_| "worlds/cafe".into());
        let root = std::path::Path::new(&world);
        let version: u32 = std::env::var("V")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(PROBE_FORMAT_VERSION);
        let y: f32 = std::env::var("Y")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(1.1);
        let span: f32 = std::env::var("SPAN")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(7.0);
        let at: Vec<f32> = std::env::var("AT")
            .unwrap_or_else(|_| "-21.19,25.25".into())
            .split(',')
            .map(|n| n.parse().unwrap())
            .collect();
        let centre = Vec3::new(at[0], y, at[1]);

        let meta = parse_world_meta_from_bytes(
            &std::fs::read(root.join("world.meta")).expect("world.meta"),
        )
        .expect("parse world.meta");
        let env = &meta.environment;
        let curve = env.toon_curve();
        let sun_dir = -env.sun_transform().forward().as_vec3();
        let sun_rgb = env.sun_radiance();

        let suffix = format!("_probes.v{version}.bin");
        let grids: Vec<ChunkProbes> = std::fs::read_dir(root.join("chunks"))
            .expect("chunks dir")
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.to_string_lossy().ends_with(&suffix))
            .filter_map(|p| std::fs::read(&p).ok().and_then(|b| parse_probes(&b).ok()))
            .collect();
        assert!(!grids.is_empty(), "no v{version} probe files under {world}");
        println!(
            "{} chunk grids, lamp plane y={y}, curve {:?}",
            grids.len(),
            curve
        );

        // The cell lookup the runtime volume performs, across chunk
        // boundaries. Vertically clamped, as `ProbeStore` is.
        let cell = |cx: i32, cy: i32, cz: i32| -> ProbeSh {
            grids
                .iter()
                .find_map(|g| {
                    (cx >= g.x0 && cx < g.x0 + g.nx as i32 && cz >= g.z0 && cz < g.z0 + g.nz as i32)
                        .then(|| g.get_clamped_y(cx, cy, cz))
                        .flatten()
                })
                .unwrap_or_default()
        };
        // Plain trilinear over the coefficients, then reconstruct: what the
        // shader did before the B-spline, kept as the baseline.
        // Cubic B-spline over the coefficients: 4 taps per axis instead of
        // 2, weights summing to 1. C2 everywhere, so the reconstructed
        // field has no slope break at a probe plane for a contour to kink
        // on. It is approximating rather than interpolating — it smooths
        // the data slightly, which for a lattice this coarse is a feature.
        let bspline_w = |t: f32| {
            let t2 = t * t;
            let t3 = t2 * t;
            [
                (1.0 - 3.0 * t + 3.0 * t2 - t3) / 6.0,
                (4.0 - 6.0 * t2 + 3.0 * t3) / 6.0,
                (1.0 + 3.0 * t + 3.0 * t2 - 3.0 * t3) / 6.0,
                t3 / 6.0,
            ]
        };
        let radiance_bspline = |p: Vec3, n: Vec3| -> Vec3 {
            let t = p / PROBE_CELL_SIZE - Vec3::splat(0.5);
            let b = t.floor();
            let f = t - b;
            let b = b.as_ivec3();
            let (wx, wy, wz) = (bspline_w(f.x), bspline_w(f.y), bspline_w(f.z));
            let mut sh = ProbeSh::default();
            for (iz, &az) in wz.iter().enumerate() {
                for (iy, &ay) in wy.iter().enumerate() {
                    for (ix, &ax) in wx.iter().enumerate() {
                        let w = ax * ay * az;
                        let c = cell(
                            b.x + ix as i32 - 1,
                            b.y + iy as i32 - 1,
                            b.z + iz as i32 - 1,
                        );
                        for j in 0..4 {
                            sh.c[j] += c.c[j] * w;
                        }
                        sh.sun_vis += c.sun_vis * w;
                    }
                }
            }
            sh.eval_l1(n) + sun_rgb * (sh.sun_vis * n.dot(sun_dir).max(0.0))
        };

        let radiance_trilinear = |p: Vec3, n: Vec3| -> Vec3 {
            let t = p / PROBE_CELL_SIZE - Vec3::splat(0.5);
            let b = t.floor();
            let f = t - b;
            let b = b.as_ivec3();
            let mut sh = ProbeSh::default();
            for k in 0..8 {
                let (dx, dy, dz) = (k & 1, (k >> 1) & 1, (k >> 2) & 1);
                let w = (if dx == 1 { f.x } else { 1.0 - f.x })
                    * (if dy == 1 { f.y } else { 1.0 - f.y })
                    * (if dz == 1 { f.z } else { 1.0 - f.z });
                let c = cell(b.x + dx, b.y + dy, b.z + dz);
                for j in 0..4 {
                    sh.c[j] += c.c[j] * w;
                }
                sh.sun_vis += c.sun_vis * w;
            }
            sh.eval_l1(n) + sun_rgb * (sh.sun_vis * n.dot(sun_dir).max(0.0))
        };

        let n = Vec3::Y;
        let cols = 97usize;
        let step = 2.0 * span / (cols - 1) as f32;

        let field = |p: Vec3, mode: usize| match mode {
            0 => luminance(radiance_trilinear(p, n)),
            _ => luminance(radiance_bspline(p, n)),
        };
        let radius_at = |dir: Vec3, level: f32, mode: usize| {
            let (mut lo, mut hi) = (0.0f32, span * 2.0);
            for _ in 0..40 {
                let mid = 0.5 * (lo + hi);
                if field(centre + dir * mid, mode) > level {
                    lo = mid;
                } else {
                    hi = mid;
                }
            }
            0.5 * (lo + hi)
        };

        for (mode, label) in [(0, "trilinear (pre-v12)"), (1, "b-spline (shipped)")] {
            let peak = field(centre, mode);
            println!(
                "\n=== {label}: centre lum {peak:.4} -> band {} of {}",
                (curve.band(peak) * curve.bands) as u32,
                curve.bands as u32
            );
            println!("band index per pixel ({span} m each way, '.' = band 0):");
            for zi in 0..cols {
                let mut line = String::new();
                for xi in 0..cols {
                    let p = centre
                        + Vec3::new(
                            (xi as f32 - (cols / 2) as f32) * step,
                            0.0,
                            (zi as f32 - (cols / 2) as f32) * step,
                        );
                    let b = (curve.band(field(p, mode)) * curve.bands).round() as usize;
                    line.push(if b == 0 {
                        '.'
                    } else {
                        b"0123456789"[b] as char
                    });
                }
                println!("{line}");
            }

            // Per band contour: anisotropy between a lattice axis and the
            // diagonal, and "kinkiness" — the rms second difference of
            // r(theta) around the contour, as a fraction of the mean radius.
            // A circle scores ~0; every corner of a polygon is a spike.
            println!("band contour  r(axis)  r(diag)  axis/diag   kink%");
            let diag = Vec3::new(1.0, 0.0, 1.0).normalize();
            for k in 1..=curve.bands as u32 {
                let level = curve.threshold(k);
                if level >= peak {
                    continue;
                }
                let (ra, rd) = (
                    radius_at(Vec3::X, level, mode),
                    radius_at(diag, level, mode),
                );
                let m = 720usize;
                let r: Vec<f32> = (0..m)
                    .map(|i| {
                        let a = i as f32 / m as f32 * std::f32::consts::TAU;
                        radius_at(Vec3::new(a.cos(), 0.0, a.sin()), level, mode)
                    })
                    .collect();
                let mean = r.iter().sum::<f32>() / m as f32;
                let kink = (0..m)
                    .map(|i| {
                        let d = r[(i + 1) % m] - 2.0 * r[i] + r[(i + m - 1) % m];
                        d * d
                    })
                    .sum::<f32>()
                    / m as f32;
                println!(
                    "{k:>12}  {ra:>7.3}  {rd:>7.3}  {:>9.3}  {:>6.3}",
                    ra / rd.max(1e-6),
                    100.0 * kink.sqrt() / mean.max(1e-6)
                );
            }
        }
    }
}
