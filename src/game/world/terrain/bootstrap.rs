use avian3d::prelude::*;
use bevy::{
    asset::LoadState,
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

use crate::game::{
    GameEntity,
    world::chunk_assets::WorldMetaAsset,
};

/// Marker indicating terrain has been processed (prevents re-running setup)
#[derive(Component)]
pub struct TerrainProcessed;

/// Marker entity used to trigger terrain setup exactly once.
#[derive(Component)]
pub struct TerrainSetupMarker;

/// Holds the in-flight handle for async `world.meta` loading.
#[derive(Component)]
pub(crate) struct WorldMetaHandle(Handle<WorldMetaAsset>);

/// Shared render assets created once during terrain setup, reused for every spawned chunk.
#[derive(Resource)]
pub struct TerrainRenderAssets {
    pub material: Handle<ExtendedMaterial<StandardMaterial, TerrainMaterial>>,
}

/// Two-stage setup system.
///
/// **Stage 1** (marker present, no handle yet): issues an async load for `world.meta`
/// and attaches a `WorldMetaHandle` to the marker entity.
///
/// **Stage 2** (handle present, not yet processed): waits for the asset to resolve,
/// then inserts all terrain resources (`TerrainWorld`, `TerrainChunkRegistry`,
/// `HeightmapData`, `TerrainRenderAssets`) and marks the entity `TerrainProcessed`.
/// If the load fails (new world), fresh metadata is created instead.
pub fn create_terrain(
    // Stage 1: marker present, no handle yet, not processed
    marker_fresh: Query<
        Entity,
        (With<TerrainSetupMarker>, Without<WorldMetaHandle>, Without<TerrainProcessed>),
    >,
    // Stage 2: handle present, not yet processed
    marker_loading: Query<
        (Entity, &WorldMetaHandle),
        (With<TerrainSetupMarker>, Without<TerrainProcessed>),
    >,
    mut commands: Commands,
    mut terrain_materials: ResMut<Assets<ExtendedMaterial<StandardMaterial, TerrainMaterial>>>,
    world_meta_assets: Res<Assets<WorldMetaAsset>>,
    world_config: Res<storage::WorldConfig>,
    asset_server: Res<AssetServer>,
) {
    // ── Stage 1: issue the world.meta load ────────────────────────────────────
    for entity in marker_fresh.iter() {
        let path = format!("world://worlds/{}/world.meta", world_config.world_id);
        let handle = asset_server.load::<WorldMetaAsset>(path);
        commands.entity(entity).insert(WorldMetaHandle(handle));
    }

    // ── Stage 2: wait for load, then finalise terrain setup ───────────────────
    for (entity, meta_handle) in marker_loading.iter() {
        let load_state = asset_server.load_state(meta_handle.0.id());

        // Still in flight — check again next frame.
        if matches!(load_state, LoadState::Loading | LoadState::NotLoaded) {
            continue;
        }

        let width = world_config.grid_width as usize;
        let height = world_config.grid_height as usize;

        let metadata = if matches!(load_state, LoadState::Loaded) {
            if let Some(asset) = world_meta_assets.get(&meta_handle.0) {
                info!("Loaded world metadata for '{}'", world_config.world_id);
                asset.0.clone()
            } else {
                // Loaded state but asset unavailable — shouldn't happen, but fall back.
                warn!("WorldMetaAsset unexpectedly missing after Loaded state, using fresh metadata");
                make_fresh_metadata(&world_config, width, height)
            }
        } else {
            // Failed (404 = new world, or corrupt file)
            info!(
                "No saved world.meta for '{}', starting fresh",
                world_config.world_id
            );
            make_fresh_metadata(&world_config, width, height)
        };

        let world_path =
            std::path::PathBuf::from("worlds").join(&world_config.world_id);

        commands.insert_resource(storage::TerrainWorld {
            metadata,
            world_path,
        });
        commands.insert_resource(TerrainChunkRegistry::default());

        // Pre-scaled heights stored in HeightmapData (all zero until chunks load).
        commands.insert_resource(HeightmapData {
            heights: vec![0.0f32; width * height],
            grid_w: width,
            grid_h: height,
            tile_size: TILE_SIZE,
        });

        // Shared terrain material — created once, reused by every chunk.
        let stone_texture: Handle<Image> = asset_server.load_with_settings(
            "textures/terrain.png",
            |s: &mut ImageLoaderSettings| {
                s.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor {
                    address_mode_u: ImageAddressMode::Repeat,
                    address_mode_v: ImageAddressMode::Repeat,
                    ..ImageSamplerDescriptor::default()
                });
            },
        );

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
            "Terrain setup: {}x{} tiles, CHUNK_SIZE={}, waiting for streaming to spawn chunks",
            width, height, CHUNK_SIZE
        );
    }
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
fn apply_chunk_to_heightmap(heightmap: &mut HeightmapData, chunk_data: &ChunkData) {
    let start_x = chunk_data.chunk_x as usize * (CHUNK_SIZE - 1);
    let start_y = chunk_data.chunk_y as usize * (CHUNK_SIZE - 1);
    let grid_h = heightmap.grid_h;

    match &chunk_data.height_data {
        HeightData::UseBase => {
            // No stored data — the zero-filled base is used as-is.
        }
        HeightData::Delta(mods) => {
            // Deltas are stored pre-scaled; add them directly to HeightmapData.heights.
            for m in mods {
                let gx = start_x + m.local_x() as usize;
                let gy = start_y + m.local_y() as usize;
                if gx < heightmap.grid_w && gy < heightmap.grid_h {
                    heightmap.heights[gx * grid_h + gy] += m.delta;
                }
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
                    let gx = start_x + lx;
                    let gy = start_y + ly;
                    if gx < heightmap.grid_w && gy < heightmap.grid_h {
                        heightmap.heights[gx * grid_h + gy] = dequantized[lx * ch + ly];
                    }
                }
            }
        }
    }
}

