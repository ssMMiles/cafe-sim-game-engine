//! GPU compute path for terrain lightmap baking.
//!
//! Triggered by `run_bake` in `mod.rs` when `BakeConfig::use_gpu` is `true`
//! (non-wasm only). The CPU path in `terrain.rs` remains available as fallback.
//!
//! ## Pipeline
//!
//! 1. **Main world** (`enqueue_terrain_bake_jobs`): For each loaded chunk build
//!    a `ShaderStorageBuffer` output buffer + metadata, push a `GpuBakeJob` into
//!    `GpuBakePending`, and spawn a readback entity with an observer.
//!
//! 2. **ExtractSchedule** (`extract_bake_jobs`): Drain jobs from main world into
//!    the render world's `GpuBakeExtracted`.
//!
//! 3. **PrepareBindGroups** (`prepare_gpu_bake_jobs`): Upload heightmap + uniform
//!    data as raw GPU buffers, look up the `GpuShaderStorageBuffer` for the output
//!    handle, build the bind group.
//!
//! 4. **Render graph node** (`BakeComputeNode`): Dispatch the compute shader.
//!
//! 5. **Observer on readback entity** (`handle_bake_complete`): Convert raw
//!    bytes → `Image`, save PNG, swap into the material.

use bevy::{
    asset::{Assets, RenderAssetUsages},
    mesh::{Mesh, Mesh3d},
    pbr::ExtendedMaterial,
    prelude::*,
    render::{
        ExtractSchedule, MainWorld, Render, RenderApp, RenderStartup, RenderSystems,
        gpu_readback::{Readback, ReadbackComplete},
        render_asset::RenderAssets,
        render_graph::{self, RenderGraph, RenderLabel},
        render_resource::{
            BindGroup, BindGroupEntries, BindGroupLayoutDescriptor, BindGroupLayoutEntries, Buffer,
            BufferUsages, CachedComputePipelineId, ComputePassDescriptor,
            ComputePipelineDescriptor, PipelineCache, ShaderStages, binding_types,
        },
        renderer::{RenderContext, RenderDevice},
        storage::{GpuShaderStorageBuffer, ShaderStorageBuffer},
    },
};
use std::path::PathBuf;

use super::BakeConfig;
use crate::game::world::{
    objects::PlacedObject,
    terrain::{
        HeightmapData,
        material::TerrainMaterial,
        storage::{TerrainChunkRegistry, TerrainWorld},
    },
};

const SHADER_PATH: &str = "shaders/lightmap_bake.wgsl";

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
    pub terrain_resolution: u32,
    pub ao_rays: u32,
    pub chunk_origin_x: f32,
    pub chunk_origin_z: f32,
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
    /// Number of hull triangles in the hull_tris storage buffer for this job.
    pub num_hull_tris: u32,
    /// Number of DDA steps for AO rays (shorter range than shadow/bounce).
    pub ao_steps: u32,
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
    pub resolution: u32,
    /// Number of rows each strip covers (used to size the Y workgroup dispatch).
    pub strip_rows: u32,
    /// One uniform set per strip, differing only in `row_offset`.
    pub strip_uniforms: Vec<BakeUniformsData>,
    /// Flat heightmap tile values covering chunk + ray-reach border (shared across strips).
    pub heightmap_bytes: Vec<f32>,
    /// Flat hull triangle data: 9 floats per triangle (shared across strips).
    pub hull_tri_bytes: Vec<f32>,
    /// Handle to the output `ShaderStorageBuffer` (pre-allocated, zeroed).
    pub out_handle: Handle<ShaderStorageBuffer>,
    /// Material to swap the lightmap into once the readback completes.
    pub material_handle: AssetId<ExtendedMaterial<StandardMaterial, TerrainMaterial>>,
    /// Absolute path to the world directory, used when saving the PNG.
    pub world_path: PathBuf,
}

/// Metadata stored on the readback entity so the observer knows what to do.
#[derive(Component)]
pub struct BakeReadbackTag {
    pub chunk_x: i32,
    pub chunk_y: i32,
    pub resolution: u32,
    pub material_handle: AssetId<ExtendedMaterial<StandardMaterial, TerrainMaterial>>,
    pub world_path: PathBuf,
}

