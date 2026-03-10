//! Chunk streaming system — spawns/despawns terrain and object chunks based on
//! how close the player is, using Bevy's `AssetServer` for async I/O.
//!
//! # Lifecycle of a chunk
//!
//! 1. `update_streaming_ring` determines which chunks should be loaded.
//!    - If the chunk has saved terrain modifications it issues an async load via
//!      `asset_server.load("world://…/chunk_X_Y.terrain")`.
//!    - Always issues an async load for the `.objects` file; a 404 is treated as
//!      empty objects (no fast-path to avoid `std::fs` calls for WASM compat).
//!    - All chunks go into `StreamingState::pending`.
//!
//! 2. `launch_chunk_tasks` checks load state every frame.
//!    When both handles are done (Loaded or Failed) it patches `HeightmapData`,
//!    extracts the height array, and spawns a background `AsyncComputeTaskPool` task
//!    to generate LOD meshes off the main thread. The chunk moves to `computing`.
//!
//! 3. `finalize_computed_chunks` polls the background tasks each frame.
//!    When a task completes it calls `spawn_chunk_from_result` (main thread only,
//!    for physics collider construction) and spawns any loaded objects.

use bevy::{
    asset::LoadState,
    prelude::*,
    tasks::{AsyncComputeTaskPool, Task, futures::check_ready},
};
use std::collections::{HashMap, HashSet};

use crate::game::simulation::Player;

use super::{
    chunk_assets::{ChunkDataAsset, ChunkObjectsAsset},
    objects::{
        PlacedObject, PlacedObjectRegistry, despawn_chunk_objects, spawn_chunk_objects,
        world_pos_to_chunk,
    },
    terrain::{
        HeightmapData, TILE_SIZE,
        bootstrap::{
            ChunkComputeResult, TerrainRenderAssets, apply_chunk_to_heightmap,
            compute_chunk_meshes, extract_chunk_heights, spawn_chunk_from_result,
        },
        storage::{ChunkState, TerrainChunkRegistry, TerrainWorld},
    },
};

// ============================================================================
// Resources
// ============================================================================

/// Controls how many chunks around the player are kept alive.
#[derive(Resource)]
pub struct ChunkStreamingConfig {
    /// Chebyshev radius within which chunks are loaded.
    pub load_radius: u32,
    /// Chebyshev radius beyond which loaded chunks are despawned.
    /// Should be > `load_radius` to create hysteresis and prevent thrashing.
    pub unload_radius: u32,
    /// Maximum number of completed background tasks to finalize per frame.
    /// Spreading finalizations avoids the frame-time spike from building multiple
    /// avian heightfield colliders + registering mesh assets in a single frame.
    pub max_spawns_per_frame: usize,
}

impl Default for ChunkStreamingConfig {
    fn default() -> Self {
        Self {
            load_radius: 4,
            unload_radius: 6,
            max_spawns_per_frame: 2,
        }
    }
}

/// A chunk that has been requested but is still loading its asset files.
pub struct PendingChunk {
    /// `None` → chunk has no terrain file (flat); terrain is already ready.
    pub terrain_handle: Option<Handle<ChunkDataAsset>>,
    /// Always present. `Failed` (e.g. 404) is treated as empty objects.
    pub objects_handle: Handle<ChunkObjectsAsset>,
}

/// A chunk whose mesh-generation task is running in the background.
pub struct ComputingChunk {
    pub task: Task<ChunkComputeResult>,
    /// Retained so we can spawn objects once the task finishes.
    pub objects_handle: Handle<ChunkObjectsAsset>,
}

/// Runtime state for the streaming system.
#[derive(Resource, Default)]
pub struct StreamingState {
    /// Chunks whose entities are currently live in the world.
    pub loaded: HashSet<(i32, i32)>,
    /// Chunks that are waiting for their asset files to finish loading.
    pub pending: HashMap<(i32, i32), PendingChunk>,
    /// Chunks whose mesh-generation task is running in the background.
    pub computing: HashMap<(i32, i32), ComputingChunk>,
}

// ============================================================================
// Systems
// ============================================================================

