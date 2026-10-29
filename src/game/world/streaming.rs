//! Chunk streaming system — spawns/despawns terrain and object chunks based on
//! how close the player is.
//!
//! # Lifecycle of a chunk (native / non-OPFS)
//!
//! 1. `update_streaming_ring` issues async AssetServer loads for terrain + objects files.
//!    All chunks go into `StreamingState::pending`.
//!
//! 2. `launch_chunk_tasks` checks load state every frame.
//!    When both handles are done it patches `HeightmapData`, extracts the height array,
//!    and spawns a background `AsyncComputeTaskPool` task to generate LOD meshes and
//!    decode the chunk's saved lightmap. The chunk moves to `computing`.
//!
//! 3. `finalize_computed_chunks` polls background tasks each frame.
//!    When a task completes it calls `spawn_chunk_from_result` and spawns any loaded objects.
//!
//! # Lifecycle of a chunk (wasm+opfs)
//!
//! 1. `update_streaming_ring` issues `spawn_local` futures to read from OPFS.
//!    Parsing happens inside the future. Typed results land in Arc<Mutex<Option<...>>> slots.
//!
//! 2. `launch_chunk_tasks` polls the slots. Once both are ready it patches the heightmap
//!    (on main thread) and spawns the compute task with parsed data.
//!    `ComputingChunk.objects_data` carries the pre-parsed `Option<ChunkObjectsFile>`.
//!
//! 3. `finalize_computed_chunks` uses `computing.objects_data` directly (no asset lookup).

use bevy::{
    pbr::ExtendedMaterial,
    prelude::*,
    tasks::{AsyncComputeTaskPool, Task, futures::check_ready},
};
use std::collections::{HashMap, HashSet};

use crate::game::WorldFocus;

use super::{
    baking::{lightmap::LightmapSource, probes::ProbeSource},
    chunk_assets::{ChunkDataAsset, ChunkObjectsAsset},
    objects::{
        PlacedObject, PlacedObjectRegistry, despawn_chunk_objects, spawn_chunk_objects,
        world_pos_to_chunk,
    },
    prefabs::PrefabLibrary,
    probe_volume::ProbeStore,
    terrain::{
        HeightmapData, TILE_SIZE,
        bootstrap::{
            ChunkComputeResult, TerrainRenderAssets, apply_chunk_to_heightmap,
            compute_chunk_meshes, spawn_chunk_from_result,
        },
        material::TerrainMaterial,
        storage::{ChunkState, TerrainChunkRegistry, TerrainWorld},
    },
};

#[cfg(all(target_arch = "wasm32", feature = "opfs"))]
use super::{objects::ChunkObjectsFile, terrain::storage::ChunkData};

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
    /// Maximum number of new asset loads to issue per frame.
    /// Prevents a burst of asset_server.load() calls when the player first enters
    /// the world (or teleports) from causing a multi-ms lock-contention spike.
    /// At 9/frame with a 4-radius ring (81 chunks), the full ring fills in ~9 frames.
    pub max_new_loads_per_frame: usize,
}

impl Default for ChunkStreamingConfig {
    fn default() -> Self {
        Self {
            load_radius: 4,
            unload_radius: 6,
            max_spawns_per_frame: 2,
            max_new_loads_per_frame: 9,
        }
    }
}

// ── Slot type aliases for the wasm+opfs path ──────────────────────────────────
//
// `None`      → load still pending
// `Some(None)` → loaded, but nothing was stored (flat / 404)
// `Some(Some(data))` → loaded with parsed data

#[cfg(all(target_arch = "wasm32", feature = "opfs"))]
type TerrainSlot = std::sync::Arc<std::sync::Mutex<Option<Option<ChunkData>>>>;
#[cfg(all(target_arch = "wasm32", feature = "opfs"))]
type ObjectsSlot = std::sync::Arc<std::sync::Mutex<Option<Option<ChunkObjectsFile>>>>;
/// Raw EXR bytes; decoding happens in the compute task, as it does natively.
#[cfg(all(target_arch = "wasm32", feature = "opfs"))]
type LightmapSlot = std::sync::Arc<std::sync::Mutex<Option<Option<Vec<u8>>>>>;
/// Raw probe-file bytes, same arrangement.
#[cfg(all(target_arch = "wasm32", feature = "opfs"))]
type ProbesSlot = std::sync::Arc<std::sync::Mutex<Option<Option<Vec<u8>>>>>;

