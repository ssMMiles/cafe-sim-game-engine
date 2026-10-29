//! GPU compute path for terrain lightmap baking.
//!
//! Triggered by `run_bake` in `mod.rs` when `BakeConfig::use_gpu` is `true`
//! *and* the adapter reports compute support ([`GpuBakeSupported`]). The CPU
//! path in `terrain.rs` remains the fallback.
//!
//! This runs on wasm too: WebGPU has compute shaders, and the bake fits
//! comfortably inside its limits (5 storage buffers of the 8 guaranteed, a
//! 16×16 workgroup at the 256-invocation cap, an 8 MB output buffer against a
//! 128 MiB binding cap). That matters more on the web than anywhere else —
//! there rayon is a single-threaded stub and bevy's task pool is `spawn_local`
//! on the main thread, so the CPU bake freezes the tab for its whole duration.
//!
//! ## Pipeline
//!
//! 1. **Main world** (`enqueue_terrain_bake_jobs`): For each loaded chunk build
//!    a `ShaderBuffer` output buffer + metadata, push a `GpuBakeJob` into
//!    `GpuBakePending`, and spawn a readback entity with an observer.
//!
//! 2. **ExtractSchedule** (`extract_bake_jobs`): Drain jobs from main world into
//!    the render world's `GpuBakeExtracted`.
//!
//! 3. **PrepareBindGroups** (`prepare_gpu_bake_jobs`): Upload heightmap + uniform
//!    data as raw GPU buffers, look up the `GpuShaderBuffer` for the output
//!    handle, build the bind group.
//!
//! 4. **Render graph node** (`BakeComputeNode`): Dispatch the compute shader.
//!
//! 5. **Observer on readback entity** (`handle_bake_complete`): Decode the
//!    packed f16 RGBA texels, blur, save EXR, swap into the material.

use bevy::{
    asset::{Assets, RenderAssetUsages},
    pbr::ExtendedMaterial,
    prelude::*,
    render::{
        ExtractSchedule, MainWorld, Render, RenderApp, RenderStartup, RenderSystems,
        gpu_readback::{Readback, ReadbackComplete},
        render_asset::RenderAssets,
        render_resource::{
            BindGroup, BindGroupEntries, BindGroupLayoutDescriptor, BindGroupLayoutEntries, Buffer,
            BufferUsages, CachedComputePipelineId, ComputePassDescriptor,
            ComputePipelineDescriptor, DownlevelFlags, PipelineCache, ShaderStages, binding_types,
        },
        renderer::{RenderContext, RenderDevice, RenderGraph, RenderGraphSystems},
        storage::{GpuShaderBuffer, ShaderBuffer},
    },
};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use super::bvh::{BvhNode, BvhTriangle, TriangleBvh};
use super::lightmap::{
    BakeLighting, BakedLight, ChunkBakeGrid, LightmapBuffer, LightmapLocation, store_lightmap,
    terrain_lightmap_location,
};
use super::terrain::extract_bake_cache;
use super::{BakeConfig, BakeRegion};
use crate::game::world::{
    objects::CHUNK_WORLD_SIZE,
    terrain::{
        HeightmapData,
        material::TerrainMaterial,
        storage::{TerrainChunkRegistry, TerrainWorld},
    },
};

const SHADER_PATH: &str = "shaders/lightmap_bake.wgsl";

/// Whether the live adapter can actually run the compute bake.
///
/// Deliberately a runtime flag rather than a `#[cfg]`: one wasm binary gets
/// compute when the browser gives it WebGPU and *not* when it falls back to
/// WebGL2, which is only knowable once the adapter exists. The adapter lives
/// in the render app, so the bool is shared with the main world — where
/// `run_bake` reads it — through an `Arc`.
///
/// Defaults to `false`, so a bake triggered before `RenderStartup` has run
/// takes the CPU path rather than dispatching into a pipeline that may not
/// exist.
#[derive(Resource, Clone)]
pub struct GpuBakeSupported(Arc<AtomicBool>);

