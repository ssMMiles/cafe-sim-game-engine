use avian3d::prelude::*;
use bevy::{
    camera::primitives::Aabb,
    image::{ImageAddressMode, ImageLoaderSettings, ImageSampler, ImageSamplerDescriptor},
    pbr::ExtendedMaterial,
    prelude::*,
};

use super::{
    CHUNK_SIZE, HEIGHT_SCALE, HeightmapData, TILE_SIZE,
    material::TerrainMaterial,
    mesh::{LOD_LEVELS, generate_lod_mesh},
    storage::{self, ChunkData, HeightData, TerrainChunkRegistry, dequantize_heights},
};

use crate::game::{GameEntity, world::chunk_assets::WorldMetaAsset};

/// Marker indicating terrain has been processed (prevents re-running setup)
#[derive(Component)]
pub struct TerrainProcessed;

/// Marker entity used to trigger terrain setup exactly once.
#[derive(Component)]
pub struct TerrainSetupMarker;

/// Holds the in-flight handle for async `world.meta` loading (native / non-OPFS path).
#[cfg(not(all(target_arch = "wasm32", feature = "opfs")))]
#[derive(Component)]
pub(crate) struct WorldMetaHandle(Handle<WorldMetaAsset>);

/// Holds the OPFS load slot for `world.meta` (wasm+opfs path).
#[cfg(all(target_arch = "wasm32", feature = "opfs"))]
#[derive(Component)]
pub(crate) struct WorldMetaOpfsHandle(
    std::sync::Arc<std::sync::Mutex<Option<Result<Option<Vec<u8>>, String>>>>,
);

/// Shared render assets created once during terrain setup, reused for every spawned chunk.
#[derive(Resource)]
pub struct TerrainRenderAssets {
    pub material: Handle<ExtendedMaterial<StandardMaterial, TerrainMaterial>>,
}

/// Data produced by the background mesh-generation task.
///
/// All fields are `Send + Sync` so the result can be moved back to the main thread.
/// `Collider` is built off the main thread since `parry3d::Shape: DowncastSync` (Send + Sync).
pub struct ChunkComputeResult {
    pub chunk_x: i32,
    pub chunk_y: i32,
    pub lod_meshes: [Mesh; LOD_LEVELS],
    pub aabb: Aabb,
    pub collider: Collider,
    pub collider_offset: Vec3,
}

/// Reconstruct a flat height array for a chunk directly from its saved `ChunkData`,
/// without touching `HeightmapData`. This is designed to run off the main thread.
///
/// Returns a `Vec<f32>` of length `CHUNK_SIZE * CHUNK_SIZE` indexed as
/// `heights[lx * CHUNK_SIZE + ly]`, matching the format expected by `compute_chunk_meshes`.
fn heights_from_chunk_data(chunk_data: Option<&ChunkData>) -> Vec<f32> {
    let total = CHUNK_SIZE * CHUNK_SIZE;
    let mut heights = vec![0.0f32; total];

    let Some(data) = chunk_data else {
        return heights; // flat (unmodified) chunk — base is all zeros
    };

    match &data.height_data {
        HeightData::UseBase => {
            // No stored modifications; the implicit base (0.0) is already set.
        }
        HeightData::Delta(mods) => {
            for m in mods {
                let lx = m.local_x() as usize;
                let ly = m.local_y() as usize;
                if lx < CHUNK_SIZE && ly < CHUNK_SIZE {
                    heights[lx * CHUNK_SIZE + ly] += m.delta;
                }
            }
        }
        HeightData::Full {
            heights: stored,
            height_min,
            height_max,
        } => {
            let dequantized = dequantize_heights(stored, *height_min, *height_max);
            let ch = data.height as usize;
            for lx in 0..data.width as usize {
                for ly in 0..ch {
                    if lx * CHUNK_SIZE + ly < total {
                        heights[lx * CHUNK_SIZE + ly] = dequantized[lx * ch + ly];
                    }
                }
            }
        }
    }

    heights
}

