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
//! 2. `process_pending_chunks` checks load state every frame.
//!    When both handles are done (Loaded or Failed) it calls `spawn_terrain_chunk`
//!    and `spawn_chunk_objects`, then moves the chunk into `StreamingState::loaded`.
//!
//! 3. `update_streaming_ring` despawns chunks that have moved beyond `unload_radius`
//!    and evicts their heightmap tiles to keep memory bounded.

use bevy::{asset::LoadState, prelude::*};
use std::collections::{HashMap, HashSet};

use crate::game::simulation::Player;

use super::{
    chunk_assets::{ChunkDataAsset, ChunkObjectsAsset},
    objects::{
        PlacedObject, PlacedObjectRegistry, despawn_chunk_objects, spawn_chunk_objects,
        world_pos_to_chunk,
    },
    terrain::{
        HeightmapData,
        bootstrap::{TerrainRenderAssets, spawn_terrain_chunk},
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
    /// Maximum chunks to spawn per frame. Spreading spawns avoids the frame-time
    /// spike caused by generating 4 LOD meshes + a heightfield collider per chunk.
    pub max_spawns_per_frame: usize,
}

impl Default for ChunkStreamingConfig {
    fn default() -> Self {
        Self {
            load_radius: 4,
            unload_radius: 6,
            max_spawns_per_frame: 1,
        }
    }
}

/// A chunk that has been requested but is still loading.
pub struct PendingChunk {
    /// `None` → chunk has no terrain file (flat); terrain is already ready.
    pub terrain_handle: Option<Handle<ChunkDataAsset>>,
    /// Always present. `Failed` (e.g. 404) is treated as empty objects.
    pub objects_handle: Handle<ChunkObjectsAsset>,
}

/// Runtime state for the streaming system.
#[derive(Resource, Default)]
pub struct StreamingState {
    /// Chunks whose entities are currently live in the world.
    pub loaded: HashSet<(i32, i32)>,
    /// Chunks that are being loaded asynchronously.
    pub pending: HashMap<(i32, i32), PendingChunk>,
}

// ============================================================================
// Systems
// ============================================================================

/// Determines which chunks should be in the streaming ring and:
/// - Issues asset loads for newly in-range chunks.
/// - Despawns chunks that have moved beyond the unload radius (and evicts their tiles).
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

    // ── Unload distant chunks ─────────────────────────────────────────────────
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

    // ── Request newly in-range chunks ─────────────────────────────────────────
    let r = config.load_radius as i32;

    for dy in -r..=r {
        for dx in -r..=r {
            let cx = player_cx + dx;
            let cy = player_cy + dy;

            if state.loaded.contains(&(cx, cy)) || state.pending.contains_key(&(cx, cy)) {
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

/// Checks pending chunk asset loads and spawns chunks whose assets are fully ready.
/// At most `ChunkStreamingConfig::max_spawns_per_frame` chunks are spawned per call
/// to avoid the per-frame spike of generating multiple LOD meshes + heightfield colliders.
pub fn process_pending_chunks(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    asset_server: Res<AssetServer>,
    chunk_data_assets: Res<Assets<ChunkDataAsset>>,
    chunk_objects_assets: Res<Assets<ChunkObjectsAsset>>,
    mut state: ResMut<StreamingState>,
    config: Res<ChunkStreamingConfig>,
    chunk_registry: Option<ResMut<TerrainChunkRegistry>>,
    heightmap: Option<ResMut<HeightmapData>>,
    render_assets: Option<Res<TerrainRenderAssets>>,
    mut obj_registry: ResMut<PlacedObjectRegistry>,
) {
    let (Some(mut chunk_registry), Some(mut heightmap), Some(render_assets)) =
        (chunk_registry, heightmap, render_assets)
    else {
        return;
    };

    // ready = spawn with data; flat = spawn without terrain data (terrain failed).
    // Capped to max_spawns_per_frame total to spread mesh-generation cost across frames.
    let budget = config.max_spawns_per_frame;
    let mut ready: Vec<(i32, i32)> = Vec::new();
    let mut flat: Vec<(i32, i32)> = Vec::new();

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

        let objects_done = match asset_server.load_state(pending.objects_handle.id()) {
            LoadState::Loaded => true,
            LoadState::Failed(_) => {
                // 404 or other error → treat as empty objects
                true
            }
            _ => false,
        };

        if let Some(terrain_loaded) = terrain_state {
            if objects_done {
                if terrain_loaded {
                    ready.push(chunk);
                } else {
                    flat.push(chunk);
                }
            }
        }
    }

    // Cap both lists so total spawns this frame ≤ budget.
    flat.truncate(budget);
    ready.truncate(budget.saturating_sub(flat.len()));

    // Spawn failed terrain chunks as flat (no data).
    for chunk in flat {
        state.pending.remove(&chunk);
        let (cx, cy) = chunk;
        spawn_terrain_chunk(
            &mut commands,
            &mut meshes,
            &mut chunk_registry,
            &mut heightmap,
            &render_assets,
            cx,
            cy,
            None,
        );
        state.loaded.insert(chunk);
    }

    // Spawn ready chunks.
    for chunk in ready {
        let Some(pending) = state.pending.remove(&chunk) else {
            continue;
        };
        let (cx, cy) = chunk;

        // Retrieve terrain data (if any).
        let chunk_data_ref = pending
            .terrain_handle
            .as_ref()
            .and_then(|h| chunk_data_assets.get(h))
            .map(|a| &a.0);

        spawn_terrain_chunk(
            &mut commands,
            &mut meshes,
            &mut chunk_registry,
            &mut heightmap,
            &render_assets,
            cx,
            cy,
            chunk_data_ref,
        );

        // Spawn objects (if any loaded successfully).
        if let Some(asset) = chunk_objects_assets.get(&pending.objects_handle) {
            spawn_chunk_objects(&mut commands, &*asset_server, &mut obj_registry, &asset.0);
        }

        state.loaded.insert(chunk);
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
