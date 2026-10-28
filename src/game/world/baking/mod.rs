//! Lightmap baking system (CPU + optional GPU compute).
//!
//! Triggered by Shift+B. Iterates all loaded terrain chunks and placed mesh
//! instances, computes per-texel HDR radiance (sun shadow rays, ambient × AO,
//! terrain bounce), writes EXR lightmaps to disk, and swaps them into the
//! live GPU materials. All lighting is baked — the runtime shaders only
//! sample the result (see `lightmap.rs` for the model and units).
//!
//! Module layout:
//! - `mod.rs`         — `BakingPlugin`, `BakeConfig` resource, trigger system
//! - `bvh.rs`         — triangle BVH over static scenery, shared by CPU paths and uploaded to the GPU
//! - `lightmap.rs`    — shared `BakeLighting` model, `LightmapBuffer`, EXR I/O, paths
//! - `object_lightmaps.rs` — per-sub-mesh object lightmap naming + loader system
//! - `raycast.rs`     — CPU shadow ray and AO functions against HeightmapData
//! - `terrain.rs`     — terrain chunk lightmap bake logic (CPU path)
//! - `uv_unwrap.rs`   — non-overlapping per-triangle lightmap UV2 + `LightmapUvCache`
//! - `objects.rs`     — placed mesh lightmap bake logic (UV rasterization)
//! - `gpu.rs`         — GPU compute path for terrain baking (any adapter with compute)

pub mod bvh;
pub mod gpu;
pub mod lightmap;
pub mod object_lightmaps;
pub mod objects;
pub mod raycast;
pub mod terrain;
pub mod uv_unwrap;

use avian3d::prelude::RigidBody;
use bevy::{
    ecs::system::SystemParam,
    pbr::ExtendedMaterial,
    prelude::*,
    tasks::{AsyncComputeTaskPool, Task, futures::check_ready},
};
use std::sync::Arc;

use crate::game::{
    WorldFocus,
    gfx::toon_material::ToonMaterial,
    world::{
        objects::{CHUNK_WORLD_SIZE, PlacedObject, world_pos_to_chunk},
        terrain::{
            HeightmapData, TerrainChunk,
            material::TerrainMaterial,
            storage::{TerrainChunkRegistry, TerrainWorld},
        },
    },
};
use bvh::{TriangleBvh, collect_occluder_triangles};
use lightmap::{BakeLighting, BakedLight};
use objects::ObjectBakeResult;
use uv_unwrap::LightmapUvCache;

/// The object bake currently running on `AsyncComputeTaskPool`, if any.
///
/// Object baking is seconds of ray casting. Running it inline in `Update`
/// froze the whole app for the duration of every Shift+B, so it moved to the
/// task pool — the same thing chunk meshing (`streaming.rs`) and world saves
/// (`terrain/save.rs`) already do. `apply_object_bake` polls this.
///
/// One task for the whole pass rather than one per sub-mesh: the jobs are
/// gathered together, the results are applied together, and a single slot
/// makes "a bake is already running" trivial to detect.
#[derive(Resource, Default)]
pub struct ObjectBakeTask(Option<Task<Vec<ObjectBakeResult>>>);

// ============================================================================
// Configuration
// ============================================================================