/// Pure function: compute all LOD meshes and prepare heightfield data for a chunk.
///
/// `chunk_data` is the optional saved terrain data for this chunk. Heights are
/// reconstructed from it off the main thread — no `HeightmapData` access required.
///
/// This function is designed to run off the main thread (no Bevy world access).
pub fn compute_chunk_meshes(
    chunk_data: Option<ChunkData>,
    chunk_x: i32,
    chunk_y: i32,
    tile_size: (f32, f32),
) -> ChunkComputeResult {
    let heights = heights_from_chunk_data(chunk_data.as_ref());
    let cs = (CHUNK_SIZE - 1) as i32;
    let start_x = chunk_x * cs;
    let start_y = chunk_y * cs;

    // Closure that maps global tile coords back to the local heights array.
    // Clamps to chunk boundaries so generate_lod_mesh edge-sample behaviour is preserved.
    let get_height = |gx: i32, gy: i32| -> f32 {
        let lx = (gx - start_x).clamp(0, cs) as usize;
        let ly = (gy - start_y).clamp(0, cs) as usize;
        heights[lx * CHUNK_SIZE + ly]
    };

    let mut aabb_out = None;
    let lod_meshes: [Mesh; LOD_LEVELS] = std::array::from_fn(|lod| {
        let (mesh, aabb) = generate_lod_mesh(
            &get_height,
            start_x,
            start_y,
            CHUNK_SIZE,
            CHUNK_SIZE,
            tile_size,
            lod,
        );
        if lod == 0 {
            aabb_out = Some(aabb);
        }
        mesh
    });

    // Build the heightfield collider off the main thread.
    // Downsampled by HF_STRIDE — 64×64 is more than precise enough for character collision
    // and is 16× less data for avian to build its BVH over.
    // parry3d::Shape: DowncastSync (Send + Sync), so Collider is safe to construct here.
    const HF_STRIDE: usize = 4;
    let hf_size = CHUNK_SIZE.div_ceil(HF_STRIDE);
    let hf_heights: Vec<Vec<f32>> = (0..hf_size)
        .map(|hx| {
            let lx = (hx * HF_STRIDE).min(CHUNK_SIZE - 1);
            (0..hf_size)
                .map(|hy| {
                    let ly = (hy * HF_STRIDE).min(CHUNK_SIZE - 1);
                    heights[lx * CHUNK_SIZE + ly]
                })
                .collect()
        })
        .collect();

    let physical_width = (CHUNK_SIZE - 1) as f32 * tile_size.0;
    let physical_depth = (CHUNK_SIZE - 1) as f32 * tile_size.1;
    let collider =
        Collider::heightfield(hf_heights, Vec3::new(physical_width, 1.0, physical_depth));
    let collider_offset = Vec3::new(physical_width * 0.5, 0.0, physical_depth * 0.5);

    ChunkComputeResult {
        chunk_x,
        chunk_y,
        lod_meshes,
        aabb: aabb_out.unwrap(),
        collider,
        collider_offset,
    }
}

/// Spawn a terrain chunk entity from pre-computed mesh data.
///
/// Called on the main thread after background mesh generation completes.
/// Inserts the meshes into `Assets<Mesh>`, builds the avian heightfield collider,
/// and registers the entity in `TerrainChunkRegistry`.
pub fn spawn_chunk_from_result(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    chunk_registry: &mut TerrainChunkRegistry,
    render_assets: &TerrainRenderAssets,
    result: ChunkComputeResult,
) -> Entity {
    let cs = (CHUNK_SIZE - 1) as i32;
    let start_x = result.chunk_x * cs;
    let start_y = result.chunk_y * cs;

    let offset = Vec3::new(
        start_x as f32 * TILE_SIZE.0,
        0.0,
        start_y as f32 * TILE_SIZE.1,
    );

    let lod_mesh_handles: [Handle<Mesh>; LOD_LEVELS] = result.lod_meshes.map(|m| meshes.add(m));

    let entity = commands
        .spawn((
            GameEntity,
            Mesh3d(lod_mesh_handles[0].clone()),
            MeshMaterial3d(render_assets.material.clone()),
            Transform::from_translation(offset),
            GlobalTransform::default(),
            Visibility::Inherited,
            result.aabb,
            super::TerrainChunk {
                chunk_x: result.chunk_x,
                chunk_y: result.chunk_y,
                lod_meshes: lod_mesh_handles,
                current_lod: 0,
            },
            RigidBody::Static,
        ))
        .id();

    commands.entity(entity).with_child((
        result.collider,
        Transform::from_translation(result.collider_offset),
        avian3d::debug_render::DebugRender::none(),
    ));

    chunk_registry
        .loaded_chunks
        .insert((result.chunk_x, result.chunk_y), entity);

    entity
}