/// A chunk that has been requested but is still loading its data.
pub struct PendingChunk {
    #[cfg(not(all(target_arch = "wasm32", feature = "opfs")))]
    /// `None` → chunk has no terrain file (flat); terrain is already ready.
    pub terrain_handle: Option<Handle<ChunkDataAsset>>,
    #[cfg(not(all(target_arch = "wasm32", feature = "opfs")))]
    /// `None` = no objects file exists; `Some(h)` = load in progress or done.
    pub objects_handle: Option<Handle<ChunkObjectsAsset>>,

    #[cfg(all(target_arch = "wasm32", feature = "opfs"))]
    pub opfs_terrain: TerrainSlot,
    #[cfg(all(target_arch = "wasm32", feature = "opfs"))]
    pub opfs_objects: ObjectsSlot,
    #[cfg(all(target_arch = "wasm32", feature = "opfs"))]
    pub opfs_lightmap: LightmapSlot,
    #[cfg(all(target_arch = "wasm32", feature = "opfs"))]
    pub opfs_probes: ProbesSlot,
}

/// A chunk whose mesh-generation task is running in the background.
pub struct ComputingChunk {
    pub task: Task<ChunkComputeResult>,
    #[cfg(not(all(target_arch = "wasm32", feature = "opfs")))]
    /// Retained so we can spawn objects once the task finishes.
    pub objects_handle: Option<Handle<ChunkObjectsAsset>>,
    #[cfg(all(target_arch = "wasm32", feature = "opfs"))]
    /// Pre-parsed at launch time; used directly in `finalize_computed_chunks`.
    pub objects_data: Option<ChunkObjectsFile>,
}

/// Runtime state for the streaming system.
#[derive(Resource, Default)]
pub struct StreamingState {
    /// Chunks whose entities are currently live in the world.
    pub loaded: HashSet<(i32, i32)>,
    /// Chunks that are waiting for their data to finish loading.
    pub pending: HashMap<(i32, i32), PendingChunk>,
    /// Chunks whose mesh-generation task is running in the background.
    pub computing: HashMap<(i32, i32), ComputingChunk>,
}

// ============================================================================
// Systems
// ============================================================================