/// Controls the quality and extent of the lightmap bake pass.
#[derive(Resource, Debug, Clone)]
pub struct BakeConfig {
    /// Lightmap resolution for terrain chunks (pixels per side).
    pub terrain_resolution: u32,
    /// Upper bound on a placed sub-mesh's lightmap resolution (pixels per
    /// side). The actual size scales with triangle count — see
    /// [`uv_unwrap::lightmap_resolution`].
    pub mesh_resolution_max: u32,
    /// Lightmap texels per world metre across a placed mesh — the object-side
    /// counterpart of `terrain_resolution / CHUNK_WORLD_SIZE` (16 texels/m at
    /// the defaults). Every triangle gets its own atlas cell, and this is what
    /// sizes that cell, so a big flat quad gets a big cell instead of the
    /// same one a thumbnail-sized triangle would get.
    pub mesh_texels_per_metre: f32,
    /// Floor on an atlas cell's width in texels, for dense meshes where
    /// per-triangle world extent is tiny and `mesh_texels_per_metre` would
    /// round down to nothing.
    pub mesh_texels_per_triangle: u32,
    /// How far lit texels are flooded past the edge of each triangle, so
    /// bilinear filtering there cannot sample an empty (black) texel.
    ///
    /// A small constant, and deliberately **not** scaled to the width of the
    /// atlas gutter. The gutter is dead space whose job is to *separate*
    /// triangles so one's filtering can never reach another's texels; nothing
    /// ever samples most of it. The shader only samples UVs inside a triangle,
    /// and a bilinear tap reaches at most one texel beyond the sample point,
    /// so one pass suffices and the default is already generous.
    ///
    /// Sizing this from the cell instead cost 94 passes over a 1024² atlas for
    /// a two-triangle mesh — ~900M neighbour tests and a gigabyte of copying,
    /// to fill texels no sampler can reach.
    pub dilate_passes: u32,
    /// Number of AO hemisphere rays per texel.
    pub ao_rays: u32,
    /// AO hemisphere radius in world units.
    pub ao_radius: f32,
    /// Number of DDA steps for shadow and bounce rays (marches `ao_radius × 4`).
    pub shadow_steps: u32,
    /// Number of DDA steps for AO rays (marches `ao_radius`).
    /// Defaults to half of `shadow_steps` because AO covers 4× less distance.
    pub ao_steps: u32,
    /// Upper bound on shadow-ray steps toward a placed point/spot light.
    /// The actual count matches the sun ray's sampling density over the
    /// (usually much shorter) distance to the light — see
    /// [`BakeConfig::light_shadow_steps`].
    pub max_light_shadow_steps: u32,
    /// Prefer the GPU compute path for terrain baking. Honoured on any
    /// adapter that reports compute support, including WebGPU in the browser;
    /// falls back to the CPU rayon path when it doesn't (WebGL2) or when this
    /// is `false`. See `gpu::GpuBakeSupported`.
    pub use_gpu: bool,
    /// How many terrain chunks to bake in parallel on the GPU.
    /// Higher values increase throughput but use more VRAM
    /// (each in-flight chunk needs `resolution² × 4` bytes).
    /// Set to 1 for the most conservative VRAM usage.
    pub gpu_parallel_chunks: u32,
    /// Number of hemisphere rays for indirect lighting. Set to 0 to disable.
    pub bounce_rays: u32,
    /// Strength of the indirect bounce contribution. 0.0 = no bounce, 1.0 = natural.
    pub bounce_strength: f32,
    /// Diffuse reflectance of the terrain surface used in bounce calculations.
    pub terrain_albedo: f32,
    /// Number of light bounces to simulate per primary ray (1 = single bounce).
    pub bounce_count: u32,
    /// How many times coarser than the lightmap the **CPU** terrain bake
    /// samples ambient occlusion and indirect bounce, bilinearly
    /// interpolating between samples. 1 disables the optimisation.
    ///
    /// Those two terms are ~95% of the CPU bake's ray cost and vary over
    /// metres (AO integrates a 20 m hemisphere, bounce an 80 m one), while
    /// `blur_radius` already smears the finished lightmap over ~25 cm at the
    /// default resolution — so per-texel AO detail is computed and then
    /// thrown away. 4 cuts their cost 16× for a difference the blur hides.
    ///
    /// The sun and local-light shadows are unaffected and stay per-texel:
    /// they are the sharp terms the toon bands key off.
    ///
    /// Only the CPU path honours this, so native GPU bakes and CPU bakes of
    /// the same world differ very slightly in AO. Set to 1 for parity.
    pub cpu_lowfreq_downsample: u32,
    /// Box blur radius applied to the finished lightmap (in texels) before saving.
    /// Eliminates per-texel Monte Carlo variance so the toon quantizer sees smooth
    /// gradients rather than noisy values that produce "lines of light" at band edges.
    /// Set to 0 to skip the blur pass entirely.
    pub blur_radius: u32,
    /// Number of lightmap rows per GPU dispatch strip.
    ///
    /// Splits each chunk's bake into several dispatches so no single GPU
    /// submission processes a whole high-resolution texture at once,
    /// preventing TDR timeouts. 64 rows is safe even at 4096px.
    pub dispatch_strip_rows: u32,
    /// Only bake terrain chunks within this Chebyshev chunk radius of the
    /// player, plus the placed objects standing on them. `None` bakes every
    /// loaded chunk — 81 at the default streaming radius, which takes
    /// minutes. `Some(1)` (a 3×3 block) is the development default.
    pub bake_radius: Option<u32>,
}

