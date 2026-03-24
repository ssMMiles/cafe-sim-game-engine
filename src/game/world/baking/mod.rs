//! Lightmap baking system (CPU + optional GPU compute).
//!
//! Triggered by Shift+B. Iterates all loaded terrain chunks and
//! placed mesh instances, computes per-texel shadow + AO factors, writes PNG
//! lightmaps to disk, and swaps them into the live GPU materials.
//!
//! Module layout:
//! - `mod.rs`         — `BakingPlugin`, `BakeConfig` resource, trigger system
//! - `raycast.rs`     — CPU shadow ray and AO functions against HeightmapData
//! - `terrain.rs`     — terrain chunk lightmap bake logic (CPU path)
//! - `uv_unwrap.rs`   — xatlas UV2 generation for placed meshes
//! - `objects.rs`     — placed mesh lightmap bake logic (UV rasterization)
//! - `gpu.rs`         — GPU compute path for terrain baking (non-wasm only)

#[cfg(not(target_arch = "wasm32"))]
pub mod gpu;
pub mod objects;
pub mod raycast;
pub mod terrain;
pub mod uv_unwrap;

use bevy::{pbr::ExtendedMaterial, prelude::*};
use std::collections::HashSet;

use crate::game::{
    gfx::toon_material::ToonMaterial,
    world::{
        objects::PlacedObject,
        terrain::{
            HeightmapData, TerrainChunk,
            material::TerrainMaterial,
            storage::{TerrainChunkRegistry, TerrainWorld},
        },
    },
};

// ============================================================================
// Configuration
// ============================================================================

/// Controls the quality and extent of the lightmap bake pass.
#[derive(Resource, Debug, Clone)]
pub struct BakeConfig {
    /// Lightmap resolution for terrain chunks (pixels per side).
    pub terrain_resolution: u32,
    /// Lightmap resolution for placed mesh instances (pixels per side).
    pub mesh_resolution: u32,
    /// Number of AO hemisphere rays per texel.
    pub ao_rays: u32,
    /// AO hemisphere radius in world units.
    pub ao_radius: f32,
    /// Number of DDA steps for shadow and bounce rays (marches `ao_radius × 4`).
    pub shadow_steps: u32,
    /// Number of DDA steps for AO rays (marches `ao_radius`).
    /// Defaults to half of `shadow_steps` because AO covers 4× less distance.
    pub ao_steps: u32,
    /// Use the GPU compute path for terrain baking (non-wasm only).
    /// When `false` (or on wasm), falls back to the CPU rayon path.
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
    /// Box blur radius applied to the finished lightmap (in texels) before saving.
    /// Eliminates per-texel Monte Carlo variance so the toon quantizer sees smooth
    /// gradients rather than noisy values that produce "lines of light" at band edges.
    /// Set to 0 to skip the blur pass entirely.
    pub blur_radius: u32,
    /// Number of lightmap rows per GPU dispatch strip.
    ///
    /// Splits each chunk's bake across multiple frames so no single GPU submission
    /// processes the entire 4096×4096 texture at once, preventing TDR timeouts.
    /// 64 rows is safe for 4096px lightmaps (64 strips of 64 rows each).
    pub dispatch_strip_rows: u32,
}

impl Default for BakeConfig {
    fn default() -> Self {
        Self {
            terrain_resolution: 4096,
            mesh_resolution: 128,
            ao_rays: 16,
            ao_radius: 20.0,
            shadow_steps: 64,
            ao_steps: 32,
            use_gpu: true,
            gpu_parallel_chunks: 2,
            bounce_rays: 8,
            bounce_strength: 1.0,
            terrain_albedo: 0.5,
            bounce_count: 1,
            blur_radius: 4,
            dispatch_strip_rows: 64,
        }
    }
}

// ============================================================================
// Messages
// ============================================================================

/// Sent when a bake should start.
#[derive(Message)]
pub struct BakeMessage;

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

/// Tracks which mesh asset IDs have already had UV2 generated by xatlas so
/// the (expensive) unwrap step is never repeated for the same mesh.
#[derive(Resource, Default)]
pub struct LightmapUvCache(pub HashSet<bevy::asset::AssetId<Mesh>>);

/// Written by `run_bake` to pass the sun direction to the GPU dispatch system
/// that runs afterwards. Cleared by `dispatch_gpu_terrain_bake` each frame.
///
/// `Some(sun_dir)` means a GPU terrain bake should be dispatched this frame.
/// `None` means no bake was triggered this frame.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Resource, Default)]
struct GpuBakeTrigger(Option<Vec3>);

// ============================================================================
// Plugin
// ============================================================================

pub struct BakingPlugin;

impl Plugin for BakingPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<BakeConfig>();
        app.init_resource::<LightmapUvCache>();
        app.add_message::<BakeMessage>();

        #[cfg(not(target_arch = "wasm32"))]
        {
            app.init_resource::<GpuBakeTrigger>();
            gpu::add_gpu_baking_to_app(app);
        }

        use crate::game::world::WorldSet;
        app.add_systems(Update, trigger_bake.in_set(WorldSet::InGame));
        app.add_systems(Update, run_bake.in_set(WorldSet::InGame));

        // GPU dispatch runs after run_bake has written GpuBakeTrigger.
        #[cfg(not(target_arch = "wasm32"))]
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

