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
#[derive(Resource)]
pub struct ProbeStore {
    grids: HashMap<(i32, i32), Arc<ChunkProbes>>,
    /// Chunks whose grid was added, replaced or removed since the volume
    /// last looked (`take_dirty`); `None` means "everything" (cleared).
    dirty: Option<Vec<(i32, i32)>>,
}

impl ProbeStore {
    pub fn insert(&mut self, probes: ChunkProbes) {
        let chunk = (probes.chunk_x, probes.chunk_y);
        self.grids.insert(chunk, Arc::new(probes));
        self.mark(chunk);
    }

    pub fn remove(&mut self, chunk: (i32, i32)) {
        if self.grids.remove(&chunk).is_some() {
            self.mark(chunk);
        }
    }

    pub fn clear(&mut self) {
        if !self.grids.is_empty() {
            self.grids.clear();
            self.dirty = None;
        }
    }

    fn mark(&mut self, chunk: (i32, i32)) {
        if let Some(list) = &mut self.dirty {
            list.push(chunk);
        }
    }

    /// What changed since the last call: `Some(chunks)` or `None` for
    /// everything. Resets to "nothing".
    fn take_dirty(&mut self) -> Option<Vec<(i32, i32)>> {
        self.dirty.replace(Vec::new())
    }
}

impl Default for ProbeStore {
    fn default() -> Self {
        Self {
            grids: HashMap::new(),
            dirty: Some(Vec::new()),
        }
    }
}

// ============================================================================
// The volume texture
// ============================================================================

use super::baking::probes::{
    PROBE_CHANNELS, ProbeSh, chunk_cell_range, probe_cell_centre, probe_cell_index,
};
use super::objects::world_pos_to_chunk;
use super::terrain::storage::TerrainWorld;
use crate::game::WorldFocus;
use crate::game::gfx::toon_material::ProbeVolumeImages;
use bevy::render::{
    ExtractSchedule, MainWorld, Render, RenderApp, RenderSystems,
    render_asset::RenderAssets,
    render_resource::{Extent3d, Origin3d, TexelCopyBufferLayout, TextureAspect},
    renderer::RenderQueue,
    texture::GpuImage,
};
use engine_api::toon_material::PROBE_VOLUME_DIMS;
use std::collections::VecDeque;

/// Bytes per texel of an `Rgba16Float` texture.
const TEXEL_BYTES: usize = 8;

/// Lattice cells (re)filled per frame at most. Everything the volume does
/// is budgeted by this: a 1 m step of the window is `48 × 192` cells, a
/// chunk's grid arriving is `64 × 48 × 64` (two frames), a full refill
/// (`1.77 M`) spreads over ~14. On wasm, where this runs on the main thread
/// with no rayon, one cell is ~20 ns of f16 copying, so the frame cost is
/// bounded at a few milliseconds instead of the 50–100 ms a full rebuild
/// took (`docs/WEB_PROFILING.md`).
const FILL_BUDGET_CELLS: usize = 131_072;

/// Where the toroidal window currently sits, what still needs filling, and
/// the texture regions filled this frame awaiting upload.
///
/// The window is `PROBE_VOLUME_DIMS` cells with its minimum corner at
/// `origin`; cell `c` of the lattice is at texel `c mod dims` (which is how
/// the shader finds it with no origin at all). It follows [`WorldFocus`]
/// cell by cell: because the mapping is toroidal, moving the window by `n`
/// cells only invalidates a slab `n` deep on each axis, so each step fills
/// just that slab, and a chunk's grid arriving refills just that chunk's
/// cells. Nothing here keeps a CPU copy of the textures — fills go straight
/// to [`ProbeUpload`] regions that the render world writes into the GPU
/// textures in place, so a material bind group is never touched.
#[derive(Resource)]
pub struct ProbeVolume {
    origin: IVec3,
    /// Until the first update: the whole window is enqueued then.
    placed: bool,
    /// World-cell boxes still to fill, clipped to the window when processed.
    jobs: VecDeque<CellBox>,
    /// Filled regions, drained by `extract_probe_uploads` each frame.
    uploads: Vec<ProbeUpload>,
}