/// Spawn a single terrain chunk entity.
///
/// If `chunk_data` is `Some`, first patches `HeightmapData` with its stored heights.
/// Then generates LOD meshes and an avian heightfield collider from the (now-patched)
/// `HeightmapData`, and spawns the chunk as a root entity with `GameEntity` marker.
///
/// The spawned entity is registered in `TerrainChunkRegistry`.
pub fn spawn_terrain_chunk(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    chunk_registry: &mut TerrainChunkRegistry,
    heightmap: &mut HeightmapData,
    render_assets: &TerrainRenderAssets,
    chunk_x: u32,
    chunk_y: u32,
    chunk_data: Option<&ChunkData>,
) -> Entity {
    let grid_w = heightmap.grid_w;
    let grid_h = heightmap.grid_h;

    // Patch heightmap with stored modifications (if any).
    if let Some(data) = chunk_data {
        apply_chunk_to_heightmap(heightmap, data);
    }

    let start_x = chunk_x as usize * (CHUNK_SIZE - 1);
    let start_y = chunk_y as usize * (CHUNK_SIZE - 1);
    let end_x = (start_x + CHUNK_SIZE).min(grid_w);
    let end_y = (start_y + CHUNK_SIZE).min(grid_h);
    let chunk_width = end_x - start_x;
    let chunk_height = end_y - start_y;

    let offset = Vec3::new(
        start_x as f32 * TILE_SIZE.0,
        0.0,
        start_y as f32 * TILE_SIZE.1,
    );

    // Generate LOD meshes. HeightmapData.heights are pre-scaled, so pass height_scale=1.0.
    let mut chunk_aabb = None;
    let lod_meshes: [Handle<Mesh>; LOD_LEVELS] = std::array::from_fn(|lod| {
        let (mesh, aabb) = generate_lod_mesh(
            &heightmap.heights,
            grid_w,
            grid_h,
            start_x,
            start_y,
            chunk_width,
            chunk_height,
            TILE_SIZE,
            1.0, // heights are already pre-scaled
            lod,
        );
        if lod == 0 {
            chunk_aabb = Some(aabb);
        }
        meshes.add(mesh)
    });
    let aabb = chunk_aabb.unwrap();

    // Build avian heightfield collider.
    // Heights are already pre-scaled (world units), so no additional scaling.
    let hf_heights: Vec<Vec<f32>> = (0..chunk_width)
        .map(|lx| {
            (0..chunk_height)
                .map(|ly| heightmap.heights[(start_x + lx) * grid_h + (start_y + ly)])
                .collect()
        })
        .collect();

    let physical_width = (chunk_width - 1) as f32 * TILE_SIZE.0;
    let physical_depth = (chunk_height - 1) as f32 * TILE_SIZE.1;
    let hf_collider = Collider::heightfield(
        hf_heights,
        Vec3::new(physical_width, 1.0, physical_depth),
    );
    let hf_local_offset = Vec3::new(physical_width * 0.5, 0.0, physical_depth * 0.5);

    let entity = commands
        .spawn((
            GameEntity,
            Mesh3d(lod_meshes[0].clone()),
            MeshMaterial3d(render_assets.material.clone()),
            Transform::from_translation(offset),
            GlobalTransform::default(),
            Visibility::Inherited,
            aabb,
            super::TerrainChunk {
                chunk_x: chunk_x as usize,
                chunk_y: chunk_y as usize,
                lod_meshes,
                current_lod: 0,
            },
            RigidBody::Static,
        ))
        .id();

    commands.entity(entity).with_child((
        hf_collider,
        Transform::from_translation(hf_local_offset),
        avian3d::debug_render::DebugRender::none(),
    ));

    chunk_registry.loaded_chunks.insert((chunk_x, chunk_y), entity);

    entity
}
