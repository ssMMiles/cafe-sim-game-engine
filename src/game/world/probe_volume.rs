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
    /// and each full upload is ~12 MB.
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

    /// Byte offset of lattice cell `cell`'s texel in each buffer.
    fn texel_offset(cell: IVec3) -> usize {
        let d = dims();
        let t = IVec3::new(
            cell.x.rem_euclid(d.x),
            cell.y.rem_euclid(d.y),
            cell.z.rem_euclid(d.z),
        );
        // Image layout: x fastest, then y (rows), then z (layers).
        ((t.z * d.y + t.y) * d.x + t.x) as usize * TEXEL_BYTES
    }

    fn write(&mut self, cell: IVec3, probe: &ProbeSh) {
        let at = Self::texel_offset(cell);
        let put = |buf: &mut [u8], rgb: Vec3, a: f32| {
            for (i, v) in [rgb.x, rgb.y, rgb.z, a].into_iter().enumerate() {
                buf[at + i * 2..at + i * 2 + 2].copy_from_slice(&f16::from_f32(v).to_le_bytes());
            }
        };
        put(&mut self.buffers[0], probe.c[0], probe.sun_vis);
        put(&mut self.buffers[1], probe.c[1], 0.0);
        put(&mut self.buffers[2], probe.c[2], 0.0);
        put(&mut self.buffers[3], probe.c[3], 0.0);
    }

    /// Refill every texel of the window from `store`, with `fallback` where
    /// no loaded chunk has a probe (unbaked or unloaded ground).
    pub fn rebuild(&mut self, origin: IVec3, store: &ProbeStore, fallback: &ProbeSh) {
        self.origin = origin;
        let d = dims();
        let end = origin + d;
        // Every column, from whichever chunk's grid covers it.
        for z in origin.z..end.z {
            for x in origin.x..end.x {
                let chunk = world_pos_to_chunk(probe_cell_centre(x), probe_cell_centre(z));
                let grid = store.grids.get(&chunk);
                for y in origin.y..end.y {
                    let probe = grid
                        .and_then(|g| g.get_clamped_y(x, y, z))
                        .unwrap_or(*fallback);
                    self.write(IVec3::new(x, y, z), &probe);
                }
            }
        }
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