impl Default for ProbeVolume {
    fn default() -> Self {
        Self {
            origin: IVec3::ZERO,
            placed: false,
            jobs: VecDeque::new(),
            uploads: Vec::new(),
        }
    }
}

/// One filled box of the four textures, in *texture* coordinates, laid out
/// x fastest then y then z within the box.
pub struct ProbeUpload {
    pub origin: UVec3,
    pub size: UVec3,
    pub data: [Vec<u8>; 4],
}

/// A half-open box of lattice cells, `min` inclusive, `max` exclusive.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct CellBox {
    min: IVec3,
    max: IVec3,
}

impl CellBox {
    fn new(min: IVec3, size: IVec3) -> Self {
        Self {
            min,
            max: min + size,
        }
    }

    fn size(&self) -> IVec3 {
        (self.max - self.min).max(IVec3::ZERO)
    }

    fn cells(&self) -> usize {
        let s = self.size();
        s.x as usize * s.y as usize * s.z as usize
    }

    fn is_empty(&self) -> bool {
        self.cells() == 0
    }

    fn intersect(&self, other: &CellBox) -> CellBox {
        CellBox {
            min: self.min.max(other.min),
            max: self.max.min(other.max),
        }
    }

    /// `self` minus `other`, as up to six disjoint boxes: the x slabs
    /// outside `other`, then y slabs within its x range, then z slabs
    /// within its x and y ranges.
    fn minus(&self, other: &CellBox) -> Vec<CellBox> {
        let mut out = Vec::new();
        let mut rest = *self;
        for axis in 0..3 {
            let (lo, hi) = (rest.min[axis], rest.max[axis]);
            let (olo, ohi) = (other.min[axis], other.max[axis]);
            // Below `other` on this axis.
            if lo < olo {
                let mut b = rest;
                b.max[axis] = olo.min(hi);
                out.push(b);
            }
            // Above it.
            if hi > ohi {
                let mut b = rest;
                b.min[axis] = ohi.max(lo);
                out.push(b);
            }
            // What is left overlaps `other` on this axis; narrow and go on.
            rest.min[axis] = lo.max(olo);
            rest.max[axis] = hi.min(ohi);
            if rest.is_empty() {
                break;
            }
        }
        out.retain(|b| !b.is_empty());
        out
    }

    /// Split off a leading piece of at most `budget` cells along the
    /// longest axis; returns `(piece, remainder)`.
    fn split(&self, budget: usize) -> (CellBox, Option<CellBox>) {
        if self.cells() <= budget {
            return (*self, None);
        }
        let s = self.size();
        let axis = if s.x >= s.y && s.x >= s.z {
            0
        } else if s.y >= s.z {
            1
        } else {
            2
        };
        let per_unit = (self.cells() / s[axis] as usize).max(1);
        let units = ((budget / per_unit).max(1) as i32).min(s[axis]);
        let mut piece = *self;
        piece.max[axis] = self.min[axis] + units;
        let mut rest = *self;
        rest.min[axis] = piece.max[axis];
        (piece, (!rest.is_empty()).then_some(rest))
    }
}

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

/// A world-cell range `[lo, lo + len)` as the one or two contiguous texture
/// ranges it maps to under `c mod dim`: `(texture start, world start, len)`.
/// `len` must not exceed `dim` (ranges are clipped to the window first).
fn wrap_ranges(lo: i32, len: i32, dim: i32) -> Vec<(i32, i32, i32)> {
    debug_assert!(len <= dim);
    let t = lo.rem_euclid(dim);
    if t + len <= dim {
        vec![(t, lo, len)]
    } else {
        let first = dim - t;
        vec![(t, lo, first), (0, lo + first, len - first)]
    }
}

impl ProbeVolume {
    /// Window origin that centres on `focus`.
    fn origin_for(focus: Vec3) -> IVec3 {
        focus_cell(focus) - dims() / 2
    }

    fn window(&self) -> CellBox {
        CellBox::new(self.origin, dims())
    }