impl Default for BakeConfig {
    fn default() -> Self {
        Self {
            // 1024 over a 63.75 m chunk is 16 texels/m — ample for 5-band
            // toon shadows. Lightmaps are Rgba16Float (8 bytes/texel), so
            // this is 8 MB of VRAM per chunk; 4096 would be 128 MB each and
            // ~10 GB for a loaded streaming ring.
            terrain_resolution: 1024,
            // 512² Rgba16Float is 2 MB of VRAM per sub-mesh. Raise to 1024
            // for sharper shadows on large flat scenery (the cafe walls want
            // it) at 4× the texels to bake — the object rasteriser is still
            // single-threaded, so that is a real wall-clock cost.
            mesh_resolution_max: 2048,
            // Half the terrain's 16 texels/m. Objects are small and numerous,
            // the atlas is capped, and the toon quantiser only needs enough
            // density that a shadow edge doesn't land on a band boundary as a
            // staircase.
            mesh_texels_per_metre: 8.0,
            mesh_texels_per_triangle: 8,
            dilate_passes: 3,
            ao_rays: 16,
            ao_radius: 20.0,
            shadow_steps: 64,
            ao_steps: 32,
            max_light_shadow_steps: 48,
            use_gpu: true,
            gpu_parallel_chunks: 2,
            bounce_rays: 8,
            bounce_strength: 1.0,
            terrain_albedo: 0.5,
            bounce_count: 1,
            cpu_lowfreq_downsample: 4,
            blur_radius: 4,
            dispatch_strip_rows: 64,
            bake_radius: Some(1),
        }
    }
}

impl BakeConfig {
    /// Shadow-ray steps for a local light `dist` away.
    ///
    /// Matched to the sun ray's world-space sampling density so a light and
    /// the sun resolve the same size of occluder, then clamped: nearby lights
    /// don't waste steps, distant ones stay bounded.
    pub fn light_shadow_steps(&self, dist: f32) -> u32 {
        let step_len = (self.ao_radius * 4.0) / self.shadow_steps.max(1) as f32;
        ((dist / step_len.max(1e-4)).ceil() as u32).clamp(4, self.max_light_shadow_steps.max(4))
    }
}

/// Which part of the loaded world a bake pass covers: a Chebyshev block of
/// chunks around the player, or everything loaded.
#[derive(Clone, Copy, Debug)]
pub struct BakeRegion {
    center: (i32, i32),
    radius: Option<u32>,
}

impl BakeRegion {
    pub fn around(player_pos: Vec3, radius: Option<u32>) -> Self {
        Self {
            center: world_pos_to_chunk(player_pos.x, player_pos.z),
            radius,
        }
    }

    pub fn contains_chunk(&self, cx: i32, cy: i32) -> bool {
        match self.radius {
            None => true,
            Some(r) => {
                (cx - self.center.0)
                    .unsigned_abs()
                    .max((cy - self.center.1).unsigned_abs())
                    <= r
            }
        }
    }

    /// Whether a world position stands on a chunk inside the region.
    pub fn contains_point(&self, pos: Vec3) -> bool {
        let (cx, cy) = world_pos_to_chunk(pos.x, pos.z);
        self.contains_chunk(cx, cy)
    }

    /// World-space XZ extent of the region's chunks, or `None` when the
    /// region is "everything loaded".
    pub fn world_bounds_xz(&self) -> Option<(Vec2, Vec2)> {
        let r = self.radius? as i32;
        let min = Vec2::new((self.center.0 - r) as f32, (self.center.1 - r) as f32);
        let max = Vec2::new(
            (self.center.0 + r + 1) as f32,
            (self.center.1 + r + 1) as f32,
        );
        Some((min * CHUNK_WORLD_SIZE, max * CHUNK_WORLD_SIZE))
    }
}

// ============================================================================
// Messages
// ============================================================================

/// Sent when a bake should start.
#[derive(Message)]
pub struct BakeMessage;

type ToonMat = ExtendedMaterial<StandardMaterial, ToonMaterial>;
type OccluderMeshItem = (Entity, &'static Mesh3d, &'static GlobalTransform);