/// Two-stage (native) or two-stage (wasm+opfs) setup system.
///
/// **Native stages:**
/// - Stage 1: marker present, no handle → issue AssetServer load for `world.meta`
/// - Stage 2: handle present, not processed → wait for asset, then finalize terrain setup
///
/// **WASM+opfs stages:**
/// - Stage 0: marker present, no OPFS handle → issue `spawn_local` OPFS load
/// - Stage 1: OPFS handle present, not processed → poll slot, then finalize terrain setup
pub fn create_terrain(
    // Stage 1 native: marker present, no handle yet, not processed
    #[cfg(not(all(target_arch = "wasm32", feature = "opfs")))] marker_fresh: Query<
        Entity,
        (
            With<TerrainSetupMarker>,
            Without<WorldMetaHandle>,
            Without<TerrainProcessed>,
        ),
    >,
    // Stage 2 native: handle present, not yet processed
    #[cfg(not(all(target_arch = "wasm32", feature = "opfs")))] marker_loading: Query<
        (Entity, &WorldMetaHandle),
        (With<TerrainSetupMarker>, Without<TerrainProcessed>),
    >,
    // Stage 0 wasm+opfs: marker present, no OPFS slot yet, not processed
    #[cfg(all(target_arch = "wasm32", feature = "opfs"))] marker_fresh: Query<
        Entity,
        (
            With<TerrainSetupMarker>,
            Without<WorldMetaOpfsHandle>,
            Without<TerrainProcessed>,
        ),
    >,
    // Stage 1 wasm+opfs: OPFS slot present, not yet processed
    #[cfg(all(target_arch = "wasm32", feature = "opfs"))] marker_loading: Query<
        (Entity, &WorldMetaOpfsHandle),
        (With<TerrainSetupMarker>, Without<TerrainProcessed>),
    >,
    mut commands: Commands,
    mut terrain_materials: ResMut<Assets<ExtendedMaterial<StandardMaterial, TerrainMaterial>>>,
    #[cfg(not(all(target_arch = "wasm32", feature = "opfs")))] world_meta_assets: Res<
        Assets<WorldMetaAsset>,
    >,
    world_config: Res<storage::WorldConfig>,
    asset_server: Res<AssetServer>,
    #[cfg(all(target_arch = "wasm32", feature = "opfs"))] _storage: Res<crate::storage::Storage>,
) {
    // ── Stage fresh: initiate load ─────────────────────────────────────────────
    for entity in marker_fresh.iter() {
        #[cfg(not(all(target_arch = "wasm32", feature = "opfs")))]
        {
            let path = format!("world://worlds/{}/world.meta", world_config.world_id);
            let handle = asset_server.load::<WorldMetaAsset>(path);
            commands.entity(entity).insert(WorldMetaHandle(handle));
        }

        #[cfg(all(target_arch = "wasm32", feature = "opfs"))]
        {
            use std::sync::{Arc, Mutex};
            use wasm_bindgen_futures::spawn_local;

            let slot: Arc<Mutex<Option<Result<Option<Vec<u8>>, String>>>> =
                Arc::new(Mutex::new(None));
            let key = format!("worlds/{}/world.meta", world_config.world_id);
            let s = crate::storage::wasm::WasmStorage;
            let slot_clone = slot.clone();
            spawn_local(async move {
                let result = s.load(&key).await.map_err(|e| e.to_string());
                *slot_clone.lock().unwrap() = Some(result);
            });
            commands.entity(entity).insert(WorldMetaOpfsHandle(slot));
        }
    }

    // ── Stage loading: wait then finalize ─────────────────────────────────────
    for (entity, meta_handle) in marker_loading.iter() {
        let width = world_config.grid_width as usize;
        let height = world_config.grid_height as usize;

        // Resolve metadata depending on the platform path.
        #[cfg(not(all(target_arch = "wasm32", feature = "opfs")))]
        let metadata_opt: Option<storage::WorldMetadata> = {
            use bevy::asset::LoadState;

            let load_state = asset_server.load_state(meta_handle.0.id());

            // Still in flight — check again next frame.
            if matches!(load_state, LoadState::Loading | LoadState::NotLoaded) {
                continue;
            }

            let metadata = if matches!(load_state, LoadState::Loaded) {
                if let Some(asset) = world_meta_assets.get(&meta_handle.0) {
                    info!("Loaded world metadata for '{}'", world_config.world_id);
                    asset.0.clone()
                } else {
                    warn!(
                        "WorldMetaAsset unexpectedly missing after Loaded state, using fresh metadata"
                    );
                    make_fresh_metadata(&world_config, width, height)
                }
            } else {
                info!(
                    "No saved world.meta for '{}', starting fresh",
                    world_config.world_id
                );
                make_fresh_metadata(&world_config, width, height)
            };

            Some(metadata)
        };

        #[cfg(all(target_arch = "wasm32", feature = "opfs"))]
        let metadata_opt: Option<storage::WorldMetadata> = {
            use super::storage::parse_world_meta_from_bytes;

            // Poll the slot — if None, still waiting.
            let maybe_result = meta_handle.0.lock().unwrap().take();
            let Some(result) = maybe_result else {
                continue;
            };

            let metadata = match result {
                Ok(Some(bytes)) => match parse_world_meta_from_bytes(&bytes) {
                    Ok(meta) => {
                        info!(
                            "Loaded world metadata from OPFS for '{}'",
                            world_config.world_id
                        );
                        meta
                    }
                    Err(e) => {
                        warn!("Failed to parse world.meta from OPFS ({e}), starting fresh");
                        make_fresh_metadata(&world_config, width, height)
                    }
                },
                Ok(None) => {
                    info!(
                        "No saved world.meta in OPFS for '{}', starting fresh",
                        world_config.world_id
                    );
                    make_fresh_metadata(&world_config, width, height)
                }
                Err(e) => {
                    warn!("OPFS world.meta load error ({e}), starting fresh");
                    make_fresh_metadata(&world_config, width, height)
                }
            };

            Some(metadata)
        };

        let Some(metadata) = metadata_opt else {
            continue;
        };

        // world_path is used by native save I/O. On OPFS builds it's kept as a
        // canonical identifier (not used for fs::write).
        let world_path = std::path::PathBuf::from("worlds").join(&world_config.world_id);

        finalize_terrain_setup(
            entity,
            metadata,
            world_path,
            &world_config,
            &mut commands,
            &mut terrain_materials,
            &asset_server,
        );
    }
}