impl GpuBakeSupported {
    pub fn get(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
}

/// Record whether this adapter can run the compute bake.
///
/// Both halves of this are bevy's own test (`batching::gpu_preprocessing`):
/// the adapter has to advertise compute, *and* the device limits must not be
/// the WebGL2 downlevel set, which simulates a lack of compute by zeroing
/// every `max_compute_*` limit while the downlevel flag still reads true.
fn detect_compute_support(
    adapter: Res<bevy::render::renderer::RenderAdapter>,
    device: Res<RenderDevice>,
    supported: Res<GpuBakeSupported>,
) {
    let downlevel = adapter
        .get_downlevel_capabilities()
        .flags
        .contains(DownlevelFlags::COMPUTE_SHADERS);
    let limits_ok = device.limits().max_compute_workgroup_storage_size != 0;

    let ok = downlevel && limits_ok;
    supported.0.store(ok, Ordering::Relaxed);
    if ok {
        info!("GPU lightmap bake enabled (adapter supports compute shaders)");
    } else {
        warn!(
            "No compute shader support (downlevel={downlevel}, limits_ok={limits_ok}); \
             lightmap baking falls back to the CPU path"
        );
    }
}

// ============================================================================
// CPU-side uniform struct (must match BakeUniforms in the WGSL exactly)
// ============================================================================

#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
pub struct BakeUniformsData {
    pub cache_min_x: f32,
    pub cache_min_z: f32,
    pub tile_size_x: f32,
    pub tile_size_z: f32,
    pub cache_stride: u32,
    pub cache_depth: u32,
    /// Finished lightmap resolution; the shader bakes `+ 2·margin`.
    pub terrain_resolution: u32,
    pub ao_rays: u32,
    /// Chunk index: texel positions and seeds derive from the chunk's place
    /// in the world texel grid, so seams match (`ChunkBakeGrid::world_xz`).
    pub chunk_index_x: i32,
    pub chunk_index_y: i32,
    pub chunk_world_size: f32,
    pub ao_radius: f32,
    pub sun_dir_x: f32,
    pub sun_dir_y: f32,
    pub sun_dir_z: f32,
    pub shadow_steps: u32,
    pub cache_tile_min_x: i32,
    pub cache_tile_min_z: i32,
    pub bounce_rays: u32,
    pub bounce_strength: f32,
    pub terrain_albedo: f32,
    pub bounce_count: u32,
    /// Starting row of this dispatch strip. Set per-strip by `dispatch_strip`.
    pub row_offset: u32,
    /// Number of nodes in the scene BVH buffer; 0 means no occluders.
    pub num_bvh_nodes: u32,
    /// Number of DDA steps for AO rays (shorter range than shadow/bounce).
    pub ao_steps: u32,
    /// Sun colour × intensity, lightmap units (`BakeLighting::sun_rgb`).
    pub sun_r: f32,
    pub sun_g: f32,
    pub sun_b: f32,
    /// Ambient colour × intensity, lightmap units (`BakeLighting::ambient_rgb`).
    pub ambient_r: f32,
    pub ambient_g: f32,
    pub ambient_b: f32,
    /// Number of `BakedLight`s in the lights storage buffer.
    pub num_lights: u32,
    /// Extra texels baked on every side of the chunk (`ChunkBakeGrid::margin`).
    pub margin: u32,
    /// Clamp on a local light's shadow-ray step count. Was a `48u` literal in
    /// the WGSL, which silently desynced the GPU terrain bake from the CPU
    /// paths the moment `BakeConfig::max_light_shadow_steps` changed.
    pub max_light_shadow_steps: u32,
    pub _pad1: u32,
    pub _pad2: u32,
}

// ============================================================================
// Main-world resources
// ============================================================================

/// Pending GPU bake jobs queued by the main world each bake trigger.
#[derive(Resource, Default)]
pub struct GpuBakePending {
    pub jobs: Vec<GpuBakeChunkJob>,
}

/// Tracks readback entities that have already been processed this bake session.
///
/// `Readback` queues a new request every frame the component exists. Multiple
/// results for the same entity can arrive in the same `sync_readbacks` call
/// before the deferred despawn is applied. This set prevents double-processing.
/// Cleared by `enqueue_terrain_bake_jobs` at the start of each bake.
#[derive(Resource, Default)]
pub struct GpuBakeProcessed(pub std::collections::HashSet<Entity>);

/// One terrain chunk bake job containing all its strips (main world side).
///
/// The heightmap and hull triangle data are stored once per chunk rather than
/// cloned per strip, eliminating ~(N-1) redundant copies of the ~3 MB heightmap.
pub struct GpuBakeChunkJob {
    pub chunk_x: i32,
    pub chunk_y: i32,
    /// Lightmap resolution plus the bake margin; the GPU works on the padded grid.
    pub grid: ChunkBakeGrid,
    /// Number of rows each strip covers (used to size the Y workgroup dispatch).
    pub strip_rows: u32,
    /// One uniform set per strip, differing only in `row_offset`.
    pub strip_uniforms: Vec<BakeUniformsData>,
    /// Flat heightmap tile values covering chunk + ray-reach border (shared across strips).
    pub heightmap_bytes: Vec<f32>,
    /// Scene BVH, uploaded verbatim (shared across strips and chunks).
    pub bvh: Arc<TriangleBvh>,
    /// Placed point/spot lights, uploaded verbatim (shared across strips).
    pub lights: Vec<BakedLight>,
    /// Handle to the output `ShaderBuffer` (pre-allocated, zeroed).
    pub out_handle: Handle<ShaderBuffer>,
    /// Material to swap the lightmap into once the readback completes.
    pub material_handle: AssetId<ExtendedMaterial<StandardMaterial, TerrainMaterial>>,
    /// Where the finished EXR goes — a file path natively, an OPFS key on
    /// wasm. Resolved in the main world, where `TerrainWorld` lives.
    pub dest: LightmapLocation,
}

/// Metadata stored on the readback entity so the observer knows what to do.
#[derive(Component)]
pub struct BakeReadbackTag {
    pub chunk_x: i32,
    pub chunk_y: i32,
    pub grid: ChunkBakeGrid,
    pub material_handle: AssetId<ExtendedMaterial<StandardMaterial, TerrainMaterial>>,
    pub dest: LightmapLocation,
}

/// One terrain chunk waiting to be dispatched (no GPU buffer allocated yet).
struct GpuBakeQueueEntry {
    chunk_x: i32,
    chunk_y: i32,
    grid: ChunkBakeGrid,
    uniforms_data: BakeUniformsData,
    heightmap_bytes: Vec<f32>,
    /// Scene BVH, uploaded verbatim.
    bvh: Arc<TriangleBvh>,
    /// Placed point/spot lights, uploaded verbatim.
    lights: Vec<BakedLight>,
    material_handle: AssetId<ExtendedMaterial<StandardMaterial, TerrainMaterial>>,
    dest: LightmapLocation,
}

/// Serial dispatch queue — holds all remaining chunks that have not yet had a
/// GPU buffer allocated. The front entry is dispatched when the previous
/// readback completes, keeping peak VRAM usage to one chunk at a time.
#[derive(Resource, Default)]
pub struct GpuBakeQueue {
    pending: std::collections::VecDeque<GpuBakeQueueEntry>,
    /// How many chunks currently have a live output buffer + readback entity.
    /// Used by `top_up_gpu_bake` to stay at `gpu_parallel_chunks` in-flight.
    pub in_flight: u32,
}

// ============================================================================
// Render-world resources
// ============================================================================

/// Jobs extracted from `GpuBakePending` into the render world.
#[derive(Resource, Default)]
struct GpuBakeExtracted {
    jobs: Vec<GpuBakeChunkJob>,
}

/// A single strip dispatch that has had its bind group built and is ready to dispatch.
struct BakeInFlightStrip {
    bind_group: BindGroup,
    resolution: u32,
    /// Number of rows this strip covers (drives the Y workgroup count).
    strip_rows: u32,
    /// Per-strip uniform buffer (different `row_offset` per strip).
    _uniform_buf: Buffer,
}

/// Shared GPU buffers for a chunk, kept alive until all its strips have dispatched.
struct ChunkSharedBuffers {
    _heightmap_buf: Buffer,
    _bvh_nodes_buf: Buffer,
    _bvh_tris_buf: Buffer,
    _lights_buf: Buffer,
}

/// In-flight jobs awaiting dispatch via the render graph node.
#[derive(Resource, Default)]
pub struct GpuBakeInFlight {
    strips: Vec<BakeInFlightStrip>,
    /// Shared buffers kept alive until after dispatch + cleanup.
    _shared: Vec<ChunkSharedBuffers>,
}

/// Cached pipeline + layout descriptor for the lightmap compute shader.
#[derive(Resource)]
struct GpuBakePipeline {
    layout: BindGroupLayoutDescriptor,
    pipeline_id: CachedComputePipelineId,
}

// ============================================================================
// Bake compute pass
// ============================================================================

/// Dispatches the lightmap compute pass.
///
/// Bevy 0.19 removed the `RenderGraph` *API* (nodes, labels, edges) but kept
/// `RenderGraph` as the top-level **schedule** for rendering that isn't tied to
/// a camera view — which is exactly what this is. So the old
/// `render_graph::Node` became a plain system in that schedule, in
/// [`RenderGraphSystems::Render`] (before `Submit` flushes the command
/// buffers).
///
/// The resource lookups stay fallible for the same reason as before: the
/// pipeline may still be compiling, in which case jobs simply retry next frame
/// because `Readback` keeps polling until the entity is despawned.
fn bake_compute_pass(
    pipeline_res: Option<Res<GpuBakePipeline>>,
    in_flight: Option<Res<GpuBakeInFlight>>,
    pipeline_cache: Res<PipelineCache>,
    mut ctx: RenderContext,
) {
    let (Some(pipeline_res), Some(in_flight)) = (pipeline_res, in_flight) else {
        return;
    };

    let Some(compute_pipeline) = pipeline_cache.get_compute_pipeline(pipeline_res.pipeline_id)
    else {
        return;
    };

    for strip in &in_flight.strips {
        let mut pass = ctx
            .command_encoder()
            .begin_compute_pass(&ComputePassDescriptor {
                label: Some("lightmap_bake_pass"),
                timestamp_writes: None,
            });

        pass.set_pipeline(compute_pipeline);
        pass.set_bind_group(0, &strip.bind_group, &[]);

        // 16×16 workgroups, rounded up to cover the strip.
        let wg_x = strip.resolution.div_ceil(16);
        let wg_y = strip.strip_rows.div_ceil(16);
        pass.dispatch_workgroups(wg_x, wg_y, 1);
    }
}

// ============================================================================
// Plugin wiring (called from BakingPlugin::build)
// ============================================================================

/// Register all render-app resources and systems for the GPU baking path.
/// Called from `BakingPlugin::build` only on non-wasm targets.
pub fn add_gpu_baking_to_app(app: &mut App) {
    let supported = GpuBakeSupported(Arc::new(AtomicBool::new(false)));
    app.insert_resource(supported.clone());
    app.init_resource::<GpuBakePending>();
    app.init_resource::<GpuBakeProcessed>();
    app.init_resource::<GpuBakeQueue>();

    // ReadbackComplete is fired via world.trigger() (global), not trigger_targets,
    // so we must register a global observer here — entity-level .observe() won't fire.
    app.add_observer(handle_bake_complete);

    use crate::game::world::WorldSet;
    app.add_systems(Update, top_up_gpu_bake.in_set(WorldSet::InGame));

    let Some(render_app) = app.get_sub_app_mut(RenderApp) else {
        return;
    };

    render_app
        .insert_resource(supported)
        .init_resource::<GpuBakeExtracted>()
        .init_resource::<GpuBakeInFlight>()
        .add_systems(
            RenderStartup,
            (init_gpu_bake_pipeline, detect_compute_support),
        )
        .add_systems(ExtractSchedule, extract_bake_jobs)
        .add_systems(
            RenderGraph,
            bake_compute_pass.in_set(RenderGraphSystems::Render),
        )
        .add_systems(
            Render,
            (
                prepare_gpu_bake_jobs.in_set(RenderSystems::PrepareBindGroups),
                clear_in_flight_after_render.in_set(RenderSystems::Cleanup),
            ),
        );
}

// ============================================================================
// Main-world: enqueue jobs
// ============================================================================

/// Populate the serial bake queue with one entry per loaded terrain chunk, then
/// dispatch the first chunk immediately.
///
/// Subsequent chunks are dispatched one-at-a-time as each readback completes in
/// `handle_bake_complete`. This keeps peak VRAM usage to a single output buffer
/// regardless of how many chunks are loaded, preventing OOM at high resolutions.
///
/// Called from `dispatch_gpu_terrain_bake` in `mod.rs` when `use_gpu` is true.
pub fn enqueue_terrain_bake_jobs(
    heightmap: &HeightmapData,
    terrain_world: &TerrainWorld,
    config: &BakeConfig,
    region: &BakeRegion,
    chunk_registry: &TerrainChunkRegistry,
    chunk_query: &Query<(
        &crate::game::world::terrain::TerrainChunk,
        &MeshMaterial3d<ExtendedMaterial<StandardMaterial, TerrainMaterial>>,
    )>,
    processed: &mut GpuBakeProcessed,
    queue: &mut GpuBakeQueue,
    lighting: &BakeLighting,
    bvh: &Arc<TriangleBvh>,
) {
    // Clear processed set so results from a prior bake don't block this one,
    // and discard any leftover entries from an interrupted previous bake.
    processed.0.clear();
    queue.pending.clear();

    let grid = ChunkBakeGrid::new(config);
    let ray_reach = config.ao_radius * 4.0;

    for (chunk, material_handle) in chunk_query.iter() {
        if !region.contains_chunk(chunk.chunk_x, chunk.chunk_y)
            || !chunk_registry
                .loaded_chunks
                .contains_key(&(chunk.chunk_x, chunk.chunk_y))
        {
            continue;
        }

        let cache = extract_bake_cache(heightmap, grid, chunk.chunk_x, chunk.chunk_y, ray_reach);

        let uniforms_data = BakeUniformsData {
            cache_min_x: cache.cache_min_x(),
            cache_min_z: cache.cache_min_z(),
            tile_size_x: cache.tile_size.0,
            tile_size_z: cache.tile_size.1,
            cache_stride: cache.stride() as u32,
            cache_depth: cache.depth() as u32,
            terrain_resolution: grid.res as u32,
            ao_rays: config.ao_rays,
            chunk_index_x: chunk.chunk_x,
            chunk_index_y: chunk.chunk_y,
            chunk_world_size: CHUNK_WORLD_SIZE,
            ao_radius: config.ao_radius,
            sun_dir_x: lighting.sun_dir.x,
            sun_dir_y: lighting.sun_dir.y,
            sun_dir_z: lighting.sun_dir.z,
            shadow_steps: config.shadow_steps,
            cache_tile_min_x: cache.tile_min_x(),
            cache_tile_min_z: cache.tile_min_z(),
            bounce_rays: config.bounce_rays,
            bounce_strength: config.bounce_strength,
            terrain_albedo: config.terrain_albedo,
            bounce_count: config.bounce_count,
            row_offset: 0,
            num_bvh_nodes: bvh.nodes.len() as u32,
            ao_steps: config.ao_steps,
            sun_r: lighting.sun_rgb.x,
            sun_g: lighting.sun_rgb.y,
            sun_b: lighting.sun_rgb.z,
            ambient_r: lighting.ambient_rgb.x,
            ambient_g: lighting.ambient_rgb.y,
            ambient_b: lighting.ambient_rgb.z,
            num_lights: lighting.lights.len() as u32,
            margin: grid.margin as u32,
            max_light_shadow_steps: config.max_light_shadow_steps.max(4),
            _pad1: 0,
            _pad2: 0,
        };

        queue.pending.push_back(GpuBakeQueueEntry {
            chunk_x: chunk.chunk_x,
            chunk_y: chunk.chunk_y,
            grid,
            uniforms_data,
            heightmap_bytes: cache.into_data(),
            bvh: bvh.clone(),
            material_handle: material_handle.id(),
            dest: terrain_lightmap_location(terrain_world, chunk.chunk_x, chunk.chunk_y),
            lights: lighting.lights.clone(),
        });
    }

    let total = queue.pending.len();
    info!(
        "GPU terrain bake queued {total} chunks (parallel_chunks={}, resolution={}). \
         Dispatching will begin next frame.",
        config.gpu_parallel_chunks, config.terrain_resolution
    );
}

// ============================================================================
// Main-world: chunk dispatch helper
// ============================================================================

/// Allocate the output SSB, build all strip uniforms, and push a single
/// `GpuBakeChunkJob` containing every strip for the chunk.
///
/// This dispatches all strips in one frame, sharing the heightmap and hull
/// triangle data across strips instead of cloning them per-strip.
fn dispatch_chunk(
    entry: GpuBakeQueueEntry,
    strip_rows: u32,
    buffers: &mut Assets<ShaderBuffer>,
    pending: &mut GpuBakePending,
) {
    let padded = entry.grid.padded() as u32;
    let texel_count = (padded * padded) as usize;
    // One `vec2<u32>` per texel: four packed f16s, i.e. an Rgba16Float texel.
    let byte_count = texel_count * 2 * std::mem::size_of::<u32>();

    // COPY_SRC is required by GpuReadbackPlugin so it can copy results to CPU.
    let mut out_buf = ShaderBuffer::with_size(byte_count as u64, RenderAssetUsages::RENDER_WORLD);
    out_buf.buffer_usage = BufferUsages::STORAGE | BufferUsages::COPY_SRC | BufferUsages::COPY_DST;
    let out_handle = buffers.add(out_buf);

    let total_strips = padded.div_ceil(strip_rows);

    // Build one uniform set per strip, differing only in row_offset.
    let mut strip_uniforms = Vec::with_capacity(total_strips as usize);
    for s in 0..total_strips {
        let mut uniforms = entry.uniforms_data;
        uniforms.row_offset = s * strip_rows;
        strip_uniforms.push(uniforms);
    }

    pending.jobs.push(GpuBakeChunkJob {
        chunk_x: entry.chunk_x,
        chunk_y: entry.chunk_y,
        grid: entry.grid,
        strip_rows,
        strip_uniforms,
        heightmap_bytes: entry.heightmap_bytes,
        bvh: entry.bvh,
        lights: entry.lights,
        out_handle,
        material_handle: entry.material_handle,
        dest: entry.dest,
    });
}

// ============================================================================
// Main-world: per-frame queue top-up
// ============================================================================

/// Each frame, dispatch the next queued chunk if a slot is available.
///
/// All strips for a chunk are dispatched in a single frame. Each strip is a
/// small compute dispatch (e.g. 64 rows of 4096 texels = ~1024 workgroups),
/// so dispatching all ~64 strips back-to-back is well within TDR limits.
/// The `dispatch_strip_rows` config still controls per-dispatch granularity
/// if individual dispatches need to be smaller.
pub fn top_up_gpu_bake(
    config: Res<BakeConfig>,
    mut queue: ResMut<GpuBakeQueue>,
    mut buffers: ResMut<Assets<ShaderBuffer>>,
    mut pending: ResMut<GpuBakePending>,
    mut commands: Commands,
) {
    while queue.in_flight < config.gpu_parallel_chunks && !queue.pending.is_empty() {
        let entry = queue.pending.pop_front().unwrap();
        let remaining = queue.pending.len();
        let total_strips = (entry.grid.padded() as u32).div_ceil(config.dispatch_strip_rows);
        let chunk_x = entry.chunk_x;
        let chunk_y = entry.chunk_y;

        info!(
            "GPU terrain bake: dispatching chunk ({}, {}) — {} strips of {} rows ({} remaining in queue)",
            chunk_x, chunk_y, total_strips, config.dispatch_strip_rows, remaining,
        );

        dispatch_chunk(
            entry,
            config.dispatch_strip_rows,
            &mut buffers,
            &mut pending,
        );

        // The chunk job we just pushed is the last one in the vec.
        let chunk_job = pending.jobs.last().unwrap();
        commands.spawn((
            Readback::buffer(chunk_job.out_handle.clone()),
            BakeReadbackTag {
                chunk_x,
                chunk_y,
                grid: chunk_job.grid,
                material_handle: chunk_job.material_handle,
                dest: chunk_job.dest.clone(),
            },
            crate::game::GameEntity,
        ));
        queue.in_flight += 1;
    }
}

// ============================================================================
// Render-world: extract
// ============================================================================

/// Extract pending jobs from the main world into the render world.
fn extract_bake_jobs(mut main_world: ResMut<MainWorld>, mut extracted: ResMut<GpuBakeExtracted>) {
    let mut pending = main_world.resource_mut::<GpuBakePending>();
    extracted.jobs.append(&mut pending.jobs);
}

// ============================================================================
// Render-world: init pipeline
// ============================================================================

fn init_gpu_bake_pipeline(
    mut commands: Commands,
    asset_server: Res<AssetServer>,
    pipeline_cache: Res<PipelineCache>,
) {
    use std::num::NonZero;

    // `BakeUniformsData` is a plain `bytemuck` struct, not `ShaderType`, so we
    // use the `_sized` variants and provide the size explicitly.
    let uniform_size = NonZero::new(std::mem::size_of::<BakeUniformsData>() as u64)
        .expect("BakeUniformsData must be non-zero sized");

    let layout = BindGroupLayoutDescriptor::new(
        "lightmap_bake_layout",
        &BindGroupLayoutEntries::sequential(
            ShaderStages::COMPUTE,
            (
                // binding 0 – heightmap read-only storage (no min-size needed)
                binding_types::storage_buffer_read_only_sized(false, None),
                // binding 1 – BakeUniforms uniform buffer
                binding_types::uniform_buffer_sized(false, Some(uniform_size)),
                // binding 2 – output storage (read/write, no min-size needed)
                binding_types::storage_buffer_sized(false, None),
                // binding 3 – scene BVH nodes, read-only storage
                binding_types::storage_buffer_read_only_sized(false, None),
                // binding 4 – placed lights, read-only storage
                binding_types::storage_buffer_read_only_sized(false, None),
                // binding 5 – scene BVH triangles, read-only storage
                binding_types::storage_buffer_read_only_sized(false, None),
            ),
        ),
    );

    let shader = asset_server.load(SHADER_PATH);
    let pipeline_id = pipeline_cache.queue_compute_pipeline(ComputePipelineDescriptor {
        label: Some("lightmap_bake_pipeline".into()),
        layout: vec![layout.clone()],
        shader,
        zero_initialize_workgroup_memory: false,
        ..default()
    });

    commands.insert_resource(GpuBakePipeline {
        layout,
        pipeline_id,
    });
}

// ============================================================================
// Render-world: prepare bind groups
// ============================================================================

fn prepare_gpu_bake_jobs(
    render_device: Res<RenderDevice>,
    pipeline_cache: Res<PipelineCache>,
    pipeline: Option<Res<GpuBakePipeline>>,
    gpu_buffers: Res<RenderAssets<GpuShaderBuffer>>,
    mut extracted: ResMut<GpuBakeExtracted>,
    mut in_flight: ResMut<GpuBakeInFlight>,
) {
    use bevy::render::render_resource::BufferInitDescriptor;

    let Some(pipeline) = pipeline else {
        return;
    };

    let bind_group_layout = pipeline_cache.get_bind_group_layout(&pipeline.layout);

    for chunk_job in extracted.jobs.drain(..) {
        // Retrieve the GPU-side output buffer prepared from the ShaderBuffer asset.
        let Some(gpu_out) = gpu_buffers.get(&chunk_job.out_handle) else {
            warn!(
                "GPU bake: output ShaderBuffer not ready for chunk ({}, {}), skipping",
                chunk_job.chunk_x, chunk_job.chunk_y
            );
            continue;
        };

        // Upload heightmap ONCE for all strips of this chunk.
        let heightmap_buf = render_device.create_buffer_with_data(&BufferInitDescriptor {
            label: Some("bake_heightmap"),
            contents: bytemuck::cast_slice(&chunk_job.heightmap_bytes),
            usage: BufferUsages::STORAGE | BufferUsages::COPY_DST,
        });

        // Upload the scene BVH ONCE per chunk (dummy entries when there are
        // no occluders, so the bindings are valid; `num_bvh_nodes == 0`
        // short-circuits traversal).
        let node_data: &[BvhNode] = if chunk_job.bvh.nodes.is_empty() {
            &[bytemuck::Zeroable::zeroed()]
        } else {
            &chunk_job.bvh.nodes
        };
        let bvh_nodes_buf = render_device.create_buffer_with_data(&BufferInitDescriptor {
            label: Some("bake_bvh_nodes"),
            contents: bytemuck::cast_slice(node_data),
            usage: BufferUsages::STORAGE | BufferUsages::COPY_DST,
        });
        let tri_data: &[BvhTriangle] = if chunk_job.bvh.tris.is_empty() {
            &[bytemuck::Zeroable::zeroed()]
        } else {
            &chunk_job.bvh.tris
        };
        let bvh_tris_buf = render_device.create_buffer_with_data(&BufferInitDescriptor {
            label: Some("bake_bvh_tris"),
            contents: bytemuck::cast_slice(tri_data),
            usage: BufferUsages::STORAGE | BufferUsages::COPY_DST,
        });

        // Upload the placed lights ONCE (dummy entry when there are none, so
        // the binding is always valid; `num_lights` is what gates the loop).
        let light_data: &[BakedLight] = if chunk_job.lights.is_empty() {
            &[bytemuck::Zeroable::zeroed()]
        } else {
            &chunk_job.lights
        };
        let lights_buf = render_device.create_buffer_with_data(&BufferInitDescriptor {
            label: Some("bake_lights"),
            contents: bytemuck::cast_slice(light_data),
            usage: BufferUsages::STORAGE | BufferUsages::COPY_DST,
        });

        // Build one bind group per strip, sharing the heightmap + BVH + lights + output buffers.
        for strip_uniforms in &chunk_job.strip_uniforms {
            let uniform_buf = render_device.create_buffer_with_data(&BufferInitDescriptor {
                label: Some("bake_uniforms"),
                contents: bytemuck::bytes_of(strip_uniforms),
                usage: BufferUsages::UNIFORM | BufferUsages::COPY_DST,
            });

            let bind_group = render_device.create_bind_group(
                Some("lightmap_bake_bindgroup"),
                &bind_group_layout,
                &BindGroupEntries::sequential((
                    heightmap_buf.as_entire_buffer_binding(),
                    uniform_buf.as_entire_buffer_binding(),
                    gpu_out.buffer.as_entire_buffer_binding(),
                    bvh_nodes_buf.as_entire_buffer_binding(),
                    lights_buf.as_entire_buffer_binding(),
                    bvh_tris_buf.as_entire_buffer_binding(),
                )),
            );

            in_flight.strips.push(BakeInFlightStrip {
                bind_group,
                resolution: chunk_job.grid.padded() as u32,
                strip_rows: chunk_job.strip_rows,
                _uniform_buf: uniform_buf,
            });
        }

        // Keep shared buffers alive until after dispatch.
        in_flight._shared.push(ChunkSharedBuffers {
            _heightmap_buf: heightmap_buf,
            _bvh_nodes_buf: bvh_nodes_buf,
            _bvh_tris_buf: bvh_tris_buf,
            _lights_buf: lights_buf,
        });
    }
}

/// Clear in-flight jobs after the render pass has run.
fn clear_in_flight_after_render(mut in_flight: ResMut<GpuBakeInFlight>) {
    in_flight.strips.clear();
    in_flight._shared.clear();
}

// ============================================================================
// Main-world: readback observer
// ============================================================================

/// Global observer fired when any GPU readback completes.
/// Ignores events for entities that don't have `BakeReadbackTag`.
fn handle_bake_complete(
    trigger: On<ReadbackComplete>,
    tags: Query<&BakeReadbackTag>,
    mut processed: ResMut<GpuBakeProcessed>,
    mut images: ResMut<Assets<Image>>,
    mut materials: ResMut<Assets<ExtendedMaterial<StandardMaterial, TerrainMaterial>>>,
    mut commands: Commands,
    mut queue: ResMut<GpuBakeQueue>,
    config: Res<BakeConfig>,
) {
    // ReadbackComplete.entity is the entity that had the Readback component.
    let entity = trigger.event().entity;

    // Guard: Readback fires every frame the component exists, so multiple results
    // can arrive before the deferred despawn is applied. Process each entity once.
    if !processed.0.insert(entity) {
        return;
    }

    let Ok(tag) = tags.get(entity) else {
        // Not one of our bake entities — ignore.
        return;
    };

    let raw_bytes: &[u8] = &trigger.data;
    let padded = tag.grid.padded();

    // The shader wrote one Rgba16Float texel (two packed f16 pairs) per entry
    // of the padded grid.
    let Some(raw) = LightmapBuffer::from_rgba16_bytes(padded, raw_bytes) else {
        error!(
            "GPU bake readback size mismatch for chunk ({}, {}): expected {} bytes, got {}",
            tag.chunk_x,
            tag.chunk_y,
            padded * padded * 8,
            raw_bytes.len()
        );
        commands.entity(entity).try_despawn();
        return;
    };

    // Blur to eliminate per-texel Monte Carlo variance before toon
    // quantization, then drop the margin that let the blur see past the
    // chunk edge (see `ChunkBakeGrid`).
    let lightmap = raw.blur(config.blur_radius).crop(tag.grid.margin);

    // A failed write is logged but the live material still gets the result.
    if let Err(e) = store_lightmap(&tag.dest, &lightmap) {
        error!(
            "GPU bake: failed to save lightmap for chunk ({}, {}): {e}",
            tag.chunk_x, tag.chunk_y,
        );
    }

    let new_handle = images.add(lightmap.into_image());

    if let Some(mut mat) = materials.get_mut(tag.material_handle) {
        mat.extension.lightmap = new_handle;
    }

    info!(
        "GPU bake complete for terrain chunk ({}, {})",
        tag.chunk_x, tag.chunk_y
    );

    // try_despawn silences the warning if GameEntity cleanup already despawned this entity
    // in the same frame (race between OnExit and ReadbackComplete).
    commands.entity(entity).try_despawn();

    // Decrement the in-flight counter. `top_up_gpu_bake` will dispatch the next
    // queued chunk on the following frame, keeping dispatches to one per frame.
    queue.in_flight = queue.in_flight.saturating_sub(1);

    if queue.pending.is_empty() && queue.in_flight == 0 {
        info!("GPU terrain bake: all chunks complete.");
    }
}

#[cfg(test)]
mod tests {
    use super::{BvhNode, BvhTriangle};
    use half::f16;

