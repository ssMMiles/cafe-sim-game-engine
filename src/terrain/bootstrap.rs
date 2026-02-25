use bevy::{
    image::{ImageAddressMode, ImageLoaderSettings, ImageSampler, ImageSamplerDescriptor},
    pbr::ExtendedMaterial,
    prelude::*,
};

use super::{
    CHUNK_SIZE, HEIGHT_SCALE, HeightmapData, TILE_SIZE,
    material::TerrainMaterial,
    mesh::{LOD_LEVELS, generate_lod_mesh},
    reconcile::apply_stored_modifications,
    storage,
};

/// Marker indicating terrain has been processed (prevents re-running setup)
#[derive(Component)]
pub struct TerrainProcessed;

/// Marker entity used to trigger terrain setup exactly once.
#[derive(Component)]
pub struct TerrainSetupMarker;

pub(crate) fn create_terrain(
    marker_query: Query<Entity, (With<TerrainSetupMarker>, Without<TerrainProcessed>)>,
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut terrain_materials: ResMut<Assets<ExtendedMaterial<StandardMaterial, TerrainMaterial>>>,
    world_config: Res<storage::WorldConfig>,
    asset_server: Res<AssetServer>,
) {
    let Ok(entity) = marker_query.single() else {
        return;
    };

    let width = world_config.grid_width as usize;
    let height = world_config.grid_height as usize;

    // Start with a flat world — all heights at zero (un-scaled)
    let mut heights = vec![0.0f32; width * height];

    // Load saved terrain modifications (if any) and apply them on top
    let world_path = std::path::PathBuf::from("worlds").join(&world_config.world_id);
    let metadata = if storage::world_exists(&world_path) {
        match storage::load_world(&world_path) {
            Ok(meta) => {
                apply_stored_modifications(&mut heights, &meta, width, height, &world_path);
                info!("Loaded terrain modifications from {:?}", world_path);
                meta
            }
            Err(e) => {
                warn!("Could not load world metadata, starting fresh: {e}");
                storage::create_world_metadata(
                    world_config.world_id.clone(),
                    width as u32,
                    height as u32,
                    CHUNK_SIZE as u32,
                    TILE_SIZE,
                    HEIGHT_SCALE,
                )
            }
        }
    } else {
        storage::create_world_metadata(
            world_config.world_id.clone(),
            width as u32,
            height as u32,
            CHUNK_SIZE as u32,
            TILE_SIZE,
            HEIGHT_SCALE,
        )
    };

    commands.insert_resource(storage::TerrainWorld {
        metadata,
        world_path,
    });
    commands.insert_resource(storage::TerrainChunkRegistry::default());

    commands.insert_resource(HeightmapData {
        heights: heights.iter().map(|h| h * HEIGHT_SCALE).collect(),
        grid_w: width,
        grid_h: height,
        tile_size: TILE_SIZE,
    });

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

    let chunks_x = (width + CHUNK_SIZE - 2) / (CHUNK_SIZE - 1);
    let chunks_y = (height + CHUNK_SIZE - 2) / (CHUNK_SIZE - 1);

    info!(
        "Terrain {}x{} → {}x{} = {} chunks",
        width,
        height,
        chunks_x,
        chunks_y,
        chunks_x * chunks_y
    );

    commands.entity(entity).insert((
        Transform::default(),
        GlobalTransform::default(),
        Visibility::Inherited,
        TerrainProcessed,
    ));

    commands.entity(entity).with_children(|parent| {
        for cy in 0..chunks_y {
            for cx in 0..chunks_x {
                let start_x = cx * (CHUNK_SIZE - 1);
                let start_y = cy * (CHUNK_SIZE - 1);
                let end_x = (start_x + CHUNK_SIZE).min(width);
                let end_y = (start_y + CHUNK_SIZE).min(height);

                let chunk_width = end_x - start_x;
                let chunk_height = end_y - start_y;

                // World-space offset for this chunk
                let offset = Vec3::new(
                    start_x as f32 * TILE_SIZE.0,
                    0.0,
                    start_y as f32 * TILE_SIZE.1,
                );

                let mut chunk_aabb = None;
                let lod_meshes: [Handle<Mesh>; LOD_LEVELS] = std::array::from_fn(|lod| {
                    let (mesh, aabb) = generate_lod_mesh(
                        &heights,
                        width,
                        height,
                        start_x,
                        start_y,
                        chunk_width,
                        chunk_height,
                        TILE_SIZE,
                        HEIGHT_SCALE,
                        lod,
                    );
                    if lod == 0 {
                        chunk_aabb = Some(aabb);
                    }
                    meshes.add(mesh)
                });

                let aabb = chunk_aabb.unwrap();

                parent.spawn((
                    Mesh3d(lod_meshes[0].clone()),
                    MeshMaterial3d(material.clone()),
                    Transform::from_translation(offset),
                    Visibility::Inherited,
                    aabb,
                    super::TerrainChunk {
                        chunk_x: cx,
                        chunk_y: cy,
                        lod_meshes,
                        current_lod: 0,
                    },
                ));
            }
        }
    });

    info!(
        "Created {} terrain chunks ({} x {})",
        chunks_x * chunks_y,
        chunks_x,
        chunks_y
    );
}