    /// Texel of lattice cell `cell` — the reference mapping `fill` must agree
    /// with (it goes through `wrap_ranges`); tests read through it.
    #[cfg(test)]
    fn texel_of(cell: IVec3) -> UVec3 {
        let d = dims();
        UVec3::new(
            cell.x.rem_euclid(d.x) as u32,
            cell.y.rem_euclid(d.y) as u32,
            cell.z.rem_euclid(d.z) as u32,
        )
    }

    /// Queue a refill of the whole window (first placement, environment
    /// change, store cleared). Supersedes everything already queued.
    fn enqueue_all(&mut self) {
        self.jobs.clear();
        self.jobs.push_back(self.window());
    }

    /// Queue the cells of chunk `(cx, cz)` at every height.
    fn enqueue_chunk(&mut self, (cx, cz): (i32, i32)) {
        let (x0, nx) = chunk_cell_range(cx);
        let (z0, nz) = chunk_cell_range(cz);
        self.jobs.push_back(CellBox {
            min: IVec3::new(x0, i32::MIN / 2, z0),
            max: IVec3::new(x0 + nx as i32, i32::MAX / 2, z0 + nz as i32),
        });
    }

    /// Move the window to `origin`, queueing only the cells it newly covers.
    fn move_to(&mut self, origin: IVec3) {
        let old = self.window();
        self.origin = origin;
        let new = self.window();
        for slab in new.minus(&old) {
            self.jobs.push_back(slab);
        }
    }

    /// Work through queued boxes until `budget` cells have been filled.
    fn process_jobs(&mut self, store: &ProbeStore, fallback: &ProbeSh, mut budget: usize) {
        let fallback_bits = fallback.to_f16();
        let window = self.window();
        while budget > 0 {
            let Some(job) = self.jobs.pop_front() else {
                break;
            };
            let job = job.intersect(&window);
            if job.is_empty() {
                continue;
            }
            let (piece, rest) = job.split(budget);
            if let Some(rest) = rest {
                self.jobs.push_front(rest);
            }
            budget = budget.saturating_sub(piece.cells());
            self.fill(piece, store, &fallback_bits);
        }
    }

    /// Fill a world-cell box (already inside the window) into one upload
    /// per texture-contiguous piece.
    fn fill(&mut self, cells: CellBox, store: &ProbeStore, fallback_bits: &[u16; PROBE_CHANNELS]) {
        let d = dims();
        let s = cells.size();
        for (tz, wz, nz) in wrap_ranges(cells.min.z, s.z, d.z) {
            for (ty, wy, ny) in wrap_ranges(cells.min.y, s.y, d.y) {
                for (tx, wx, nx) in wrap_ranges(cells.min.x, s.x, d.x) {
                    let piece = CellBox::new(IVec3::new(wx, wy, wz), IVec3::new(nx, ny, nz));
                    let data = fill_region(&piece, store, fallback_bits);
                    self.uploads.push(ProbeUpload {
                        origin: UVec3::new(tx as u32, ty as u32, tz as u32),
                        size: UVec3::new(nx as u32, ny as u32, nz as u32),
                        data,
                    });
                }
            }
        }
    }
}