/// The world queries every bake path walks: terrain chunks, placed objects,
/// the meshes under them, and the static scenery that casts shadows.
#[derive(SystemParam)]
pub struct BakeSceneQueries<'w, 's> {
    pub terrain_chunks: Query<
        'w,
        's,
        (
            &'static TerrainChunk,
            &'static MeshMaterial3d<ExtendedMaterial<StandardMaterial, TerrainMaterial>>,
        ),
    >,
    pub placed: Query<'w, 's, (Entity, &'static PlacedObject, &'static GlobalTransform)>,
    pub toon_meshes: object_lightmaps::ToonMeshQuery<'w, 's>,
    pub children: Query<'w, 's, &'static Children>,
    /// Shadow-caster candidates: every toon-shaded mesh in the world.
    /// `collect_occluder_triangles` keeps only the static ones.
    pub occluder_meshes: Query<'w, 's, OccluderMeshItem, With<MeshMaterial3d<ToonMat>>>,
    pub parents: Query<'w, 's, &'static ChildOf>,
    /// What marks a subtree as static scenery for shadow casting.
    pub scenery: Query<'w, 's, (Option<&'static PlacedObject>, Option<&'static RigidBody>)>,
}

// ============================================================================
// Components
// ============================================================================

/// Marker component for the light entity whose direction is used as the sun
/// during the lightmap bake pass. Place this on whichever entity you want to
/// drive baked lighting — it does not need to be a `DirectionalLight`.
#[derive(Component)]
pub struct BakedSun;

// ============================================================================
// Resources
// ============================================================================

/// Written by `run_bake` to pass the resolved lighting and region to the GPU
/// dispatch system that runs afterwards. Cleared by
/// `dispatch_gpu_terrain_bake` each frame.
///
/// `Some(..)` means a GPU terrain bake should be dispatched this frame.
/// `None` means no bake was triggered this frame.
#[derive(Resource, Default)]
struct GpuBakeTrigger(Option<(BakeLighting, BakeRegion, Arc<TriangleBvh>)>);

// ============================================================================
// Plugin
// ============================================================================

pub struct BakingPlugin;

impl Plugin for BakingPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<BakeConfig>();
        app.init_resource::<LightmapUvCache>();
        app.init_resource::<ObjectBakeTask>();
        app.add_message::<BakeMessage>();

        app.init_resource::<GpuBakeTrigger>();
        gpu::add_gpu_baking_to_app(app);

        use crate::game::world::WorldSet;
        app.add_systems(Update, run_bake.in_set(WorldSet::InGame));

        // Picks up whatever `run_bake` handed to the task pool. Unconditional
        // — the object bake is the CPU path on every platform.
        app.add_systems(
            Update,
            apply_object_bake.after(run_bake).in_set(WorldSet::InGame),
        );
        // Leaving the world abandons any bake still running: dropping the
        // `Task` cancels it, rather than burning cores on lightmaps for a
        // level nobody is in any more.
        app.add_systems(OnExit(crate::AppState::InGame), cancel_object_bake);

        // Saved object lightmaps are applied once each placed object's scene
        // has toon materials.
        #[cfg(not(target_arch = "wasm32"))]
        app.add_systems(
            Update,
            object_lightmaps::apply_object_lightmaps.in_set(WorldSet::InGame),
        );
        // On wasm the read is an async OPFS fetch, so it splits in two: kick
        // off, then apply whatever has landed.
        #[cfg(all(target_arch = "wasm32", feature = "opfs"))]
        {
            app.init_resource::<object_lightmaps::PendingObjectLightmaps>();
            app.add_systems(
                Update,
                (
                    object_lightmaps::fetch_object_lightmaps,
                    object_lightmaps::apply_object_lightmaps,
                )
                    .chain()
                    .in_set(WorldSet::InGame),
            );
        }

        // GPU dispatch runs after run_bake has written GpuBakeTrigger.
        app.add_systems(
            Update,
            dispatch_gpu_terrain_bake
                .after(run_bake)
                .in_set(WorldSet::InGame),
        );
    }
}

// ============================================================================
// Systems
// ============================================================================

