use bevy::{
    asset::RenderAssetUsages,
    camera::primitives::Aabb,
    mesh::{Indices, PrimitiveTopology},
    pbr::ExtendedMaterial,
    prelude::*,
};
use image::{DynamicImage, ImageBuffer, Rgba};

use crate::terrain::{
    material::{TerrainConfig, TerrainMaterial},
    mesh::{calculate_terrain_aabb, generate_terrain_topology},
};

mod material;
mod mesh;

#[derive(Debug, Component)]
pub struct Heightmap(pub Handle<Image>);

pub struct HeightmapPlugin;
impl Plugin for HeightmapPlugin {
    fn build(&self, app: &mut App) {
        // register custom terrainmaterial
        app.add_plugins(MaterialPlugin::<
            ExtendedMaterial<StandardMaterial, TerrainMaterial>,
        >::default());
        app.add_systems(Update, create_heightmap_mesh);
    }
}

fn create_heightmap_mesh(
    heightmap_data: Query<(&Heightmap, Entity), Without<Mesh3d>>,
    images: Res<Assets<Image>>,
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut terrain_materials: ResMut<Assets<ExtendedMaterial<StandardMaterial, TerrainMaterial>>>,
) {
    for (image_handle, entity) in heightmap_data.iter() {
        if let Some(image) = images.get(&image_handle.0) {
            let heightmap_data = HeightmapData::from_image(&image);

            // let mesh = generate_heightmap_mesh_naive(&heightmap_data);
            // let material = StandardMaterial {
            //     base_color: Color::WHITE,
            //     ..Default::default()
            // };
            
            let indices_mesh = heightmap_data.to_mesh();
            let terrain_material = TerrainMaterial {
                heightmap: image_handle.0.clone(),
                config: TerrainConfig {
                    grid_size: Vec2::new(heightmap_data.width as f32, heightmap_data.height as f32),
                    tile_size : Vec2::new(
                        heightmap_data.tile_size.0,
                        heightmap_data.tile_size.1,
                    ),
                    height_scale: heightmap_data.height_scale,
                },
            };

            // Calculate proper AABB for frustum culling
            let aabb = calculate_terrain_aabb(
                heightmap_data.width,
                heightmap_data.height,
                heightmap_data.tile_size,
                heightmap_data.height_scale,
            );

            commands.entity(entity).insert((
                heightmap_data,
                Mesh3d(meshes.add(indices_mesh)),
                MeshMaterial3d(terrain_materials.add(ExtendedMaterial {
                    base: StandardMaterial {
                        base_color: Color::srgb(1.0, 0.1, 0.1),
                        perceptual_roughness: 1.0,
                        metallic: 0.0,
                        reflectance: 0.0,
                        ..Default::default()
                    },
                    extension: terrain_material,
                })),
                Transform::default(),
                GlobalTransform::default(),
                aabb,
            ));
        }
    }
}

fn generate_heightmap_mesh_naive(heightmap_data: &HeightmapData) -> Mesh {
    let width = heightmap_data.width;
    let height = heightmap_data.height;
    let tile_size = heightmap_data.tile_size;

    let mut positions = Vec::new();
    let mut normals = Vec::new();
    let mut uvs = Vec::new();
    let mut indices = Vec::new();

    for x in 0..width {
        for y in 0..height {
            let height_value = heightmap_data.tiles[x][y];
            positions.push([x as f32 * tile_size.0, height_value, y as f32 * tile_size.1]);
            normals.push([0.0, 1.0, 0.0]);
            uvs.push([
                x as f32 / (width - 1) as f32,
                y as f32 / (height - 1) as f32,
            ]);
        }
    }

    for x in 0..(width - 1) {
        for y in 0..(height - 1) {
            let top_left = (x * height + y) as u32;
            let top_right = ((x + 1) * height + y) as u32;
            let bottom_left = (x * height + (y + 1)) as u32;
            let bottom_right = ((x + 1) * height + (y + 1)) as u32;

            indices.push(top_left);
            indices.push(bottom_left);
            indices.push(top_right);

            indices.push(top_right);
            indices.push(bottom_left);
            indices.push(bottom_right);
        }
    }

    let mut mesh = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::all());
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, normals);
    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, uvs);
    mesh.insert_indices(Indices::U32(indices));

    mesh
}

const TILE_SIZE: (f32, f32) = (1.0, 1.0);
const HEIGHT_SCALE: f32 = 10.0;

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
                    tiles[x as usize][y as usize] = r / 255.0 * 10.0; // Scale height
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

    pub fn to_mesh(&self) -> Mesh {
        generate_terrain_topology(self.width, self.height, self.tile_size, self.height_scale)
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
        let mut tiles = vec![vec![0.0; height]; width];

        for x in 0..width {
            for y in 0..height {
                tiles[x as usize][y as usize] = 0.0;
            }
        }

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