/// Fill one texture-contiguous box of world cells: the four textures' bytes,
/// x fastest then y then z, straight from the stored `f16`s (no decode).
/// `c0`'s alpha carries the sun visibility; the other three have none.
fn fill_region(
    cells: &CellBox,
    store: &ProbeStore,
    fallback_bits: &[u16; PROBE_CHANNELS],
) -> [Vec<u8>; 4] {
    let s = cells.size();
    let (nx, ny) = (s.x as usize, s.y as usize);
    let mut data: [Vec<u8>; 4] = std::array::from_fn(|_| vec![0u8; cells.cells() * TEXEL_BYTES]);
    for z in cells.min.z..cells.max.z {
        for x in cells.min.x..cells.max.x {
            // One chunk lookup per column, not per cell.
            let chunk = world_pos_to_chunk(probe_cell_centre(x), probe_cell_centre(z));
            let grid = store.grids.get(&chunk);
            let col = ((z - cells.min.z) as usize * ny) * nx + (x - cells.min.x) as usize;
            for y in cells.min.y..cells.max.y {
                let bits = grid
                    .and_then(|g| g.bits_clamped_y(x, y, z))
                    .unwrap_or(fallback_bits);
                let at = (col + (y - cells.min.y) as usize * nx) * TEXEL_BYTES;
                for (k, buf) in data.iter_mut().enumerate() {
                    let alpha = if k == 0 { bits[12] } else { 0 };
                    for (i, v) in [bits[k * 3], bits[k * 3 + 1], bits[k * 3 + 2], alpha]
                        .into_iter()
                        .enumerate()
                    {
                        buf[at + i * 2..at + i * 2 + 2].copy_from_slice(&v.to_le_bytes());
                    }
                }
            }
        }
    }
    data
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
/// Follow the focus, pick up store changes, and fill within budget.
pub fn update_probe_volume(
    focus: Res<WorldFocus>,
    terrain_world: Option<Res<TerrainWorld>>,
    mut store: ResMut<ProbeStore>,
    mut volume: ResMut<ProbeVolume>,
) {
    let origin = ProbeVolume::origin_for(focus.0);
    if !volume.placed {
        volume.origin = origin;
        volume.placed = true;
        volume.enqueue_all();
    } else if origin != volume.origin {
        volume.move_to(origin);
    }

    let environment_changed = terrain_world.as_ref().is_some_and(|w| w.is_changed());
    match store.take_dirty() {
        Some(chunks) if !environment_changed => {
            for chunk in chunks {
                volume.enqueue_chunk(chunk);
            }
        }
        _ => volume.enqueue_all(),
    }

    if volume.jobs.is_empty() {
        return;
    }
    let fallback = fallback_probe(terrain_world.as_deref());
    volume.process_jobs(&store, &fallback, FILL_BUDGET_CELLS);
}

// ============================================================================
// Render world: write the filled regions into the textures
// ============================================================================

/// Regions handed over from the main world, waiting for the textures.
#[derive(Resource, Default)]
struct ProbeUploadQueue(Vec<ProbeUpload>);

fn extract_probe_uploads(
    mut main_world: ResMut<MainWorld>,
    mut queue: ResMut<ProbeUploadQueue>,
    mut commands: Commands,
) {
    let mut volume = main_world.resource_mut::<ProbeVolume>();
    queue.0.append(&mut volume.uploads);
    if let Some(images) = main_world.get_resource::<ProbeVolumeImages>() {
        commands.insert_resource(images.clone());
    }
}

/// `write_texture` each region into the existing GPU textures. Regions are
/// kept until the textures exist (their first upload is bevy's own, from
/// the empty `Image`s), then written in order so later fills win.
fn write_probe_uploads(
    mut queue: ResMut<ProbeUploadQueue>,
    images: Option<Res<ProbeVolumeImages>>,
    gpu_images: Res<RenderAssets<GpuImage>>,
    render_queue: Res<RenderQueue>,
) {
    let Some(images) = images else {
        return;
    };
    let textures: Vec<_> = images
        .handles()
        .into_iter()
        .map(|h| gpu_images.get(h))
        .collect();
    if textures.iter().any(|t| t.is_none()) {
        return;
    }
    for upload in queue.0.drain(..) {
        for (k, texture) in textures.iter().enumerate() {
            let texture = &texture.unwrap().texture;
            render_queue.write_texture(
                bevy::render::render_resource::TexelCopyTextureInfo {
                    texture,
                    mip_level: 0,
                    origin: Origin3d {
                        x: upload.origin.x,
                        y: upload.origin.y,
                        z: upload.origin.z,
                    },
                    aspect: TextureAspect::All,
                },
                &upload.data[k],
                TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(upload.size.x * TEXEL_BYTES as u32),
                    rows_per_image: Some(upload.size.y),
                },
                Extent3d {
                    width: upload.size.x,
                    height: upload.size.y,
                    depth_or_array_layers: upload.size.z,
                },
            );
        }
    }
}