/// Respond to `BakeMessage` and run CPU bake passes.
///
/// Handles placed-object baking (always CPU) and terrain baking when the GPU
/// path is unavailable or switched off. Otherwise writes `GpuBakeTrigger` so
/// `dispatch_gpu_terrain_bake` can do the GPU work in the same frame.
fn run_bake(
    mut bake_messages: MessageReader<BakeMessage>,
    mut commands: Commands,
    config: Res<BakeConfig>,
    focus: Res<WorldFocus>,
    heightmap: Option<Res<HeightmapData>>,
    terrain_world: Option<Res<TerrainWorld>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut images: ResMut<Assets<Image>>,
    mut terrain_materials: ResMut<Assets<ExtendedMaterial<StandardMaterial, TerrainMaterial>>>,
    mut toon_materials: ResMut<Assets<ExtendedMaterial<StandardMaterial, ToonMaterial>>>,
    mut uv_cache: ResMut<LightmapUvCache>,
    mut object_bake: ResMut<ObjectBakeTask>,
    scene: BakeSceneQueries,
    baked_sun_query: Query<&GlobalTransform, With<BakedSun>>,
    mut gpu_trigger: ResMut<GpuBakeTrigger>,
    gpu_supported: Res<gpu::GpuBakeSupported>,
) {
    if bake_messages.read().count() == 0 {
        return;
    }

    let (Some(heightmap), Some(terrain_world)) = (heightmap, terrain_world) else {
        warn!("Bake skipped: world not loaded");
        return;
    };

    let sun_dir = baked_sun_query
        .iter()
        .next()
        .map(|t| -t.forward().as_vec3())
        .unwrap_or(Vec3::new(-0.3, 0.8, -0.5).normalize());
    // Resolve the lighting once so every bake path shades identically.
    // Lights outside the bake region are included: one just beyond the edge
    // still shines onto surfaces inside it.
    let lights: Vec<BakedLight> = scene
        .placed
        .iter()
        .filter_map(|(_, obj, gt)| BakedLight::from_placed(&obj.kind, gt))
        .collect();
    let light_count = lights.len();
    let lighting = BakeLighting::new(&terrain_world.metadata.environment, sun_dir, lights);
    let region = BakeRegion::around(focus.0, config.bake_radius);

    // One BVH over every static occluder a ray from inside the region could
    // reach: the sun ray's reach, or the longest light range if that's more.
    let reach = lighting
        .lights
        .iter()
        .map(|l| l.range)
        .fold(config.ao_radius * 4.0, f32::max);
    let occluder_bounds = region
        .world_bounds_xz()
        .map(|(min, max)| (min - Vec2::splat(reach), max + Vec2::splat(reach)));
    let triangles = collect_occluder_triangles(
        &scene.occluder_meshes,
        &scene.parents,
        &scene.scenery,
        &meshes,
        occluder_bounds,
    );
    let triangle_count = triangles.len();
    let bvh = Arc::new(TriangleBvh::build(triangles));

    match config.bake_radius {
        Some(r) => info!(
            "Starting lightmap bake pass: {}×{} chunks around the player, {light_count} placed light(s), \
             {triangle_count} occluder triangles in {} BVH nodes…",
            2 * r + 1,
            2 * r + 1,
            bvh.nodes.len()
        ),
        None => info!(
            "Starting lightmap bake pass: all loaded chunks, {light_count} placed light(s), \
             {triangle_count} occluder triangles in {} BVH nodes…",
            bvh.nodes.len()
        ),
    }

    // ── Phase 1: Terrain ────────────────────────────────────────────────────
    // `use_gpu` is the preference; `GpuBakeSupported` is whether the adapter
    // can actually honour it. On the web that is the difference between a
    // WebGPU context and a WebGL2 fallback, and it is only known at runtime.
    if config.use_gpu && gpu_supported.get() {
        // Signal the GPU dispatch system to handle terrain baking this frame.
        // Cloned: the object bake below shades with the same lighting, and
        // the GPU dispatch runs in a later system.
        gpu_trigger.0 = Some((lighting.clone(), region, bvh.clone()));
    } else {
        terrain::bake_terrain_lightmaps(
            &heightmap,
            &terrain_world,
            &config,
            &region,
            &scene.terrain_chunks,
            &mut terrain_materials,
            &mut images,
            &lighting,
            &bvh,
        );
    }

    #[cfg(target_arch = "wasm32")]
    terrain::bake_terrain_lightmaps(
        &heightmap,
        &terrain_world,
        &config,
        &region,
        &scene.terrain_chunks,
        &mut terrain_materials,
        &mut images,
        &lighting,
        &bvh,
    );

    // ── Phase 2: Placed objects (CPU, on the task pool) ─────────────────────
    //
    // Gathering has to happen here — it unwraps meshes, adds material assets
    // and reads the heightmap resource — but it is cheap. Everything
    // expensive goes to `AsyncComputeTaskPool` so Shift+B doesn't freeze the
    // frame; `apply_object_bake` picks the results up when they land.
    if object_bake.0.is_some() {
        warn!("Object bake already running — terrain re-baked, objects skipped");
        return;
    }

    let jobs = objects::collect_object_bake_jobs(
        &mut commands,
        &heightmap,
        &terrain_world,
        &config,
        &region,
        &scene.placed,
        &scene.toon_meshes,
        &scene.children,
        &mut meshes,
        &mut toon_materials,
        &mut uv_cache,
    );

    if jobs.is_empty() {
        return;
    }

    let job_count = jobs.len();
    let task_config = config.clone();
    let task_lighting = lighting.clone();
    let task_bvh = bvh.clone();
    object_bake.0 = Some(AsyncComputeTaskPool::get().spawn(async move {
        objects::run_object_bake_jobs(jobs, &task_config, &task_lighting, &task_bvh)
    }));
    info!("Object bake dispatched: {job_count} sub-mesh(es) on the task pool");
}