    const SHADER_SRC: &str = include_str!("../../../../assets/shaders/lightmap_bake.wgsl");

    /// Everything memcpy'd to the GPU must keep the WGSL-side layout.
    #[test]
    fn bvh_buffers_match_the_wgsl_structs() {
        assert_eq!(std::mem::size_of::<BvhNode>(), 32);
        assert_eq!(std::mem::size_of::<BvhTriangle>(), 48);
    }

    /// The uniform block is memcpy'd into a GPU uniform buffer, so its size
    /// must stay a multiple of 16 or the WGSL struct silently disagrees.
    #[test]
    fn bake_uniforms_are_vec4_aligned() {
        let size = std::mem::size_of::<super::BakeUniformsData>();
        assert_eq!(size % 16, 0, "BakeUniformsData is {size} bytes");
    }

    /// The bake shader must validate with *no* optional capabilities: it is
    /// plain WGSL (no naga_oil imports), and the GPU path has already failed
    /// once in the field on `pack2x16float` wanting `SHADER_FLOAT16_IN_FLOAT32`.
    #[test]
    fn compute_shader_validates_without_optional_capabilities() {
        let module = naga::front::wgsl::parse_str(SHADER_SRC).unwrap_or_else(|e| {
            panic!(
                "lightmap_bake.wgsl failed to parse: {}",
                e.emit_to_string(SHADER_SRC)
            )
        });
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::empty(),
        )
        .validate(&module)
        .unwrap_or_else(|e| panic!("lightmap_bake.wgsl failed validation: {e:?}"));
    }