/// Determines which chunks should be in the streaming ring and:
/// - Issues asset/OPFS loads for newly in-range chunks.
/// - Despawns chunks that have moved beyond the unload radius (and evicts their tiles).
/// - Cancels background compute tasks for chunks that go out of range.
pub fn update_streaming_ring(
    focus: Res<WorldFocus>,
    terrain_world: Option<Res<TerrainWorld>>,
    config: Res<ChunkStreamingConfig>,
    #[cfg(not(all(target_arch = "wasm32", feature = "opfs")))] asset_server: Res<AssetServer>,
    #[cfg(all(target_arch = "wasm32", feature = "opfs"))] _storage: Res<crate::storage::Storage>,
    mut commands: Commands,
    mut state: ResMut<StreamingState>,
    chunk_registry: Option<ResMut<TerrainChunkRegistry>>,
    mut obj_registry: ResMut<PlacedObjectRegistry>,
    placed_query: Query<(Entity, &PlacedObject)>,
    mut heightmap: Option<ResMut<HeightmapData>>,
    history: Option<Res<crate::editor::history::History>>,
    mut probe_store: ResMut<ProbeStore>,
) {
    // All three are inserted together by create_terrain; wait until they're available.
    let Some(terrain_world) = terrain_world else {
        return;
    };
    let Some(mut chunk_registry) = chunk_registry else {
        return;
    };

    let focus_pos = focus.0;
    let (player_cx, player_cy) = world_pos_to_chunk(focus_pos.x, focus_pos.z);

    let metadata = &terrain_world.metadata;

    // ── Unload distant loaded chunks ──────────────────────────────────────────
    // Chunks with unsaved edits are exempt: the save systems rebuild chunk
    // files from live entities and heightmap tiles, so unloading a dirty
    // chunk would make the next save overwrite its file with empty/flat
    // data. A dirty chunk stays resident until a save drains the dirty sets,
    // after which it unloads normally. Chunks the editor's undo history
    // refers to are pinned the same way, so an undo can always find its
    // target.
    let pinned = history.as_ref().map(|h| &h.pinned);
    let to_unload: Vec<(i32, i32)> = state
        .loaded
        .iter()
        .copied()
        .filter(|&(cx, cy)| chebyshev(cx, cy, player_cx, player_cy) > config.unload_radius)
        .filter(|chunk| {
            !chunk_registry.dirty_chunks.contains(chunk)
                && !obj_registry.dirty_chunks.contains(chunk)
                && !pinned.is_some_and(|p| p.contains(chunk))
        })
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
        probe_store.remove(chunk);
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

    // Collect candidates and sort closest-first so the player sees nearby ground
    // before distant chunks, and so the per-frame budget always prioritises them.
    let mut candidates: Vec<(i32, i32)> = Vec::new();
    for dy in -r..=r {
        for dx in -r..=r {
            let cx = player_cx + dx;
            let cy = player_cy + dy;
            if !state.loaded.contains(&(cx, cy))
                && !state.pending.contains_key(&(cx, cy))
                && !state.computing.contains_key(&(cx, cy))
            {
                candidates.push((cx, cy));
            }
        }
    }
    candidates.sort_by_key(|&(cx, cy)| chebyshev(cx, cy, player_cx, player_cy));

    for (cx, cy) in candidates.into_iter().take(config.max_new_loads_per_frame) {
        // Does this chunk have saved terrain data? O(1) map lookup.
        let has_terrain_file = terrain_world
            .chunk_state_map
            .get(&(cx, cy))
            .map(|s| *s != ChunkState::Unmodified)
            .unwrap_or(false);

        #[cfg(not(all(target_arch = "wasm32", feature = "opfs")))]
        {
            let terrain_handle = has_terrain_file.then(|| {
                asset_server.load::<ChunkDataAsset>(format!(
                    "world://worlds/{}/chunks/chunk_{}_{}.terrain",
                    metadata.world_id, cx, cy
                ))
            });

            // Only issue the objects load if the file exists on disk; avoids
            // Bevy logging asset-not-found errors for unvisited chunks.
            let objects_path = terrain_world
                .world_path
                .join("chunks")
                .join(format!("chunk_{}_{}.objects", cx, cy));
            let objects_handle = objects_path.exists().then(|| {
                asset_server.load::<ChunkObjectsAsset>(format!(
                    "world://worlds/{}/chunks/chunk_{}_{}.objects",
                    metadata.world_id, cx, cy
                ))
            });

            state.pending.insert(
                (cx, cy),
                PendingChunk {
                    terrain_handle,
                    objects_handle,
                },
            );
        }

        #[cfg(all(target_arch = "wasm32", feature = "opfs"))]
        {
            use super::objects::parse_chunk_objects_from_bytes;
            use super::terrain::storage::parse_chunk_from_bytes;
            use std::sync::{Arc, Mutex};
            use wasm_bindgen_futures::spawn_local;

            let terrain_slot: TerrainSlot = Arc::new(Mutex::new(None));
            let objects_slot: ObjectsSlot = Arc::new(Mutex::new(None));
            let lightmap_slot: LightmapSlot = Arc::new(Mutex::new(None));
            let probes_slot: ProbesSlot = Arc::new(Mutex::new(None));

            // Terrain: only load if there is saved data; otherwise immediately mark flat.
            if has_terrain_file {
                let s = crate::storage::wasm::WasmStorage;
                let key = format!(
                    "worlds/{}/chunks/chunk_{}_{}.terrain",
                    metadata.world_id, cx, cy
                );
                let slot = terrain_slot.clone();
                spawn_local(async move {
                    let result = match s.load(&key).await {
                        Ok(Some(bytes)) => match parse_chunk_from_bytes(&bytes) {
                            Ok(data) => Some(data),
                            Err(e) => {
                                log::warn!("Failed to parse terrain for chunk ({cx},{cy}): {e}");
                                None
                            }
                        },
                        Ok(None) => None,
                        Err(e) => {
                            log::warn!("OPFS terrain load error for ({cx},{cy}): {e}");
                            None
                        }
                    };
                    *slot.lock().unwrap() = Some(result);
                });
            } else {
                // No file → immediately ready as flat.
                *terrain_slot.lock().unwrap() = Some(None);
            }

            // Objects: always try; None on 404 or parse error means empty chunk.
            {
                let s = crate::storage::wasm::WasmStorage;
                let key = format!(
                    "worlds/{}/chunks/chunk_{}_{}.objects",
                    metadata.world_id, cx, cy
                );
                let slot = objects_slot.clone();
                spawn_local(async move {
                    let result = match s.load(&key).await {
                        Ok(Some(bytes)) => match parse_chunk_objects_from_bytes(&bytes) {
                            Ok(data) => Some(data),
                            Err(e) => {
                                log::warn!("Failed to parse objects for chunk ({cx},{cy}): {e}");
                                None
                            }
                        },
                        Ok(None) => None,
                        Err(e) => {
                            log::warn!("OPFS objects load error for ({cx},{cy}): {e}");
                            None
                        }
                    };
                    *slot.lock().unwrap() = Some(result);
                });
            }

            // Lightmap: raw bytes only. Absent or unreadable simply means
            // "unbaked", so every failure collapses to `None`.
            {
                let s = crate::storage::wasm::WasmStorage;
                let key = super::baking::lightmap::terrain_lightmap_key(&metadata.world_id, cx, cy);
                let slot = lightmap_slot.clone();
                spawn_local(async move {
                    let result = match s.load(&key).await {
                        Ok(bytes) => bytes,
                        Err(e) => {
                            log::warn!("OPFS lightmap load error for ({cx},{cy}): {e}");
                            None
                        }
                    };
                    *slot.lock().unwrap() = Some(result);
                });
            }

            // Probes: same shape as the lightmap — bytes or nothing.
            {
                let s = crate::storage::wasm::WasmStorage;
                let key = super::baking::probes::chunk_probes_key(&metadata.world_id, cx, cy);
                let slot = probes_slot.clone();
                spawn_local(async move {
                    let result = match s.load(&key).await {
                        Ok(bytes) => bytes,
                        Err(e) => {
                            log::warn!("OPFS probe load error for ({cx},{cy}): {e}");
                            None
                        }
                    };
                    *slot.lock().unwrap() = Some(result);
                });
            }

            state.pending.insert(
                (cx, cy),
                PendingChunk {
                    opfs_terrain: terrain_slot,
                    opfs_objects: objects_slot,
                    opfs_lightmap: lightmap_slot,
                    opfs_probes: probes_slot,
                },
            );
        }
    }
}