/// Watch for Shift+B and send `BakeMessage`.
fn trigger_bake(input: Res<ButtonInput<KeyCode>>, mut bake_messages: MessageWriter<BakeMessage>) {
    if input.just_pressed(KeyCode::KeyB)
        && (input.pressed(KeyCode::ShiftLeft) || input.pressed(KeyCode::ShiftRight))
    {
        info!("Bake triggered (Shift+B)");
        bake_messages.write(BakeMessage);
    }
}

/// Respond to `BakeMessage` and run CPU bake passes.
///
/// Handles placed-object baking (always CPU) and terrain baking when `use_gpu`
/// is false or on wasm. On native with `use_gpu` enabled, writes `GpuBakeTrigger`
/// so `dispatch_gpu_terrain_bake` can do the GPU work in the same frame.
#[allow(clippy::too_many_arguments)]
fn run_bake(
    mut bake_messages: MessageReader<BakeMessage>,
    config: Res<BakeConfig>,
    heightmap: Option<Res<HeightmapData>>,
    terrain_world: Option<Res<TerrainWorld>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut images: ResMut<Assets<Image>>,
    mut terrain_materials: ResMut<Assets<ExtendedMaterial<StandardMaterial, TerrainMaterial>>>,
    mut toon_materials: ResMut<Assets<ExtendedMaterial<StandardMaterial, ToonMaterial>>>,
    mut uv_cache: ResMut<LightmapUvCache>,
    terrain_chunk_query: Query<(
        &TerrainChunk,
        &MeshMaterial3d<ExtendedMaterial<StandardMaterial, TerrainMaterial>>,
    )>,
    placed_query: Query<(Entity, &PlacedObject, &GlobalTransform)>,
    mesh_query: Query<(
        &Mesh3d,
        &MeshMaterial3d<ExtendedMaterial<StandardMaterial, ToonMaterial>>,
    )>,
    mesh_gt_query: Query<(&Mesh3d, &GlobalTransform)>,
    children_query: Query<&Children>,
    baked_sun_query: Query<&GlobalTransform, With<BakedSun>>,
    #[cfg(not(target_arch = "wasm32"))] mut gpu_trigger: ResMut<GpuBakeTrigger>,
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

    info!("Starting lightmap bake pass…");

    // ── Phase 1: Terrain ────────────────────────────────────────────────────
    #[cfg(not(target_arch = "wasm32"))]
    if config.use_gpu {
        // Signal the GPU dispatch system to handle terrain baking this frame.
        gpu_trigger.0 = Some(sun_dir);
    } else {
        terrain::bake_terrain_lightmaps(
            &heightmap,
            &terrain_world,
            &config,
            &terrain_chunk_query,
            &mut terrain_materials,
            &mut images,
            sun_dir,
            &placed_query,
            &children_query,
            &mesh_gt_query,
            &*meshes,
        );
    }

    #[cfg(target_arch = "wasm32")]
    terrain::bake_terrain_lightmaps(
        &heightmap,
        &terrain_world,
        &config,
        &terrain_chunk_query,
        &mut terrain_materials,
        &mut images,
        sun_dir,
        &placed_query,
        &children_query,
        &mesh_gt_query,
        &*meshes,
    );

    // ── Phase 2: Placed objects (always CPU) ────────────────────────────────
    objects::bake_object_lightmaps(
        &heightmap,
        &terrain_world,
        &config,
        &placed_query,
        &mesh_query,
        &mesh_gt_query,
        &children_query,
        &mut meshes,
        &mut toon_materials,
        &mut images,
        &mut uv_cache.0,
        sun_dir,
    );
}

/// Dispatches GPU terrain bake jobs when `GpuBakeTrigger` is set.
///
/// Runs immediately after `run_bake` on non-wasm targets.
#[cfg(not(target_arch = "wasm32"))]
#[allow(clippy::too_many_arguments)]
fn dispatch_gpu_terrain_bake(
    mut gpu_trigger: ResMut<GpuBakeTrigger>,
    config: Res<BakeConfig>,
    heightmap: Option<Res<HeightmapData>>,
    terrain_world: Option<Res<TerrainWorld>>,
    chunk_registry: Option<Res<TerrainChunkRegistry>>,
    mut terrain_materials: ResMut<Assets<ExtendedMaterial<StandardMaterial, TerrainMaterial>>>,
    mut images: ResMut<Assets<Image>>,
    terrain_chunk_query: Query<(
        &TerrainChunk,
        &MeshMaterial3d<ExtendedMaterial<StandardMaterial, TerrainMaterial>>,
    )>,
    placed_query: Query<(Entity, &PlacedObject, &GlobalTransform)>,
    children_query: Query<&Children>,
    mesh_gt_query: Query<(&Mesh3d, &GlobalTransform)>,
    meshes: Res<Assets<Mesh>>,
    mut gpu_processed: ResMut<gpu::GpuBakeProcessed>,
    mut gpu_queue: ResMut<gpu::GpuBakeQueue>,
) {
    let Some(sun_dir) = gpu_trigger.0.take() else {
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
                &registry,
                &terrain_chunk_query,
                &mut gpu_processed,
                &mut gpu_queue,
                sun_dir,
                &placed_query,
                &children_query,
                &mesh_gt_query,
                &*meshes,
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
                &terrain_chunk_query,
                &mut terrain_materials,
                &mut images,
                sun_dir,
                &placed_query,
                &children_query,
                &mesh_gt_query,
                &*meshes,
            );
        }
    }
}