/// Determines which chunks should be in the streaming ring and:
/// - Issues asset loads for newly in-range chunks.
/// - Despawns chunks that have moved beyond the unload radius (and evicts their tiles).
/// - Cancels background compute tasks for chunks that go out of range.
pub fn update_streaming_ring(
    player_query: Query<&Transform, With<Player>>,
    terrain_world: Option<Res<TerrainWorld>>,
    config: Res<ChunkStreamingConfig>,
    asset_server: Res<AssetServer>,
    mut commands: Commands,
    mut state: ResMut<StreamingState>,
    chunk_registry: Option<ResMut<TerrainChunkRegistry>>,
    mut obj_registry: ResMut<PlacedObjectRegistry>,
    placed_query: Query<(Entity, &PlacedObject)>,
    mut heightmap: Option<ResMut<HeightmapData>>,
) {
    // All three are inserted together by create_terrain; wait until they're available.
    let Some(terrain_world) = terrain_world else {
        return;
    };
    let Some(mut chunk_registry) = chunk_registry else {
        return;
    };

    let Ok(player_transform) = player_query.single() else {
        return;
    };

    let player_pos = player_transform.translation;
    let (player_cx, player_cy) = world_pos_to_chunk(player_pos.x, player_pos.z);

    let metadata = &terrain_world.metadata;

    // ── Unload distant loaded chunks ──────────────────────────────────────────
    let to_unload: Vec<(i32, i32)> = state
        .loaded
        .iter()
        .copied()
        .filter(|&(cx, cy)| chebyshev(cx, cy, player_cx, player_cy) > config.unload_radius)
        .collect();

    for chunk in to_unload {
        if let Some(entity) = chunk_registry.loaded_chunks.remove(&chunk) {
            commands.entity(entity).despawn();
        }
        despawn_chunk_objects(&mut commands, &mut obj_registry, &placed_query, chunk);
        // Evict heightmap tiles to keep memory bounded for infinite terrain.
        if let Some(ref mut hm) = heightmap {
            hm.remove_chunk_tiles(chunk.0, chunk.1);
        }
        state.loaded.remove(&chunk);
    }

    // ── Cancel out-of-range computing tasks ───────────────────────────────────
    // Dropping the Task cancels it automatically.
    let to_cancel: Vec<(i32, i32)> = state
        .computing
        .keys()
        .copied()
        .filter(|&(cx, cy)| chebyshev(cx, cy, player_cx, player_cy) > config.unload_radius)
        .collect();
    for chunk in to_cancel {
        state.computing.remove(&chunk);
    }

    // ── Request newly in-range chunks ─────────────────────────────────────────
    let r = config.load_radius as i32;

    for dy in -r..=r {
        for dx in -r..=r {
            let cx = player_cx + dx;
            let cy = player_cy + dy;

            if state.loaded.contains(&(cx, cy))
                || state.pending.contains_key(&(cx, cy))
                || state.computing.contains_key(&(cx, cy))
            {
                continue;
            }

            // Does this chunk have saved terrain data?
            let has_terrain_file = metadata
                .chunk_index
                .iter()
                .find(|e| e.chunk_x == cx && e.chunk_y == cy)
                .map(|e| e.state != ChunkState::Unmodified)
                .unwrap_or(false);

            let terrain_handle = has_terrain_file.then(|| {
                asset_server.load::<ChunkDataAsset>(format!(
                    "world://worlds/{}/chunks/chunk_{}_{}.terrain",
                    metadata.world_id, cx, cy
                ))
            });

            // Always issue the objects load. Failed (404) → treated as empty.
            let objects_handle = asset_server.load::<ChunkObjectsAsset>(format!(
                "world://worlds/{}/chunks/chunk_{}_{}.objects",
                metadata.world_id, cx, cy
            ));

            state.pending.insert(
                (cx, cy),
                PendingChunk {
                    terrain_handle,
                    objects_handle,
                },
            );
        }
    }
}

