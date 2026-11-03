//! Runtime home of baked irradiance probes.
//!
//! `ProbeStore` holds every loaded chunk's probe grid (`baking::probes`),
//! fed by the chunk streamer when a chunk arrives and by the bake when it
//! finishes, and emptied as chunks unload. The volume texture that dynamic
//! objects actually sample is assembled from it — see the plan in
//! `docs/DYNAMIC_LIGHTING_PLAN.md` §8.

use bevy::prelude::*;
use std::{collections::HashMap, sync::Arc};

use super::baking::probes::ChunkProbes;

/// Probe grids of every loaded chunk, keyed by chunk coordinates.
#[derive(Resource, Default)]
pub struct ProbeStore {
    grids: HashMap<(i32, i32), Arc<ChunkProbes>>,
    /// Set whenever a grid is added, replaced or removed; whoever consumes
    /// the store clears it.
    pub dirty: bool,
}

impl ProbeStore {
    pub fn insert(&mut self, probes: ChunkProbes) {
        self.grids
            .insert((probes.chunk_x, probes.chunk_y), Arc::new(probes));
        self.dirty = true;
    }

    pub fn remove(&mut self, chunk: (i32, i32)) {
        if self.grids.remove(&chunk).is_some() {
            self.dirty = true;
        }
    }

    pub fn clear(&mut self) {
        if !self.grids.is_empty() {
            self.grids.clear();
            self.dirty = true;
        }
    }
}

// ============================================================================
// The volume texture
// ============================================================================

use super::baking::probes::{ProbeSh, probe_cell_centre, probe_cell_index};
use super::objects::world_pos_to_chunk;
use super::terrain::storage::TerrainWorld;
use crate::game::WorldFocus;
use crate::game::gfx::toon_material::ProbeVolumeImages;
use engine_api::toon_material::PROBE_VOLUME_DIMS;
use half::f16;
use rayon::prelude::*;

/// Bytes per texel of an `Rgba16Float` texture.
const TEXEL_BYTES: usize = 8;

/// Where the toroidal window currently sits, and the CPU copies of the four
/// textures it is built into.
///
/// The window is `PROBE_VOLUME_DIMS` cells with its minimum corner at
/// `origin`; cell `c` of the lattice is at texel `c mod dims` (which is how
/// the shader finds it with no origin at all). It re-centres on
/// [`WorldFocus`] when the focus leaves its middle third, and is rebuilt from
/// the [`ProbeStore`] when that changes — both by refilling the CPU buffers
/// and handing them to the `Image`s, which bevy copies into the existing GPU
/// textures in place.
#[derive(Resource)]
pub struct ProbeVolume {
    origin: IVec3,
    buffers: [Vec<u8>; 4],
    /// Rebuild requested (store changed, environment changed, re-centred).
    dirty: bool,
    /// Rebuilds are coalesced: streaming can land several chunks a second,
    /// and each full upload is ~57 MB (four textures).
    cooldown: f32,
}

impl Default for ProbeVolume {
    fn default() -> Self {
        let n = PROBE_VOLUME_DIMS.iter().product::<u32>() as usize * TEXEL_BYTES;
        Self {
            origin: IVec3::MIN,
            buffers: std::array::from_fn(|_| vec![0u8; n]),
            dirty: true,
            cooldown: 0.0,
        }
    }
}

/// Seconds between coalesced rebuilds while chunks keep arriving.
const REBUILD_INTERVAL: f32 = 0.5;

/// Lattice cell the window would be centred on for a focus position.
fn focus_cell(focus: Vec3) -> IVec3 {
    IVec3::new(
        probe_cell_index(focus.x),
        probe_cell_index(focus.y),
        probe_cell_index(focus.z),
    )
}

fn dims() -> IVec3 {
    IVec3::new(
        PROBE_VOLUME_DIMS[0] as i32,
        PROBE_VOLUME_DIMS[1] as i32,
        PROBE_VOLUME_DIMS[2] as i32,
    )
}

impl ProbeVolume {
    /// Window origin that centres on `focus`.
    fn origin_for(focus: Vec3) -> IVec3 {
        focus_cell(focus) - dims() / 2
    }

    /// True when `focus` has left the middle third of the window on any
    /// axis — the point at which the window moves.
    fn needs_recentre(&self, focus: Vec3) -> bool {
        let centre = self.origin + dims() / 2;
        let off = (focus_cell(focus) - centre).abs();
        off.cmpgt(dims() / 6).any()
    }