/// One terrain chunk waiting to be dispatched (no GPU buffer allocated yet).
struct GpuBakeQueueEntry {
    chunk_x: i32,
    chunk_y: i32,
    resolution: u32,
    uniforms_data: BakeUniformsData,
    heightmap_bytes: Vec<f32>,
    /// Flat hull triangle data: 9 floats per triangle (v0.xyz, v1.xyz, v2.xyz).
    hull_tri_bytes: Vec<f32>,
    material_handle: AssetId<ExtendedMaterial<StandardMaterial, TerrainMaterial>>,
    world_path: PathBuf,
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
    _obj_tris_buf: Buffer,
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
// Render graph node
// ============================================================================

/// Label for the lightmap bake compute node in the render graph.
#[derive(Debug, Hash, PartialEq, Eq, Clone, RenderLabel)]
struct BakeComputeNodeLabel;

/// Render graph node that dispatches the lightmap compute pass.
#[derive(Default)]
struct BakeComputeNode;

impl render_graph::Node for BakeComputeNode {
    fn run(
        &self,
        _graph: &mut render_graph::RenderGraphContext,
        render_context: &mut RenderContext,
        world: &World,
    ) -> Result<(), render_graph::NodeRunError> {
        let Some(pipeline_res) = world.get_resource::<GpuBakePipeline>() else {
            return Ok(());
        };
        let pipeline_cache = world.resource::<PipelineCache>();
        let Some(compute_pipeline) = pipeline_cache.get_compute_pipeline(pipeline_res.pipeline_id)
        else {
            // Pipeline still compiling — jobs will retry next frame because
            // Readback polls until the entity is despawned.
            return Ok(());
        };

        let Some(in_flight) = world.get_resource::<GpuBakeInFlight>() else {
            return Ok(());
        };

        for strip in &in_flight.strips {
            let mut pass =
                render_context
                    .command_encoder()
                    .begin_compute_pass(&ComputePassDescriptor {
                        label: Some("lightmap_bake_pass"),
                        timestamp_writes: None,
                    });

            pass.set_pipeline(compute_pipeline);
            pass.set_bind_group(0, &strip.bind_group, &[]);

            let wg_x = (strip.resolution + 15) / 16;
            let wg_y = (strip.strip_rows + 15) / 16;
            pass.dispatch_workgroups(wg_x, wg_y, 1);
        }

        Ok(())
    }
}

// ============================================================================
// Plugin wiring (called from BakingPlugin::build)
// ============================================================================

/// Register all render-app resources and systems for the GPU baking path.
/// Called from `BakingPlugin::build` only on non-wasm targets.
pub fn add_gpu_baking_to_app(app: &mut App) {
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
        .init_resource::<GpuBakeExtracted>()
        .init_resource::<GpuBakeInFlight>()
        .add_systems(
            RenderStartup,
            (init_gpu_bake_pipeline, add_bake_render_graph_node),
        )
        .add_systems(ExtractSchedule, extract_bake_jobs)
        .add_systems(
            Render,
            (
                prepare_gpu_bake_jobs.in_set(RenderSystems::PrepareBindGroups),
                clear_in_flight_after_render.in_set(RenderSystems::Cleanup),
            ),
        );
}

fn add_bake_render_graph_node(mut render_graph: ResMut<RenderGraph>) {
    render_graph.add_node(BakeComputeNodeLabel, BakeComputeNode);
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
#[allow(clippy::too_many_arguments)]
pub fn enqueue_terrain_bake_jobs(
    heightmap: &HeightmapData,
    terrain_world: &TerrainWorld,
    config: &BakeConfig,
    chunk_registry: &TerrainChunkRegistry,
    chunk_query: &Query<(
        &crate::game::world::terrain::TerrainChunk,
        &MeshMaterial3d<ExtendedMaterial<StandardMaterial, TerrainMaterial>>,
    )>,
    processed: &mut GpuBakeProcessed,
    queue: &mut GpuBakeQueue,
    sun_dir: Vec3,
    placed_query: &Query<(Entity, &PlacedObject, &GlobalTransform)>,
    children_query: &Query<&Children>,
    mesh_gt_query: &Query<(&Mesh3d, &GlobalTransform)>,
    meshes: &Assets<Mesh>,
) {
    use super::raycast::BakeHeightCache;
    use crate::game::world::terrain::TILE_SIZE;
    use crate::game::world::terrain::constants::CHUNK_SIZE;

    // Clear processed set so results from a prior bake don't block this one,
    // and discard any leftover entries from an interrupted previous bake.
    processed.0.clear();
    queue.pending.clear();

    let res = config.terrain_resolution;
    let chunk_world_size = CHUNK_SIZE as f32 * TILE_SIZE.0;
    let ray_reach = config.ao_radius * 4.0;

    for (chunk, material_handle) in chunk_query.iter() {
        if !chunk_registry
            .loaded_chunks
            .contains_key(&(chunk.chunk_x, chunk.chunk_y))
        {
            continue;
        }

        let chunk_origin_x = chunk.chunk_x as f32 * chunk_world_size;
        let chunk_origin_z = chunk.chunk_y as f32 * chunk_world_size;

        let cache = BakeHeightCache::extract(
            heightmap,
            chunk_origin_x - ray_reach,
            chunk_origin_z - ray_reach,
            chunk_origin_x + chunk_world_size + ray_reach,
            chunk_origin_z + chunk_world_size + ray_reach,
        );

        // Collect convex hull shadow proxies for placed objects near this chunk.
        let hull_tris = super::raycast::collect_shadow_hulls(
            placed_query,
            children_query,
            mesh_gt_query,
            meshes,
            chunk_origin_x - ray_reach,
            chunk_origin_x + chunk_world_size + ray_reach,
            chunk_origin_z - ray_reach,
            chunk_origin_z + chunk_world_size + ray_reach,
            None,
        );

        let hull_tri_bytes: Vec<f32> = hull_tris
            .iter()
            .flat_map(|tri| {
                [
                    tri[0].x, tri[0].y, tri[0].z, tri[1].x, tri[1].y, tri[1].z, tri[2].x, tri[2].y,
                    tri[2].z,
                ]
            })
            .collect();
        let num_hull_tris = hull_tris.len() as u32;

        let uniforms_data = BakeUniformsData {
            cache_min_x: cache.cache_min_x(),
            cache_min_z: cache.cache_min_z(),
            tile_size_x: cache.tile_size.0,
            tile_size_z: cache.tile_size.1,
            cache_stride: cache.stride() as u32,
            cache_depth: cache.depth() as u32,
            terrain_resolution: res,
            ao_rays: config.ao_rays,
            chunk_origin_x,
            chunk_origin_z,
            chunk_world_size,
            ao_radius: config.ao_radius,
            sun_dir_x: sun_dir.x,
            sun_dir_y: sun_dir.y,
            sun_dir_z: sun_dir.z,
            shadow_steps: config.shadow_steps,
            cache_tile_min_x: cache.tile_min_x(),
            cache_tile_min_z: cache.tile_min_z(),
            bounce_rays: config.bounce_rays,
            bounce_strength: config.bounce_strength,
            terrain_albedo: config.terrain_albedo,
            bounce_count: config.bounce_count,
            row_offset: 0,
            num_hull_tris,
            ao_steps: config.ao_steps,
        };

        queue.pending.push_back(GpuBakeQueueEntry {
            chunk_x: chunk.chunk_x,
            chunk_y: chunk.chunk_y,
            resolution: res,
            uniforms_data,
            heightmap_bytes: cache.into_data(),
            hull_tri_bytes,
            material_handle: material_handle.id(),
            world_path: terrain_world.world_path.clone(),
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
    buffers: &mut Assets<ShaderStorageBuffer>,
    pending: &mut GpuBakePending,
) {
    let texel_count = (entry.resolution * entry.resolution) as usize;
    let byte_count = texel_count * std::mem::size_of::<u32>();

    // COPY_SRC is required by GpuReadbackPlugin so it can copy results to CPU.
    let mut out_buf = ShaderStorageBuffer::with_size(byte_count, RenderAssetUsages::RENDER_WORLD);
    out_buf.buffer_description.usage =
        BufferUsages::STORAGE | BufferUsages::COPY_SRC | BufferUsages::COPY_DST;
    let out_handle = buffers.add(out_buf);

    let total_strips = entry.resolution.div_ceil(strip_rows);

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
        resolution: entry.resolution,
        strip_rows,
        strip_uniforms,
        heightmap_bytes: entry.heightmap_bytes,
        hull_tri_bytes: entry.hull_tri_bytes,
        out_handle,
        material_handle: entry.material_handle,
        world_path: entry.world_path,
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
    mut buffers: ResMut<Assets<ShaderStorageBuffer>>,
    mut pending: ResMut<GpuBakePending>,
    mut commands: Commands,
) {
    while queue.in_flight < config.gpu_parallel_chunks && !queue.pending.is_empty() {
        let entry = queue.pending.pop_front().unwrap();
        let remaining = queue.pending.len();
        let total_strips = entry.resolution.div_ceil(config.dispatch_strip_rows);
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
                resolution: chunk_job.resolution,
                material_handle: chunk_job.material_handle,
                world_path: chunk_job.world_path.clone(),
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
    extracted.jobs.extend(pending.jobs.drain(..));
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
                // binding 3 – hull triangles read-only storage (no min-size needed)
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
    gpu_buffers: Res<RenderAssets<GpuShaderStorageBuffer>>,
    mut extracted: ResMut<GpuBakeExtracted>,
    mut in_flight: ResMut<GpuBakeInFlight>,
) {
    use bevy::render::render_resource::BufferInitDescriptor;

    let Some(pipeline) = pipeline else {
        return;
    };

    let bind_group_layout = pipeline_cache.get_bind_group_layout(&pipeline.layout);

    for chunk_job in extracted.jobs.drain(..) {
        // Retrieve the GPU-side output buffer prepared from the ShaderStorageBuffer asset.
        let Some(gpu_out) = gpu_buffers.get(&chunk_job.out_handle) else {
            warn!(
                "GPU bake: output ShaderStorageBuffer not ready for chunk ({}, {}), skipping",
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

        // Upload hull triangles ONCE (use a 1-float dummy if empty to satisfy the binding).
        let hull_data: &[f32] = if chunk_job.hull_tri_bytes.is_empty() {
            &[0.0f32]
        } else {
            &chunk_job.hull_tri_bytes
        };
        let obj_tris_buf = render_device.create_buffer_with_data(&BufferInitDescriptor {
            label: Some("bake_hull_tris"),
            contents: bytemuck::cast_slice(hull_data),
            usage: BufferUsages::STORAGE | BufferUsages::COPY_DST,
        });

        // Build one bind group per strip, sharing the heightmap + hull + output buffers.
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
                    obj_tris_buf.as_entire_buffer_binding(),
                )),
            );

            in_flight.strips.push(BakeInFlightStrip {
                bind_group,
                resolution: chunk_job.resolution,
                strip_rows: chunk_job.strip_rows,
                _uniform_buf: uniform_buf,
            });
        }

        // Keep shared buffers alive until after dispatch.
        in_flight._shared.push(ChunkSharedBuffers {
            _heightmap_buf: heightmap_buf,
            _obj_tris_buf: obj_tris_buf,
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
    let res = tag.resolution as usize;

    // Each output entry is a u32 in [0, 255]; take the low byte as the R8Unorm value.
    // The buffer was written as u32 values (not u8), so cast_slice to get u32s first.
    let u32_values: &[u32] = bytemuck::cast_slice(raw_bytes);
    let raw_pixels: Vec<u8> = u32_values.iter().map(|&v| (v & 0xFF) as u8).collect();

    if raw_pixels.len() != res * res {
        error!(
            "GPU bake readback size mismatch for chunk ({}, {}): expected {} texels, got {}",
            tag.chunk_x,
            tag.chunk_y,
            res * res,
            raw_pixels.len()
        );
        commands.entity(entity).try_despawn();
        return;
    }

    // Blur to eliminate per-texel Monte Carlo variance before toon quantization.
    let pixels = super::terrain::blur_lightmap(&raw_pixels, res, config.blur_radius);

    // Save PNG to disk.
    if let Err(e) =
        super::terrain::save_lightmap_png(&pixels, res, &tag.world_path, tag.chunk_x, tag.chunk_y)
    {
        error!(
            "GPU bake: failed to save PNG for chunk ({}, {}): {e}",
            tag.chunk_x, tag.chunk_y
        );
    }

    // Insert GPU image and swap into the material.
    let image = Image::new(
        bevy::render::render_resource::Extent3d {
            width: res as u32,
            height: res as u32,
            depth_or_array_layers: 1,
        },
        bevy::render::render_resource::TextureDimension::D2,
        pixels,
        bevy::render::render_resource::TextureFormat::R8Unorm,
        RenderAssetUsages::RENDER_WORLD,
    );
    let new_handle = images.add(image);

    if let Some(mat) = materials.get_mut(tag.material_handle) {
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