/// Checks pending chunk asset loads and, for each fully-loaded chunk, patches
/// `HeightmapData` and spawns a background `AsyncComputeTaskPool` task to generate
/// LOD meshes off the main thread.
///
/// Spawning background tasks is cheap (just a queue push), so no per-frame budget
/// is applied here — the budget in `finalize_computed_chunks` limits main-thread cost.
pub fn launch_chunk_tasks(
    asset_server: Res<AssetServer>,
    chunk_data_assets: Res<Assets<ChunkDataAsset>>,
    mut state: ResMut<StreamingState>,
    heightmap: Option<ResMut<HeightmapData>>,
) {
    let Some(mut heightmap) = heightmap else {
        return;
    };

    // Determine which pending chunks have all their assets resolved.
    let mut ready_with_data: Vec<(i32, i32)> = Vec::new();
    let mut ready_flat: Vec<(i32, i32)> = Vec::new();

    for (&chunk, pending) in state.pending.iter() {
        let terrain_state = match &pending.terrain_handle {
            None => Some(true), // flat — no file needed
            Some(h) => match asset_server.load_state(h.id()) {
                LoadState::Loaded => Some(true),
                LoadState::Failed(_) => {
                    warn!(
                        "Failed to load terrain for chunk {:?}, spawning flat",
                        chunk
                    );
                    Some(false) // failed → spawn flat
                }
                _ => None, // still loading
            },
        };

        let objects_done = matches!(
            asset_server.load_state(pending.objects_handle.id()),
            LoadState::Loaded | LoadState::Failed(_)
        );

        if let Some(terrain_loaded) = terrain_state {
            if objects_done {
                if terrain_loaded {
                    ready_with_data.push(chunk);
                } else {
                    ready_flat.push(chunk);
                }
            }
        }
    }

    let thread_pool = AsyncComputeTaskPool::get();

    // Launch tasks for chunks with terrain data.
    for chunk_coord in ready_with_data {
        let Some(pending) = state.pending.remove(&chunk_coord) else {
            continue;
        };
        let (cx, cy) = chunk_coord;

        // Patch heightmap with stored modifications (must happen on main thread).
        if let Some(terrain_asset) = pending
            .terrain_handle
            .as_ref()
            .and_then(|h| chunk_data_assets.get(h))
        {
            apply_chunk_to_heightmap(&mut heightmap, &terrain_asset.0);
        }

        // Extract heights into an owned Vec so the task can move it off-thread.
        let heights = extract_chunk_heights(&heightmap, cx, cy);

        let task = thread_pool.spawn(async move {
            compute_chunk_meshes(heights, cx, cy, TILE_SIZE)
        });

        state.computing.insert(
            chunk_coord,
            ComputingChunk {
                task,
                objects_handle: pending.objects_handle,
            },
        );
    }

    // Launch tasks for flat (data-less) chunks.
    for chunk_coord in ready_flat {
        let Some(pending) = state.pending.remove(&chunk_coord) else {
            continue;
        };
        let (cx, cy) = chunk_coord;

        // No heightmap patch needed — use whatever is already in HeightmapData
        // (implicitly 0.0 for tiles never written).
        let heights = extract_chunk_heights(&heightmap, cx, cy);

        let task = thread_pool.spawn(async move {
            compute_chunk_meshes(heights, cx, cy, TILE_SIZE)
        });

        state.computing.insert(
            chunk_coord,
            ComputingChunk {
                task,
                objects_handle: pending.objects_handle,
            },
        );
    }
}

/// Polls background mesh-generation tasks and finalizes any that are complete.
///
/// For each ready task: inserts the computed `Mesh` assets, builds the avian
/// heightfield collider (main-thread only), spawns the chunk entity, and spawns
/// any loaded objects. At most `max_spawns_per_frame` chunks are finalized per call.
pub fn finalize_computed_chunks(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    chunk_objects_assets: Res<Assets<ChunkObjectsAsset>>,
    mut state: ResMut<StreamingState>,
    config: Res<ChunkStreamingConfig>,
    chunk_registry: Option<ResMut<TerrainChunkRegistry>>,
    render_assets: Option<Res<TerrainRenderAssets>>,
    mut obj_registry: ResMut<PlacedObjectRegistry>,
    asset_server: Res<AssetServer>,
) {
    let (Some(mut chunk_registry), Some(render_assets)) = (chunk_registry, render_assets) else {
        return;
    };

    let budget = config.max_spawns_per_frame;
    let mut ready_chunks: Vec<(i32, i32)> = Vec::new();

    // Poll each computing task without blocking; collect those that are done.
    for (&chunk_coord, computing) in state.computing.iter_mut() {
        if ready_chunks.len() >= budget {
            break;
        }
        if computing.task.is_finished() {
            ready_chunks.push(chunk_coord);
        }
    }

    for chunk_coord in ready_chunks {
        let Some(mut computing) = state.computing.remove(&chunk_coord) else {
            continue;
        };

        // Task is finished; check_ready polls it once and returns the result.
        let Some(result) = check_ready(&mut computing.task) else {
            // Shouldn't happen — we confirmed is_finished() — but be defensive.
            state.computing.insert(chunk_coord, computing);
            continue;
        };

        spawn_chunk_from_result(
            &mut commands,
            &mut meshes,
            &mut chunk_registry,
            &render_assets,
            result,
        );

        if let Some(asset) = chunk_objects_assets.get(&computing.objects_handle) {
            spawn_chunk_objects(&mut commands, &*asset_server, &mut obj_registry, &asset.0);
        }

        state.loaded.insert(chunk_coord);
    }
}

// ============================================================================
// Helpers
// ============================================================================

/// Chebyshev (Chessboard) distance between two chunk coordinates.
#[inline]
fn chebyshev(ax: i32, ay: i32, bx: i32, by: i32) -> u32 {
    (ax - bx).unsigned_abs().max((ay - by).unsigned_abs())
}
