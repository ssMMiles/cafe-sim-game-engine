use bevy::{image::TextureFormatPixelInfo, pbr::ExtendedMaterial, prelude::*};

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

/// Runtime-queryable heightmap resource with bilinear interpolation.
/// Heights are stored pre-scaled in column-major order: `heights[x * grid_h + y]`.
#[derive(Debug, Resource)]
pub struct HeightmapData {
    pub heights: Vec<f32>,
    pub grid_w: usize,
    pub grid_h: usize,
    pub tile_size: (f32, f32),
}

impl HeightmapData {
    /// Get the pre-scaled height at grid coordinates (x, y).
    pub fn get_tile(&self, x: usize, y: usize) -> Option<f32> {
        if x < self.grid_w && y < self.grid_h {
            Some(self.heights[x * self.grid_h + y])
        } else {
            None
        }
    }

    /// Set the pre-scaled height at grid coordinates (x, y).
    pub fn set_tile(&mut self, x: usize, y: usize, value: f32) {
        if x < self.grid_w && y < self.grid_h {
            self.heights[x * self.grid_h + y] = value;
        }
    }

    /// Bilinear interpolation of terrain height at a world-space (x, z) position.
    /// Returns `None` if the position is outside the heightmap bounds.
    pub fn sample_height(&self, world_x: f32, world_z: f32) -> Option<f32> {
        let fx = world_x / self.tile_size.0;
        let fz = world_z / self.tile_size.1;

        // Need at least one cell of margin for interpolation
        if fx < 0.0 || fz < 0.0 {
            return None;
        }

        let ix = fx as usize;
        let iz = fz as usize;

        if ix + 1 >= self.grid_w || iz + 1 >= self.grid_h {
            // Allow sampling exactly on the last grid line
            if ix < self.grid_w && iz < self.grid_h && fx.fract() < f32::EPSILON && fz.fract() < f32::EPSILON {
                return self.get_tile(ix, iz);
            }
            return None;
        }

        let tx = fx - ix as f32;
        let tz = fz - iz as f32;

        let h00 = self.heights[ix * self.grid_h + iz];
        let h10 = self.heights[(ix + 1) * self.grid_h + iz];
        let h01 = self.heights[ix * self.grid_h + (iz + 1)];
        let h11 = self.heights[(ix + 1) * self.grid_h + (iz + 1)];

        let h = h00 * (1.0 - tx) * (1.0 - tz)
            + h10 * tx * (1.0 - tz)
            + h01 * (1.0 - tx) * tz
            + h11 * tx * tz;

        Some(h)
    }

    /// Sample terrain surface normal and slope angle (radians) at a world-space position.
    /// Uses central differences with a one-tile epsilon.
    pub fn sample_normal(&self, world_x: f32, world_z: f32) -> Option<(Vec3, f32)> {
        let eps_x = self.tile_size.0;
        let eps_z = self.tile_size.1;

        let h_left = self.sample_height(world_x - eps_x, world_z)?;
        let h_right = self.sample_height(world_x + eps_x, world_z)?;
        let h_back = self.sample_height(world_x, world_z - eps_z)?;
        let h_front = self.sample_height(world_x, world_z + eps_z)?;

        let dx = (h_right - h_left) / (2.0 * eps_x);
        let dz = (h_front - h_back) / (2.0 * eps_z);

        let normal = Vec3::new(-dx, 1.0, -dz).normalize();
        let slope = normal.y.acos();

        Some((normal, slope))
    }
}