    /// Byte offset of lattice cell `cell`'s texel *within its z layer*.
    ///
    /// The shipping half of the toroidal mapping: [`Self::rebuild`] works a
    /// layer at a time, so this is what it indexes with.
    fn layer_offset(cell: IVec3) -> usize {
        let d = dims();
        // Image layout: x fastest, then y (rows).
        (cell.y.rem_euclid(d.y) * d.x + cell.x.rem_euclid(d.x)) as usize * TEXEL_BYTES
    }

    /// Byte offset of lattice cell `cell`'s texel in each buffer.
    ///
    /// The whole mapping in one expression, which is the reference form the
    /// tests and [`Self::read`] use; `rebuild` reaches it a layer at a time
    /// through [`Self::layer_offset`], so both share that half.
    #[cfg(test)]
    fn texel_offset(cell: IVec3) -> usize {
        let d = dims();
        let layer = (d.x * d.y) as usize * TEXEL_BYTES;
        cell.z.rem_euclid(d.z) as usize * layer + Self::layer_offset(cell)
    }

    /// Write one probe into the four buffers at byte offset `at`.
    fn write_at(bufs: [&mut [u8]; 4], at: usize, probe: &ProbeSh) {
        // `c0`'s alpha carries the sun visibility; the other three have none.
        let alpha = [probe.sun_vis, 0.0, 0.0, 0.0];
        for (k, buf) in bufs.into_iter().enumerate() {
            let rgb = probe.c[k];
            for (i, v) in [rgb.x, rgb.y, rgb.z, alpha[k]].into_iter().enumerate() {
                buf[at + i * 2..at + i * 2 + 2].copy_from_slice(&f16::from_f32(v).to_le_bytes());
            }
        }
    }

    /// Refill every texel of the window from `store`, with `fallback` where
    /// no loaded chunk has a probe (unbaked or unloaded ground).
    ///
    /// Parallel over the texture's **z layers**, which is sound precisely
    /// because the mapping is toroidal: cell `z` lands in layer
    /// `z mod dims.z`, so each layer is written by exactly one world `z` and
    /// no two tasks touch the same byte. At the 1 m lattice the window is
    /// 1.77 M cells and this runs inline in `Update` on a re-centre — 17.8 ms
    /// single-threaded, which is a dropped frame every time the focus leaves
    /// the middle third (~32 m). `rebuild_cost` measures it.
    ///
    /// Still O(window) per re-centre. The toroidal layout means a move of
    /// `n` cells only *invalidates* a slab `n` deep on each axis, so filling
    /// just those would be the real fix; it is more bookkeeping than this
    /// was worth, and `store.dirty` rebuilds need the full pass anyway.
    pub fn rebuild(&mut self, origin: IVec3, store: &ProbeStore, fallback: &ProbeSh) {
        self.origin = origin;
        let d = dims();
        let layer = (d.x * d.y) as usize * TEXEL_BYTES;
        let [b0, b1, b2, b3] = &mut self.buffers;

        b0.par_chunks_mut(layer)
            .zip(b1.par_chunks_mut(layer))
            .zip(b2.par_chunks_mut(layer))
            .zip(b3.par_chunks_mut(layer))
            .enumerate()
            .for_each(|(tz, (((c0, c1), c2), c3))| {
                // The one world z in the window that maps to this layer.
                let z = origin.z + (tz as i32 - origin.z).rem_euclid(d.z);
                for x in origin.x..origin.x + d.x {
                    // One chunk lookup per column, not per cell.
                    let chunk = world_pos_to_chunk(probe_cell_centre(x), probe_cell_centre(z));
                    let grid = store.grids.get(&chunk);
                    let tx = x.rem_euclid(d.x);
                    for y in origin.y..origin.y + d.y {
                        let probe = grid
                            .and_then(|g| g.get_clamped_y(x, y, z))
                            .unwrap_or(*fallback);
                        let at = Self::layer_offset(IVec3::new(tx, y, z));
                        Self::write_at([c0, c1, c2, c3], at, &probe);
                    }
                }
            });
    }

    /// The probe written for cell `cell`.
    #[cfg(test)]
    pub fn read(&self, cell: IVec3) -> ProbeSh {
        let at = Self::texel_offset(cell);
        let get = |buf: &[u8], i: usize| {
            f16::from_le_bytes([buf[at + i * 2], buf[at + i * 2 + 1]]).to_f32()
        };
        let rgb = |k: usize| {
            Vec3::new(
                get(&self.buffers[k], 0),
                get(&self.buffers[k], 1),
                get(&self.buffers[k], 2),
            )
        };
        ProbeSh {
            c: [rgb(0), rgb(1), rgb(2), rgb(3)],
            sun_vis: get(&self.buffers[0], 3),
        }
    }
}