    /// Line-for-line Rust transliteration of `f32_to_f16_bits` in
    /// lightmap_bake.wgsl. Keep the two in sync; this test is what proves the
    /// shader's hand-rolled conversion is bit-exact.
    fn f32_to_f16_bits(value: f32) -> u32 {
        let bits = value.to_bits();
        let sign = (bits >> 16) & 0x8000;
        let exp32 = ((bits >> 23) & 0xFF) as i32;
        let mant = bits & 0x7F_FFFF;

        if exp32 == 0xFF {
            return if mant != 0 {
                sign | 0x7E00
            } else {
                sign | 0x7C00
            };
        }

        let exp16 = exp32 - 127 + 15;
        if exp16 >= 31 {
            return sign | 0x7C00;
        }
        if exp16 <= 0 {
            if exp16 < -10 {
                return sign;
            }
            let full = mant | 0x80_0000;
            let shift = (14 - exp16) as u32;
            let mut sub = full >> shift;
            let rem = full & ((1u32 << shift) - 1);
            let halfway = 1u32 << (shift - 1);
            if rem > halfway || (rem == halfway && (sub & 1) == 1) {
                sub += 1;
            }
            return sign | sub;
        }

        let mut h = sign | ((exp16 as u32) << 10) | (mant >> 13);
        let rem = mant & 0x1FFF;
        if rem > 0x1000 || (rem == 0x1000 && (h & 1) == 1) {
            h += 1;
        }
        h
    }