/// Shared finalisation logic: inserts all terrain resources and marks the entity processed.
fn finalize_terrain_setup(
    entity: Entity,
    metadata: storage::WorldMetadata,
    world_path: std::path::PathBuf,
    world_config: &storage::WorldConfig,
    commands: &mut Commands,
    terrain_materials: &mut Assets<ExtendedMaterial<StandardMaterial, TerrainMaterial>>,
    asset_server: &AssetServer,
) {
    let width = world_config.grid_width as usize;
    let height = world_config.grid_height as usize;

    commands.insert_resource(storage::TerrainWorld {
        chunk_state_map: storage::build_chunk_state_map(&metadata),
        metadata,
        world_path,
    });
    commands.insert_resource(TerrainChunkRegistry::default());

    // Sparse heightmap — starts empty (all tiles implicitly 0.0).
    commands.insert_resource(HeightmapData::new(TILE_SIZE));

    // Shared terrain material — created once, reused by every chunk.
    let stone_texture: Handle<Image> =
        asset_server.load_with_settings("textures/terrain.png", |s: &mut ImageLoaderSettings| {
            s.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor {
                address_mode_u: ImageAddressMode::Repeat,
                address_mode_v: ImageAddressMode::Repeat,
                ..ImageSamplerDescriptor::default()
            });
        });

    let material = terrain_materials.add(ExtendedMaterial {
        base: StandardMaterial {
            base_color: Color::srgb(0.81, 0.62, 0.60),
            perceptual_roughness: 1.0,
            metallic: 0.0,
            reflectance: 0.0,
            ..default()
        },
        extension: TerrainMaterial { stone_texture },
    });

    commands.insert_resource(TerrainRenderAssets { material });

    // Mark processed so this system never runs again for this entity.
    commands.entity(entity).insert(TerrainProcessed);

    info!(
        "Terrain setup: {}x{} tile seed, CHUNK_SIZE={}, waiting for streaming to spawn chunks",
        width, height, CHUNK_SIZE
    );
}

fn make_fresh_metadata(
    world_config: &storage::WorldConfig,
    width: usize,
    height: usize,
) -> storage::WorldMetadata {
    storage::create_world_metadata(
        world_config.world_id.clone(),
        width as u32,
        height as u32,
        CHUNK_SIZE as u32,
        TILE_SIZE,
        HEIGHT_SCALE,
    )
}

/// Patch `HeightmapData` with the heights stored in `chunk_data` (pre-scaled form).
/// Called before generating the chunk mesh so seams are correct.
pub(crate) fn apply_chunk_to_heightmap(heightmap: &mut HeightmapData, chunk_data: &ChunkData) {
    let cs = (CHUNK_SIZE - 1) as i32;
    let start_x = chunk_data.chunk_x * cs;
    let start_y = chunk_data.chunk_y * cs;

    match &chunk_data.height_data {
        HeightData::UseBase => {
            // No stored data — the zero-filled base is used as-is.
        }
        HeightData::Delta(mods) => {
            // Deltas are stored pre-scaled; add them directly.
            for m in mods {
                let gx = start_x + m.local_x() as i32;
                let gy = start_y + m.local_y() as i32;
                let current = heightmap.get_tile(gx, gy);
                heightmap.set_tile(gx, gy, current + m.delta);
            }
        }
        HeightData::Full {
            heights: stored,
            height_min,
            height_max,
        } => {
            // Dequantized values are pre-scaled; assign directly.
            let dequantized = dequantize_heights(stored, *height_min, *height_max);
            let ch = chunk_data.height as usize;
            for lx in 0..chunk_data.width as usize {
                for ly in 0..ch {
                    let gx = start_x + lx as i32;
                    let gy = start_y + ly as i32;
                    heightmap.set_tile(gx, gy, dequantized[lx * ch + ly]);
                }
            }
        }
    }
}