/// What a cell no chunk covers gets: the sky and sun of an open field, so an
/// unbaked level shows a sunlit character rather than a black one.
pub fn fallback_probe(terrain_world: Option<&TerrainWorld>) -> ProbeSh {
    let ambient = terrain_world
        .map(|w| w.metadata.environment.ambient_radiance())
        .unwrap_or(Vec3::splat(0.25));
    ProbeSh {
        c: [ambient, Vec3::ZERO, Vec3::ZERO, Vec3::ZERO],
        sun_vis: 1.0,
    }
}

/// Keep the volume textures following the player and the probe store.
pub fn update_probe_volume(
    time: Res<Time>,
    focus: Res<WorldFocus>,
    terrain_world: Option<Res<TerrainWorld>>,
    mut store: ResMut<ProbeStore>,
    mut volume: ResMut<ProbeVolume>,
    handles: Res<ProbeVolumeImages>,
    mut images: ResMut<Assets<Image>>,
) {
    volume.cooldown = (volume.cooldown - time.delta_secs()).max(0.0);

    let recentre = volume.needs_recentre(focus.0);
    if store.dirty || terrain_world.as_ref().is_some_and(|w| w.is_changed()) {
        volume.dirty = true;
        store.dirty = false;
    }
    // A move is applied at once (the player is about to leave the data);
    // store churn waits out the cooldown.
    if !(recentre || (volume.dirty && volume.cooldown == 0.0)) {
        return;
    }

    let origin = if recentre {
        ProbeVolume::origin_for(focus.0)
    } else {
        volume.origin
    };
    let fallback = fallback_probe(terrain_world.as_deref());
    volume.rebuild(origin, &store, &fallback);
    volume.dirty = false;
    volume.cooldown = REBUILD_INTERVAL;

    for (handle, buffer) in handles.handles().into_iter().zip(&volume.buffers) {
        if let Some(mut image) = images.get_mut(handle) {
            image.data = Some(buffer.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::world::baking::probes::{
        ChunkProbes, PROBE_CELL_SIZE, PROBE_CHANNELS, chunk_cell_range,
    };

    fn grid_with(chunk: (i32, i32), value: f32, y0: i32, ny: u32) -> ChunkProbes {
        let (x0, nx) = chunk_cell_range(chunk.0);
        let (z0, nz) = chunk_cell_range(chunk.1);
        let probe = ProbeSh {
            c: [Vec3::splat(value), Vec3::X * value, Vec3::ZERO, Vec3::ZERO],
            sun_vis: 0.0,
        };
        let mut data = Vec::with_capacity((nx * ny * nz) as usize * PROBE_CHANNELS);
        for _ in 0..nx * ny * nz {
            data.extend_from_slice(&probe.to_f16());
        }
        ChunkProbes {
            chunk_x: chunk.0,
            chunk_y: chunk.1,
            x0,
            y0,
            z0,
            nx,
            ny,
            nz,
            data,
        }
    }

    /// How long a full window rebuild takes, which is the cost the 1 m
    /// lattice bought: the window is `192 × 48 × 192` = 1.77 M cells and
    /// every re-centre (the focus leaving the middle third, so every ~32 m)
    /// rewrites all of them plus ~57 MB of buffer copies. It runs inline in
    /// `Update`, so this is a frame hitch, not background work.
    ///
    /// `cargo test --release rebuild_cost -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn rebuild_cost() {
        let mut store = ProbeStore::default();
        // A 9-chunk ring of real-sized grids around the origin, as the
        // streamer would have loaded.
        for cx in -1..=1 {
            for cz in -1..=1 {
                store.insert(grid_with((cx, cz), 0.5, -2, 20));
            }
        }
        let mut volume = ProbeVolume::default();
        let fallback = ProbeSh::default();
        let d = dims();
        // Warm up, then time a re-centre to a fresh origin each round so no
        // run can benefit from the last one's cache.
        volume.rebuild(IVec3::ZERO, &store, &fallback);
        let rounds = 5usize;
        let start = std::time::Instant::now();
        for i in 0..rounds as i32 {
            volume.rebuild(IVec3::splat(i * 4) - d / 2, &store, &fallback);
        }
        let per = start.elapsed() / rounds as u32;
        println!(
            "window {}x{}x{} = {} cells: rebuild {:?} ({:.1} MB of buffers)",
            d.x,
            d.y,
            d.z,
            d.x * d.y * d.z,
            per,
            (volume.buffers.iter().map(|b| b.len()).sum::<usize>() as f32) / 1e6,
        );
    }

    #[test]
    fn texels_wrap_toroidally_and_match_the_shader_mapping() {
        let d = dims();
        // Cell i and cell i + dims land on the same texel.
        assert_eq!(
            ProbeVolume::texel_offset(IVec3::new(3, 4, 5)),
            ProbeVolume::texel_offset(IVec3::new(3 + d.x, 4 - d.y, 5 + 2 * d.z))
        );
        // Negative cells wrap to the top, like `fract` in the shader.
        assert_eq!(
            ProbeVolume::texel_offset(IVec3::new(-1, 0, 0)),
            ProbeVolume::texel_offset(IVec3::new(d.x - 1, 0, 0))
        );
        // `rebuild` indexes within a layer, so that half must agree with the
        // reference form — it is the piece that actually ships.
        for cell in [
            IVec3::new(0, 0, 0),
            IVec3::new(5, 7, 11),
            IVec3::new(-3, -1, -9),
            dims() + IVec3::ONE,
        ] {
            let layer = (dims().x * dims().y) as usize * TEXEL_BYTES;
            assert_eq!(
                ProbeVolume::texel_offset(cell) % layer,
                ProbeVolume::layer_offset(cell),
                "layer_offset disagrees with texel_offset at {cell}"
            );
        }

        // x fastest, then y, then z.
        assert_eq!(ProbeVolume::texel_offset(IVec3::new(1, 0, 0)), TEXEL_BYTES);
        assert_eq!(
            ProbeVolume::texel_offset(IVec3::new(0, 1, 0)),
            d.x as usize * TEXEL_BYTES
        );
        assert_eq!(
            ProbeVolume::texel_offset(IVec3::new(0, 0, 1)),
            (d.x * d.y) as usize * TEXEL_BYTES
        );
    }

    #[test]
    fn rebuild_takes_loaded_grids_and_falls_back_elsewhere() {
        let mut store = ProbeStore::default();
        store.insert(grid_with((0, 0), 0.8, 0, 3));
        let fallback = ProbeSh {
            c: [Vec3::splat(0.2), Vec3::ZERO, Vec3::ZERO, Vec3::ZERO],
            sun_vis: 1.0,
        };
        let mut volume = ProbeVolume::default();
        volume.rebuild(IVec3::new(-4, -2, -4), &store, &fallback);

        // Inside chunk (0,0), inside the grid's y range: the grid's probe.
        let inside = volume.read(IVec3::new(5, 1, 5));
        assert!((inside.c[0].x - 0.8).abs() < 1e-3, "{inside:?}");
        assert!((inside.c[1].x - 0.8).abs() < 1e-3);
        assert_eq!(inside.sun_vis, 0.0);
        // Above the grid: clamped to its top row, not the fallback.
        let above = volume.read(IVec3::new(5, 40, 5));
        assert!((above.c[0].x - 0.8).abs() < 1e-3, "{above:?}");
        // A column in a chunk with no grid: the fallback.
        let elsewhere = volume.read(IVec3::new(-2, 1, -2));
        assert!((elsewhere.c[0].x - 0.2).abs() < 1e-3, "{elsewhere:?}");
        assert_eq!(elsewhere.sun_vis, 1.0);
    }

    #[test]
    fn window_recentres_only_when_the_focus_leaves_the_middle_third() {
        let d = dims();
        let mut volume = ProbeVolume::default();
        let home = Vec3::new(100.0, 10.0, -50.0);
        volume.origin = ProbeVolume::origin_for(home);
        assert!(!volume.needs_recentre(home));
        // Still inside the middle third.
        let near = home + Vec3::X * (PROBE_CELL_SIZE * (d.x / 6) as f32 - 0.1);
        assert!(!volume.needs_recentre(near), "{near}");
        // Just past it.
        let far = home + Vec3::X * (PROBE_CELL_SIZE * (d.x / 6 + 2) as f32);
        assert!(volume.needs_recentre(far), "{far}");
        // Vertically too.
        let up = home + Vec3::Y * (PROBE_CELL_SIZE * (d.y / 6 + 2) as f32);
        assert!(volume.needs_recentre(up));
    }
}