    // The long literals below are exact binary values (1 + 1/1024, 2^-14,
    // 2^-24 …) chosen to sit on f16 mantissa and subnormal boundaries.
    // Spelling them out in full is the point: it documents which boundary
    // each one probes, so don't let clippy shorten them.
    #[allow(clippy::excessive_precision)]
    #[test]
    fn wgsl_f16_packing_matches_half_crate() {
        let mut values: Vec<f32> = vec![
            0.0,
            -0.0,
            1.0,
            -1.0,
            0.25,
            1.5,
            // Round-to-nearest-even at the mantissa boundary.
            1.000_976_562_5,
            1.000_488_281_25,
            1.001_464_843_75,
            2049.0,
            2051.0,
            // Largest finite half, and the first values that round to inf.
            65504.0,
            65520.0,
            65536.0,
            70000.0,
            // Normal/subnormal boundary and the smallest subnormals.
            6.103_515_625e-5,
            6.0e-5,
            5.960_464_477_539_063e-8,
            4.0e-8,
            2.980_232_238_769_531_3e-8,
            2.0e-8,
            1e-10,
            f32::INFINITY,
            f32::NEG_INFINITY,
        ];
        // Dense sweep over the radiance range lightmaps actually hold.
        let mut x = 0.0f32;
        while x < 24.0 {
            values.push(x);
            x += 0.007_31;
        }

        for v in values {
            let expected = f16::from_f32(v).to_bits() as u32;
            let got = f32_to_f16_bits(v);
            assert_eq!(got, expected, "value {v} ({:#010x})", v.to_bits());
        }

        let nan = f32_to_f16_bits(f32::NAN);
        assert_eq!(nan & 0x7C00, 0x7C00, "NaN keeps an all-ones exponent");
        assert_ne!(nan & 0x03FF, 0, "NaN keeps a non-zero mantissa");
    }
}
