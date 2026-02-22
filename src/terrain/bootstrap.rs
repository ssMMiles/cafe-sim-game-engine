use bevy::{image::TextureFormatPixelInfo, pbr::ExtendedMaterial, prelude::*};

use super::{
    CHUNK_SIZE, HEIGHT_SCALE, Heightmap, HeightmapData, TILE_SIZE,
    material::TerrainMaterial,
    mesh::{LOD_LEVELS, generate_lod_mesh},
    reconcile::apply_stored_modifications,
    storage,
};

/// Marker indicating terrain has been processed (prevents re-running setup)
#[derive(Component)]
struct TerrainProcessed;

pub(crate) fn create_heightmap_mesh(
    heightmap_query: Query<(&Heightmap, Entity), Without<TerrainProcessed>>,
    images: Res<Assets<Image>>,
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut terrain_materials: ResMut<Assets<ExtendedMaterial<StandardMaterial, TerrainMaterial>>>,
    world_config: Res<storage::WorldConfig>,
) {
    for (heightmap, entity) in heightmap_query.iter() {
        let Some(image) = images.get(&heightmap.0) else {
            continue;
        };

        let width = image.width() as usize;
        let height = image.height() as usize;

        // Read pixel data into a flat height grid (column-major: heights[x * height + y])
        // Heights are un-scaled (0.0–1.0) at this stage; HEIGHT_SCALE is applied later.
        let mut heights: Vec<f32> = {
            let data = image.data.as_ref().expect("Heightmap image has no data");
            let pixel_size = image
                .texture_descriptor
                .format
                .pixel_size()
                .expect("Unsupported texture format for heightmap");
            let mut grid = vec![0.0f32; width * height];
            for x in 0..width {
                for y in 0..height {
                    let pixel_idx = y * width + x; // image is row-major
                    let byte_offset = pixel_idx * pixel_size;
                    let r = data[byte_offset] as f32 / 255.0;
                    grid[x * height + y] = r;
                }
            }
            grid
        };

        // Load saved terrain modifications (if any) and apply them on top of the base heights
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
                        world_config.heightmap_path.clone(),
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
                world_config.heightmap_path.clone(),
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
            base_heightmap: heightmap.0.clone(),
        });
        commands.insert_resource(storage::TerrainChunkRegistry::default());

        commands.insert_resource(HeightmapData {
            heights: heights.iter().map(|h| h * HEIGHT_SCALE).collect(),
            grid_w: width,
            grid_h: height,
            tile_size: TILE_SIZE,
        });

        let material = terrain_materials.add(ExtendedMaterial {
            base: StandardMaterial {
                base_color: Color::srgb(1.0, 0.1, 0.1),
                perceptual_roughness: 1.0,
                metallic: 0.0,
                reflectance: 0.0,
                ..default()
            },
            extension: TerrainMaterial {},
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
}
