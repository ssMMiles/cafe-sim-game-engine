//! Irradiance probes: baked lighting for things that move.
//!
//! A lightmap texel answers "what light reaches *this surface*". A probe
//! answers "what light would reach a surface with normal `n` at *this point
//! in space*", for every `n` at once, so a character walking through the
//! point can be shaded with the same model as the static world around it —
//! sun, sky, bounce and every placed point/spot light, with the same shadows.
//!
//! Probes sit on a world-aligned lattice ([`PROBE_CELL_SIZE`]) and are baked
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
use super::lightmap::{BakeLighting, LIGHTMAP_FORMAT_VERSION, LightmapLocation};
use super::raycast::{self, BakeHeightCache};
use crate::game::world::objects::CHUNK_WORLD_SIZE;
use crate::game::world::terrain::storage::StorageError;
pub use engine_api::toon_material::{PROBE_CELL_SIZE, probe_cell_centre, probe_cell_index};

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
    /// Direct light arriving at a surface the probe can see, plus sky:
    /// the sun through a shadow ray, and every placed light through a
    /// segment ray. `dda` is the origin for the terrain march, lifted off
    /// the surface; `surface` is the point itself, for the BVH.
    fn direct_at(&self, dda: Vec3, surface: Vec3, normal: Vec3) -> Vec3 {
        let ndotl = normal.dot(self.lighting.sun_dir).max(0.0);
        let vis = raycast::cast_shadow_ray(
            dda,
            surface,
            self.lighting.sun_dir,
            self.cache,
            self.bvh,
            self.config.ao_radius * 4.0,
            self.config.shadow_steps,
        );
        self.lighting.sun_rgb * (ndotl * vis)
            + self.lighting.local_lights(surface, normal, |dir, dist| {
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
            + self.lighting.ambient_rgb
    }

    /// Radiance a terrain hit at `t` along `dir` from `p` sends back: the
    /// lightmap's own bounce model — albedo x whatever lights it.
    fn terrain_radiance(&self, p: Vec3, dir: Vec3, t: f32) -> Vec3 {
        let q = p + dir * t;
        let h = self.cache.sample_height(q.x, q.z);
        let ground = Vec3::new(q.x, h, q.z);
        let (normal, _) = self.cache.sample_normal(q.x, q.z);
        self.direct_at(ground + Vec3::Y * 0.5, ground + normal * 0.02, normal)
            * self.config.terrain_albedo
    }

    /// Radiance a scenery hit at `t` along `dir` sends back. The same model
    /// as [`Self::terrain_radiance`] — albedo x (direct sun + sky) — but
    /// against the triangle's own normal.
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
        self.direct_at(q + Vec3::Y * 0.5, q, normal) * self.config.scenery_albedo
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
    format!("chunk_{cx}_{cy}_probes.v{LIGHTMAP_FORMAT_VERSION}.bin")
}

/// Path of a chunk's probe file inside a world directory. Versioned like the
/// lightmaps: probes are baked with the same lighting model, so a model
/// change invalidates both at once.
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
    out.extend_from_slice(&LIGHTMAP_FORMAT_VERSION.to_le_bytes());
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
    if version != LIGHTMAP_FORMAT_VERSION {
        return Err(StorageError::VersionMismatch {
            expected: LIGHTMAP_FORMAT_VERSION,
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
        // 63.75 m at 2 m cells is 31 or 32 cells.
        let (_, n) = chunk_cell_range(0);
        assert!((31..=32).contains(&n), "{n}");
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

        // Below ground: a copy of the first open-air probe above.
        let buried = probes
            .get(x0 + 5, probe_cell_index(-1.0), probes.z0 + 5)
            .unwrap();
        let lowest_open = probes
            .get(x0 + 5, probe_cell_index(1.0), probes.z0 + 5)
            .unwrap();
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
        old[4..8].copy_from_slice(&(LIGHTMAP_FORMAT_VERSION - 1).to_le_bytes());
        assert!(matches!(
            parse_probes(&old),
            Err(StorageError::VersionMismatch { .. })
        ));

        // Clamped access.
        assert_eq!(probes.get_clamped_y(-32, 99, 64), probes.get(-32, 1, 64));
        assert_eq!(probes.get_clamped_y(-32, -99, 64), probes.get(-32, -1, 64));
        assert_eq!(probes.get_clamped_y(0, 0, 64), None);
        assert!(chunk_probes_path(Path::new("w"), 3, -4).ends_with(format!(
            "chunks/chunk_3_-4_probes.v{LIGHTMAP_FORMAT_VERSION}.bin"
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
}