/// Hand finished object lightmaps back to their materials.
///
/// Separate from `run_bake` because the bake runs off-thread: the task owns
/// plain data, and only here can it be turned into an `Image` asset and
/// bound. The EXR was already written inside the task.
fn apply_object_bake(
    mut object_bake: ResMut<ObjectBakeTask>,
    mut images: ResMut<Assets<Image>>,
    mut toon_materials: ResMut<Assets<ExtendedMaterial<StandardMaterial, ToonMaterial>>>,
) {
    let Some(task) = object_bake.0.as_mut() else {
        return;
    };
    let Some(results) = check_ready(task) else {
        return;
    };
    object_bake.0 = None;

    let count = results.len();
    for result in results {
        // A material can be gone if the object was deleted mid-bake. The EXR
        // is already on disk, so the next load picks it up regardless.
        if let Some(mut mat) = toon_materials.get_mut(result.material.id()) {
            mat.extension.lightmap = images.add(result.image);
        }
    }
    info!("Object bake complete: {count} sub-mesh lightmap(s) applied");
}

/// Drop any in-flight bake when the world unloads, cancelling it.
fn cancel_object_bake(mut object_bake: ResMut<ObjectBakeTask>) {
    if object_bake.0.take().is_some() {
        info!("World unloaded — cancelling the in-flight object bake");
    }
}

/// Dispatches GPU terrain bake jobs when `GpuBakeTrigger` is set.
///
/// Runs immediately after `run_bake`.
fn dispatch_gpu_terrain_bake(
    mut gpu_trigger: ResMut<GpuBakeTrigger>,
    config: Res<BakeConfig>,
    heightmap: Option<Res<HeightmapData>>,
    terrain_world: Option<Res<TerrainWorld>>,
    chunk_registry: Option<Res<TerrainChunkRegistry>>,
    mut terrain_materials: ResMut<Assets<ExtendedMaterial<StandardMaterial, TerrainMaterial>>>,
    mut images: ResMut<Assets<Image>>,
    scene: BakeSceneQueries,
    mut gpu_processed: ResMut<gpu::GpuBakeProcessed>,
    mut gpu_queue: ResMut<gpu::GpuBakeQueue>,
) {
    let Some((lighting, region, bvh)) = gpu_trigger.0.take() else {
        return;
    };

    let (Some(heightmap), Some(terrain_world)) = (heightmap, terrain_world) else {
        warn!("GPU terrain bake skipped: world not loaded");
        return;
    };

    match chunk_registry {
        Some(registry) => {
            gpu::enqueue_terrain_bake_jobs(
                &heightmap,
                &terrain_world,
                &config,
                &region,
                &registry,
                &scene.terrain_chunks,
                &mut gpu_processed,
                &mut gpu_queue,
                &lighting,
                &bvh,
            );
        }
        None => {
            warn!(
                "GPU terrain bake requested but TerrainChunkRegistry unavailable; \
                 falling back to CPU"
            );
            terrain::bake_terrain_lightmaps(
                &heightmap,
                &terrain_world,
                &config,
                &region,
                &scene.terrain_chunks,
                &mut terrain_materials,
                &mut images,
                &lighting,
                &bvh,
            );
        }
    }
}