/// Render-world half of the volume; call from `WorldPlugin::build`.
pub fn build_render_app(app: &mut App) {
    let Some(render_app) = app.get_sub_app_mut(RenderApp) else {
        return;
    };
    render_app
        .init_resource::<ProbeUploadQueue>()
        .add_systems(ExtractSchedule, extract_probe_uploads)
        .add_systems(
            Render,
            write_probe_uploads.in_set(RenderSystems::PrepareResources),
        );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::world::baking::probes::{ChunkProbes, PROBE_CELL_SIZE, chunk_cell_range};
    use half::f16;

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

    /// What the GPU textures would hold: the uploads applied in order to
    /// four full-size buffers, the way `write_probe_uploads` applies them.
    struct Mirror([Vec<u8>; 4]);

    impl Mirror {
        fn new() -> Self {
            let n = dims().x as usize * dims().y as usize * dims().z as usize * TEXEL_BYTES;
            Self(std::array::from_fn(|_| vec![0u8; n]))
        }

        fn apply(&mut self, uploads: &[ProbeUpload]) {
            let d = dims();
            for u in uploads {
                let (sx, sy) = (u.size.x as usize, u.size.y as usize);
                for z in 0..u.size.z as usize {
                    for y in 0..sy {
                        let src = ((z * sy + y) * sx) * TEXEL_BYTES;
                        let dst =
                            (((u.origin.z as usize + z) * d.y as usize + u.origin.y as usize + y)
                                * d.x as usize
                                + u.origin.x as usize)
                                * TEXEL_BYTES;
                        for k in 0..4 {
                            self.0[k][dst..dst + sx * TEXEL_BYTES]
                                .copy_from_slice(&u.data[k][src..src + sx * TEXEL_BYTES]);
                        }
                    }
                }
            }
        }

        fn read(&self, cell: IVec3) -> ProbeSh {
            let d = dims();
            let t = ProbeVolume::texel_of(cell);
            let at = ((t.z as usize * d.y as usize + t.y as usize) * d.x as usize + t.x as usize)
                * TEXEL_BYTES;
            let get = |buf: &[u8], i: usize| {
                f16::from_le_bytes([buf[at + i * 2], buf[at + i * 2 + 1]]).to_f32()
            };
            let rgb =
                |k: usize| Vec3::new(get(&self.0[k], 0), get(&self.0[k], 1), get(&self.0[k], 2));
            ProbeSh {
                c: [rgb(0), rgb(1), rgb(2), rgb(3)],
                sun_vis: get(&self.0[0], 3),
            }
        }
    }

    /// Run every queued job to completion and return what the textures hold.
    fn drain(
        volume: &mut ProbeVolume,
        store: &ProbeStore,
        fallback: &ProbeSh,
        mirror: &mut Mirror,
    ) {
        while !volume.jobs.is_empty() {
            volume.process_jobs(store, fallback, FILL_BUDGET_CELLS);
            mirror.apply(&volume.uploads);
            volume.uploads.clear();
        }
    }

    fn place(volume: &mut ProbeVolume, origin: IVec3) {
        volume.origin = origin;
        volume.placed = true;
        volume.enqueue_all();
    }

    /// `cargo test --profile perf rebuild_cost -- --ignored --nocapture`:
    /// the cost of a full window refill (first placement, environment
    /// change), of one chunk's grid arriving, and of a one-cell step.
    #[test]
    #[ignore]
    fn rebuild_cost() {
        let mut store = ProbeStore::default();
        for cx in -1..=1 {
            for cz in -1..=1 {
                store.insert(grid_with((cx, cz), 0.5, -2, 20));
            }
        }
        let fallback = ProbeSh::default();
        let mut volume = ProbeVolume::default();
        let time = |volume: &mut ProbeVolume, label: &str| {
            let window = volume.window();
            let cells: usize = volume
                .jobs
                .iter()
                .map(|j| j.intersect(&window).cells())
                .sum();
            let start = std::time::Instant::now();
            while !volume.jobs.is_empty() {
                volume.process_jobs(&store, &fallback, usize::MAX);
            }
            let bytes: usize = volume
                .uploads
                .iter()
                .map(|u| u.data.iter().map(Vec::len).sum::<usize>())
                .sum();
            println!(
                "{label}: {cells} cells in {:?} ({:.1} MB uploaded)",
                start.elapsed(),
                bytes as f32 / 1e6
            );
            volume.uploads.clear();
        };
        place(&mut volume, IVec3::ZERO - dims() / 2);
        time(&mut volume, "full window");
        volume.enqueue_chunk((0, 0));
        time(&mut volume, "one chunk");
        volume.move_to(volume.origin + IVec3::X);
        time(&mut volume, "one-cell step");
    }

    #[test]
    fn texels_wrap_toroidally_and_match_the_shader_mapping() {
        let d = dims();
        assert_eq!(
            ProbeVolume::texel_of(IVec3::new(3, 4, 5)),
            ProbeVolume::texel_of(IVec3::new(3 + d.x, 4 - d.y, 5 + 2 * d.z))
        );
        assert_eq!(
            ProbeVolume::texel_of(IVec3::new(-1, 0, 0)),
            UVec3::new(d.x as u32 - 1, 0, 0)
        );
        // A range crossing the texture edge splits into two pieces that
        // together cover it exactly once.
        assert_eq!(wrap_ranges(5, 10, 16), vec![(5, 5, 10)]);
        assert_eq!(wrap_ranges(12, 10, 16), vec![(12, 12, 4), (0, 16, 6)]);
        assert_eq!(wrap_ranges(-3, 5, 16), vec![(13, -3, 3), (0, 0, 2)]);
    }

    #[test]
    fn box_minus_box_is_exact() {
        let a = CellBox::new(IVec3::new(0, 0, 0), IVec3::new(6, 5, 4));
        for shift in [
            IVec3::new(2, 0, 0),
            IVec3::new(-1, 2, 0),
            IVec3::new(3, -1, 1),
            IVec3::new(10, 0, 0),
            IVec3::ZERO,
        ] {
            let b = CellBox::new(a.min + shift, a.size());
            let slabs = b.minus(&a);
            // Every cell of b is either in a, or in exactly one slab.
            for z in b.min.z..b.max.z {
                for y in b.min.y..b.max.y {
                    for x in b.min.x..b.max.x {
                        let c = IVec3::new(x, y, z);
                        let in_a = (c.cmpge(a.min) & c.cmplt(a.max)).all();
                        let hits = slabs
                            .iter()
                            .filter(|s| (c.cmpge(s.min) & c.cmplt(s.max)).all())
                            .count();
                        assert_eq!(hits, if in_a { 0 } else { 1 }, "shift {shift} cell {c}");
                    }
                }
            }
            let total: usize = slabs.iter().map(CellBox::cells).sum();
            assert_eq!(total, b.cells() - a.intersect(&b).cells());
        }
    }

    #[test]
    fn split_respects_the_budget_and_loses_nothing() {
        let b = CellBox::new(IVec3::new(-3, 1, 7), IVec3::new(50, 10, 20));
        let mut budget_left = 0usize;
        let mut covered = 0usize;
        let mut rest = Some(b);
        let mut pieces = 0;
        while let Some(r) = rest {
            let (piece, next) = r.split(1000);
            assert!(piece.cells() <= 1000 || piece.size().max_element() == 1);
            assert!(!piece.is_empty());
            covered += piece.cells();
            budget_left += 1000usize.saturating_sub(piece.cells());
            rest = next;
            pieces += 1;
        }
        assert_eq!(covered, b.cells());
        assert!(
            pieces <= 12,
            "{pieces} pieces, {budget_left} cells of slack"
        );
    }

    #[test]
    fn fill_takes_loaded_grids_and_falls_back_elsewhere() {
        let mut store = ProbeStore::default();
        store.insert(grid_with((0, 0), 0.8, 0, 3));
        let fallback = ProbeSh {
            c: [Vec3::splat(0.2), Vec3::ZERO, Vec3::ZERO, Vec3::ZERO],
            sun_vis: 1.0,
        };
        let mut volume = ProbeVolume::default();
        let mut mirror = Mirror::new();
        place(&mut volume, IVec3::new(-4, -2, -4));
        drain(&mut volume, &store, &fallback, &mut mirror);

        // Inside chunk (0,0), inside the grid's y range: the grid's probe.
        let inside = mirror.read(IVec3::new(5, 1, 5));
        assert!((inside.c[0].x - 0.8).abs() < 1e-3, "{inside:?}");
        assert!((inside.c[1].x - 0.8).abs() < 1e-3);
        assert_eq!(inside.sun_vis, 0.0);
        // Above the grid: clamped to its top row, not the fallback.
        let above = mirror.read(IVec3::new(5, 40, 5));
        assert!((above.c[0].x - 0.8).abs() < 1e-3, "{above:?}");
        // A column in a chunk with no grid: the fallback.
        let elsewhere = mirror.read(IVec3::new(-2, 1, -2));
        assert!((elsewhere.c[0].x - 0.2).abs() < 1e-3, "{elsewhere:?}");
        assert_eq!(elsewhere.sun_vis, 1.0);
    }

    /// Moving the window step by step must leave the textures identical to
    /// a full rebuild at the final position — the whole point of the slab
    /// bookkeeping — and a chunk arriving must only touch its own cells.
    #[test]
    fn incremental_moves_and_chunk_arrivals_match_a_full_rebuild() {
        let mut store = ProbeStore::default();
        store.insert(grid_with((0, 0), 0.8, 0, 3));
        store.insert(grid_with((1, 0), 0.4, 0, 3));
        let fallback = ProbeSh::default();

        let mut moved = ProbeVolume::default();
        let mut moved_mirror = Mirror::new();
        place(&mut moved, IVec3::new(-10, -5, -10));
        drain(&mut moved, &store, &fallback, &mut moved_mirror);
        let mut origin = moved.origin;
        for step in [
            IVec3::X,
            IVec3::X,
            IVec3::new(0, 1, -2),
            IVec3::new(7, 0, 3),
            IVec3::new(-40, -2, 0),
        ] {
            origin += step;
            moved.move_to(origin);
            drain(&mut moved, &store, &fallback, &mut moved_mirror);
        }
        // A grid lands after the moves.
        store.insert(grid_with((-1, -1), 0.6, 0, 3));
        for chunk in store.take_dirty().unwrap() {
            moved.enqueue_chunk(chunk);
        }
        drain(&mut moved, &store, &fallback, &mut moved_mirror);

        let mut fresh = ProbeVolume::default();
        let mut fresh_mirror = Mirror::new();
        place(&mut fresh, origin);
        drain(&mut fresh, &store, &fallback, &mut fresh_mirror);

        assert_eq!(moved.origin, fresh.origin);
        for k in 0..4 {
            assert!(
                moved_mirror.0[k] == fresh_mirror.0[k],
                "texture {k} differs"
            );
        }
        // And the arrival only re-filled the window's slice of that chunk.
        let (x0, nx) = chunk_cell_range(-1);
        let window = fresh.window();
        let expected = CellBox {
            min: IVec3::new(x0, i32::MIN / 2, x0),
            max: IVec3::new(x0 + nx as i32, i32::MAX / 2, x0 + nx as i32),
        }
        .intersect(&window)
        .cells();
        let mut probe = ProbeVolume::default();
        place(&mut probe, origin);
        probe.jobs.clear();
        probe.enqueue_chunk((-1, -1));
        probe.process_jobs(&store, &fallback, usize::MAX);
        let filled: usize = probe
            .uploads
            .iter()
            .map(|u| (u.size.x * u.size.y * u.size.z) as usize)
            .sum();
        assert_eq!(filled, expected);
    }

    #[test]
    fn the_window_follows_the_focus_cell_by_cell() {
        let home = Vec3::new(100.0, 10.0, -50.0);
        let o = ProbeVolume::origin_for(home);
        assert_eq!(
            ProbeVolume::origin_for(home + Vec3::X * PROBE_CELL_SIZE),
            o + IVec3::X
        );
        assert_eq!(
            ProbeVolume::origin_for(home + Vec3::X * PROBE_CELL_SIZE * 0.4),
            o
        );
        // Centred: the focus cell sits in the middle of the window.
        assert_eq!(focus_cell(home) - o, dims() / 2);
    }
}