/// Checks pending chunk data loads and, for each fully-loaded chunk, patches
/// `HeightmapData` and spawns a background `AsyncComputeTaskPool` task to generate
/// LOD meshes off the main thread.
///
/// Spawning background tasks is cheap (just a queue push), so no per-frame budget
/// is applied here — the budget in `finalize_computed_chunks` limits main-thread cost.
pub fn launch_chunk_tasks(
    #[cfg(not(all(target_arch = "wasm32", feature = "opfs")))] asset_server: Res<AssetServer>,
    #[cfg(not(all(target_arch = "wasm32", feature = "opfs")))] chunk_data_assets: Res<
        Assets<ChunkDataAsset>,
    >,
    mut state: ResMut<StreamingState>,
    heightmap: Option<ResMut<HeightmapData>>,
    terrain_world: Option<Res<TerrainWorld>>,
) {
    let Some(mut heightmap) = heightmap else {
        return;
    };
    // Natively, where this chunk's baked lightmap lives — the task reads and
    // decodes it. On wasm the bytes have already been fetched from OPFS into
    // the chunk's slot, so this closure is unused there.
    #[cfg(not(all(target_arch = "wasm32", feature = "opfs")))]
    let lightmap_src = |cx: i32, cy: i32| -> Option<LightmapSource> {
        terrain_world
            .as_ref()
            .map(|w| super::baking::lightmap::terrain_lightmap_path(&w.world_path, cx, cy))
    };
    #[cfg(not(all(target_arch = "wasm32", feature = "opfs")))]
    let probes_src = |cx: i32, cy: i32| -> Option<ProbeSource> {
        terrain_world
            .as_ref()
            .map(|w| super::baking::probes::chunk_probes_path(&w.world_path, cx, cy))
    };
    #[cfg(all(target_arch = "wasm32", feature = "opfs"))]
    let _ = &terrain_world;

    #[cfg(not(all(target_arch = "wasm32", feature = "opfs")))]
    {
        use bevy::asset::LoadState;

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

            let objects_done = match &pending.objects_handle {
                None => true,
                Some(h) => matches!(
                    asset_server.load_state(h.id()),
                    LoadState::Loaded | LoadState::Failed(_)
                ),
            };

            if let Some(terrain_loaded) = terrain_state
                && objects_done
            {
                if terrain_loaded {
                    ready_with_data.push(chunk);
                } else {
                    ready_flat.push(chunk);
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

            // Patch heightmap with stored modifications (must happen on main thread,
            // keeps HeightmapData correct for physics/gameplay queries).
            let chunk_data = pending
                .terrain_handle
                .as_ref()
                .and_then(|h| chunk_data_assets.get(h))
                .map(|asset| {
                    apply_chunk_to_heightmap(&mut heightmap, &asset.0);
                    asset.0.clone()
                });

            let lm = lightmap_src(cx, cy);
            let pr = probes_src(cx, cy);
            let task = thread_pool
                .spawn(async move { compute_chunk_meshes(chunk_data, cx, cy, TILE_SIZE, lm, pr) });

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

            let lm = lightmap_src(cx, cy);
            let pr = probes_src(cx, cy);
            let task = thread_pool
                .spawn(async move { compute_chunk_meshes(None, cx, cy, TILE_SIZE, lm, pr) });

            state.computing.insert(
                chunk_coord,
                ComputingChunk {
                    task,
                    objects_handle: pending.objects_handle,
                },
            );
        }
    }

    #[cfg(all(target_arch = "wasm32", feature = "opfs"))]
    {
        // Collect chunks where both slots are ready.
        let ready: Vec<(i32, i32)> = state
            .pending
            .iter()
            .filter(|(_, p)| {
                p.opfs_terrain.lock().unwrap().is_some()
                    && p.opfs_objects.lock().unwrap().is_some()
                    && p.opfs_lightmap.lock().unwrap().is_some()
                    && p.opfs_probes.lock().unwrap().is_some()
            })
            .map(|(&coord, _)| coord)
            .collect();

        let thread_pool = AsyncComputeTaskPool::get();

        for chunk_coord in ready {
            let Some(pending) = state.pending.remove(&chunk_coord) else {
                continue;
            };
            let (cx, cy) = chunk_coord;

            // Both slots are Some at this point — take the inner values.
            let terrain_data: Option<ChunkData> =
                pending.opfs_terrain.lock().unwrap().take().flatten();
            let objects_data: Option<ChunkObjectsFile> =
                pending.opfs_objects.lock().unwrap().take().flatten();

            // Patch heightmap on the main thread if there is terrain data.
            if let Some(ref data) = terrain_data {
                apply_chunk_to_heightmap(&mut heightmap, data);
            }

            let lm: Option<LightmapSource> = pending.opfs_lightmap.lock().unwrap().take().flatten();
            let pr: Option<ProbeSource> = pending.opfs_probes.lock().unwrap().take().flatten();
            let task = thread_pool.spawn(async move {
                compute_chunk_meshes(terrain_data, cx, cy, TILE_SIZE, lm, pr)
            });

            state
                .computing
                .insert(chunk_coord, ComputingChunk { task, objects_data });
        }
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
    #[cfg(not(all(target_arch = "wasm32", feature = "opfs")))] chunk_objects_assets: Res<
        Assets<ChunkObjectsAsset>,
    >,
    mut state: ResMut<StreamingState>,
    config: Res<ChunkStreamingConfig>,
    chunk_registry: Option<ResMut<TerrainChunkRegistry>>,
    render_assets: Option<Res<TerrainRenderAssets>>,
    mut terrain_materials: ResMut<Assets<ExtendedMaterial<StandardMaterial, TerrainMaterial>>>,
    mut images: ResMut<Assets<Image>>,
    mut obj_registry: ResMut<PlacedObjectRegistry>,
    prefabs: Res<PrefabLibrary>,
    asset_server: Res<AssetServer>,
    mut probe_store: ResMut<ProbeStore>,
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

        let mut result = result;
        if let Some(probes) = result.probes.take() {
            probe_store.insert(probes);
        }
        spawn_chunk_from_result(
            &mut commands,
            &mut meshes,
            &mut chunk_registry,
            &render_assets,
            &mut terrain_materials,
            &mut images,
            result,
        );

        #[cfg(not(all(target_arch = "wasm32", feature = "opfs")))]
        if let Some(ref handle) = computing.objects_handle
            && let Some(asset) = chunk_objects_assets.get(handle)
        {
            spawn_chunk_objects(
                &mut commands,
                &asset_server,
                &mut obj_registry,
                &prefabs,
                &asset.0,
            );
        }

        #[cfg(all(target_arch = "wasm32", feature = "opfs"))]
        if let Some(ref objects) = computing.objects_data {
            spawn_chunk_objects(
                &mut commands,
                &*asset_server,
                &mut obj_registry,
                &prefabs,
                objects,
            );
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
