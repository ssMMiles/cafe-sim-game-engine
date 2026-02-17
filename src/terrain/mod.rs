use bevy::{image::TextureFormatPixelInfo, pbr::ExtendedMaterial, prelude::*};
use image::{DynamicImage, ImageBuffer, Rgba};

use crate::game::camera::PlayerCamera;
use crate::terrain::{
    material::TerrainMaterial,
    mesh::{generate_lod_mesh, LOD_LEVELS},
};

mod material;
mod mesh;
pub mod storage;

/// Chunk size in vertices per side (overlap of 1 between adjacent chunks)
const CHUNK_SIZE: usize = 64;

/// Squared distance thresholds for LOD switching (avoids sqrt in hot path)
const LOD_DISTANCES_SQ: [f32; LOD_LEVELS] = [
    250_000.0,    // 500^2
    1_000_000.0,  // 1000^2
    6_250_000.0,  // 2500^2
    16_000_000.0, // 4000^2
];

const TILE_SIZE: (f32, f32) = (1.0, 1.0);
const HEIGHT_SCALE: f32 = 10.0;

#[derive(Debug, Component)]
pub struct Heightmap(pub Handle<Image>);

/// Marker component for terrain chunk entities
#[derive(Component)]
pub struct TerrainChunk {
    pub chunk_x: usize,
    pub chunk_y: usize,
    pub lod_meshes: [Handle<Mesh>; LOD_LEVELS],
    pub current_lod: usize,
}

/// Tracks camera position for LOD update throttling
#[derive(Resource)]
struct TerrainLodState {
    last_camera_pos: Vec3,
    update_threshold_sq: f32,
}

impl Default for TerrainLodState {
    fn default() -> Self {
        Self {
            last_camera_pos: Vec3::ZERO,
            update_threshold_sq: 15.0 * 15.0,
        }
    }
}

/// Marker indicating terrain has been processed (prevents re-running setup)
#[derive(Component)]
struct TerrainProcessed;

pub struct HeightmapPlugin;
impl Plugin for HeightmapPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(MaterialPlugin::<
            ExtendedMaterial<StandardMaterial, TerrainMaterial>,
        >::default());

        app.init_resource::<TerrainLodState>();

        app.add_systems(Update, (create_heightmap_mesh, update_terrain_lod));
    }
}

fn create_heightmap_mesh(
    heightmap_query: Query<(&Heightmap, Entity), Without<TerrainProcessed>>,
    images: Res<Assets<Image>>,
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut terrain_materials: ResMut<Assets<ExtendedMaterial<StandardMaterial, TerrainMaterial>>>,
) {
    for (heightmap, entity) in heightmap_query.iter() {
        let Some(image) = images.get(&heightmap.0) else {
            continue;
        };

        let width = image.width() as usize;
        let height = image.height() as usize;

        // Read pixel data into a flat height grid (column-major: heights[x * height + y])
        let heights: Vec<f32> = {
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
                    let lod_meshes: [Handle<Mesh>; LOD_LEVELS] =
                        std::array::from_fn(|lod| {
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
                        TerrainChunk {
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

/// System to update terrain chunk LOD based on camera distance.
fn update_terrain_lod(
    camera_query: Query<&GlobalTransform, With<PlayerCamera>>,
    mut chunk_query: Query<(
        &mut TerrainChunk,
        &mut Mesh3d,
        &bevy::camera::primitives::Aabb,
        &GlobalTransform,
    )>,
    mut lod_state: ResMut<TerrainLodState>,
) {
    let Ok(camera_transform) = camera_query.single() else {
        return;
    };
    let camera_pos = camera_transform.translation();

    let moved_sq = camera_pos.distance_squared(lod_state.last_camera_pos);
    if moved_sq < lod_state.update_threshold_sq {
        return;
    }
    lod_state.last_camera_pos = camera_pos;

    for (mut chunk, mut mesh3d, aabb, global_transform) in chunk_query.iter_mut() {
        // AABB center is in local space; transform to world space
        let local_center: Vec3 = aabb.center.into();
        let chunk_center = global_transform.transform_point(local_center);
        let distance_sq = camera_pos.distance_squared(chunk_center);

        let current = chunk.current_lod;
        let new_lod = if current > 0 {
            let threshold_sq = LOD_DISTANCES_SQ[current - 1] * 0.81;
            if distance_sq < threshold_sq {
                current - 1
            } else if current < LOD_LEVELS - 1 && distance_sq >= LOD_DISTANCES_SQ[current] {
                current + 1
            } else {
                current
            }
        } else if distance_sq >= LOD_DISTANCES_SQ[0] {
            1
        } else {
            0
        };

        if new_lod != chunk.current_lod {
            mesh3d.0 = chunk.lod_meshes[new_lod].clone();
            chunk.current_lod = new_lod;
        }
    }
}

#[derive(Debug, Component, Reflect)]
pub struct HeightmapData {
    pub tiles: Vec<Vec<f32>>,
    pub tile_size: (f32, f32),
    pub height_scale: f32,
    pub width: usize,
    pub height: usize,
}

impl HeightmapData {
    pub fn from_image(image: &Image) -> HeightmapData {
        let width = image.width();
        let height = image.height();

        let mut tiles = vec![vec![0.; height as usize]; width as usize];

        if let Some(data) = &image.data {
            for x in 0..width {
                let offset = (x * width * 4) as usize;
                for y in 0..height {
                    let pixel_offset = offset + (y * 4) as usize;
                    let r = data[pixel_offset] as f32;
                    tiles[x as usize][y as usize] = r / 255.0 * 10.0;
                }
            }
        }

        HeightmapData {
            tiles,
            width: width as usize,
            height: height as usize,
            height_scale: HEIGHT_SCALE,
            tile_size: TILE_SIZE,
        }
    }

    pub fn save_to_file(&self, path: &str) {
        let mut buffer = ImageBuffer::new(self.width as u32, self.height as u32);

        for (x, y, pixel) in buffer.enumerate_pixels_mut() {
            let val = 255 - (self.tiles[x as usize][y as usize] * 50.0) as u8;
            *pixel = Rgba([val, val, val, 255]);
        }

        let image = DynamicImage::from(buffer);
        image.save(path).unwrap();
    }

    pub fn flat(width: usize, height: usize) -> HeightmapData {
        let tiles = vec![vec![0.0; height]; width];

        HeightmapData {
            tiles,
            width,
            height,
            height_scale: HEIGHT_SCALE,
            tile_size: TILE_SIZE,
        }
    }

    pub fn get_tile(&self, x: usize, y: usize) -> Option<f32> {
        if x < self.width && y < self.height {
            Some(self.tiles[x][y])
        } else {
            None
        }
    }

    pub fn set_tile(&mut self, x: usize, y: usize, value: f32) {
        if x < self.width && y < self.height {
            self.tiles[x][y] = value;
        }
    }

    pub fn get_tile_from_change(
        &self,
        point: (usize, usize),
        change: (isize, isize),
    ) -> Option<f32> {
        let (x, y) = point;
        let (dx, dy) = change;

        let neighbouring_point = (x as isize + dx, y as isize + dy);
        if neighbouring_point.0 < 0 || neighbouring_point.1 < 0 {
            return None;
        }

        if neighbouring_point.0 >= self.width as isize
            || neighbouring_point.1 >= self.height as isize
        {
            return None;
        }

        Some(self.tiles[neighbouring_point.0 as usize][neighbouring_point.1 as usize])
    }
}

impl Default for HeightmapData {
    fn default() -> Self {
        HeightmapData::flat(25, 25)
    }
}
