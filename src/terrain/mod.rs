use bevy::{
    asset::RenderAssetUsages,
    mesh::{Indices, PrimitiveTopology},
    pbr::ExtendedMaterial,
    prelude::*,
    render::{
        extract_resource::{ExtractResource, ExtractResourcePlugin},
        gpu_readback::{Readback, ReadbackComplete},
        render_asset::RenderAssets,
        render_graph::{self, RenderGraph, RenderLabel},
        render_resource::*,
        renderer::{RenderContext, RenderDevice, RenderQueue},
        storage::{GpuShaderStorageBuffer, ShaderStorageBuffer},
        texture::GpuImage,
        Render, RenderApp, RenderSystems,
    },
};
use image::{DynamicImage, ImageBuffer, Rgba};
use std::{
    borrow::Cow,
    sync::atomic::{AtomicBool, Ordering},
};

use crate::game::camera::PlayerCamera;
use crate::terrain::{
    material::{TerrainConfig, TerrainMaterial},
    mesh::generate_terrain_topology,
};

mod material;
mod mesh;
pub mod storage;

/// Chunk size in vertices (128 vertices = 127 cells per chunk side)
/// Larger chunks = fewer draw calls, better batching
const CHUNK_SIZE: usize = 64;

/// Number of LOD levels per chunk
const LOD_LEVELS: usize = 4;

/// Distance thresholds for LOD switching (in world units)
/// LOD 0 (full detail) used when distance < LOD_DISTANCES[0]
/// LOD 1 used when distance < LOD_DISTANCES[1], etc.
#[allow(dead_code)]
const LOD_DISTANCES: [f32; LOD_LEVELS] = [500.0, 1000.0, 2500.0, 4000.0];

/// Squared distance thresholds for LOD switching (avoids sqrt in hot path)
/// Derived from LOD_DISTANCES: [500^2, 1000^2, 2500^2, 4000^2]
const LOD_DISTANCES_SQ: [f32; LOD_LEVELS] = [
    250_000.0,    // 500^2
    1_000_000.0,  // 1000^2
    6_250_000.0,  // 2500^2
    16_000_000.0, // 4000^2
];

#[derive(Debug, Component)]
pub struct Heightmap(pub Handle<Image>);

/// Marker component for terrain chunk entities
#[derive(Component)]
pub struct TerrainChunk {
    pub chunk_x: usize,
    pub chunk_y: usize,
    /// Mesh handles for each LOD level (index 0 = highest detail)
    pub lod_meshes: [Handle<Mesh>; LOD_LEVELS],
    /// Currently active LOD level
    pub current_lod: usize,
}

/// Tracks camera position for LOD update throttling
#[derive(Resource)]
struct TerrainLodState {
    last_camera_pos: Vec3,
    /// Squared distance threshold - camera must move this far before LOD recalculation
    update_threshold_sq: f32,
}

impl Default for TerrainLodState {
    fn default() -> Self {
        Self {
            last_camera_pos: Vec3::ZERO,
            update_threshold_sq: 15.0 * 15.0, // 15 units movement threshold
        }
    }
}

/// Marker indicating terrain has been processed (prevents re-running setup)
#[derive(Component)]
struct TerrainProcessed;

// Marker components for readback tracking
#[derive(Component)]
struct PositionReadback {
    terrain_entity: Entity,
}

#[derive(Component)]
struct NormalReadback {
    terrain_entity: Entity,
}

/// Component to track terrain chunks waiting for readback to complete
#[derive(Component)]
struct PendingTerrainChunks {
    chunks_x: usize,
    chunks_y: usize,
    width: usize,
    height: usize,
    material_handle: Handle<ExtendedMaterial<StandardMaterial, TerrainMaterial>>,
    positions: Option<Vec<[f32; 3]>>,
    normals: Option<Vec<[f32; 3]>>,
}

// Resource that gets extracted from main world to render world
#[derive(Resource, Clone, ExtractResource)]
pub struct TerrainComputeData {
    pub heightmap: Handle<Image>,
    pub position_buffer: Handle<ShaderStorageBuffer>,
    pub normal_buffer: Handle<ShaderStorageBuffer>,
    pub config: TerrainConfig,
    pub needs_recompute: bool,
}

// Render world resources for compute pipeline
#[derive(Resource)]
struct TerrainComputePipeline {
    bind_group_layout: BindGroupLayoutDescriptor,
    pipeline_id: CachedComputePipelineId,
}

#[derive(Resource)]
struct TerrainComputeBindGroup(BindGroup);

// Render graph label for terrain compute node
#[derive(Debug, Hash, PartialEq, Eq, Clone, RenderLabel)]
struct TerrainComputeLabel;

pub struct HeightmapPlugin;
impl Plugin for HeightmapPlugin {
    fn build(&self, app: &mut App) {
        // Register custom terrain material
        app.add_plugins(MaterialPlugin::<
            ExtendedMaterial<StandardMaterial, TerrainMaterial>,
        >::default());

        // Enable GPU readback for copying compute results to CPU
        // app.add_plugins(GpuReadbackPlugin::default());

        // Extract compute data to render world
        app.add_plugins(ExtractResourcePlugin::<TerrainComputeData>::default());

        // Initialize LOD state resource
        app.init_resource::<TerrainLodState>();

        // Main world systems
        app.add_systems(Update, (create_heightmap_mesh, update_terrain_lod));

        // Readback observers to handle GPU->CPU data transfer
        app.add_observer(handle_position_readback);
        app.add_observer(handle_normal_readback);

        // Setup render world
        let Some(render_app) = app.get_sub_app_mut(RenderApp) else {
            return; // No rendering in headless mode
        };

        render_app
            .add_systems(
                Render,
                setup_compute_pipeline.in_set(RenderSystems::PrepareAssets),
            )
            .add_systems(
                Render,
                prepare_compute_bind_groups.in_set(RenderSystems::PrepareBindGroups),
            );

        // Add compute node to render graph
        let mut render_graph = render_app.world_mut().resource_mut::<RenderGraph>();
        render_graph.add_node(TerrainComputeLabel, TerrainComputeNode::default());
        render_graph.add_node_edge(
            TerrainComputeLabel,
            bevy::render::graph::CameraDriverLabel,
        );
    }
}

fn create_heightmap_mesh(
    heightmap_query: Query<(&Heightmap, Entity), Without<TerrainProcessed>>,
    images: Res<Assets<Image>>,
    mut commands: Commands,
    mut storage_buffers: ResMut<Assets<ShaderStorageBuffer>>,
    mut terrain_materials: ResMut<Assets<ExtendedMaterial<StandardMaterial, TerrainMaterial>>>,
) {
    for (heightmap, entity) in heightmap_query.iter() {
        let Some(image) = images.get(&heightmap.0) else {
            continue;
        };

        let width = image.width() as usize;
        let height = image.height() as usize;
        let vertex_count = width * height;

        // Create storage buffers for compute shader to fill
        let positions = vec![Vec4::ZERO; vertex_count];
        let normals = vec![Vec4::new(0.0, 1.0, 0.0, 0.0); vertex_count];

        // Create buffers with COPY_SRC usage for GPU->CPU readback
        let mut position_buffer_data = ShaderStorageBuffer::from(positions);
        position_buffer_data.buffer_description.usage |= BufferUsages::COPY_SRC;
        let position_buffer = storage_buffers.add(position_buffer_data);

        let mut normal_buffer_data = ShaderStorageBuffer::from(normals);
        normal_buffer_data.buffer_description.usage |= BufferUsages::COPY_SRC;
        let normal_buffer = storage_buffers.add(normal_buffer_data);

        // Create terrain config
        let config = TerrainConfig {
            grid_size: Vec2::new(width as f32, height as f32),
            tile_size: Vec2::new(TILE_SIZE.0, TILE_SIZE.1),
            height_scale: HEIGHT_SCALE,
        };

        // Create material (minimal now - vertex data comes from mesh attributes)
        let terrain_material = TerrainMaterial {
            config: config.clone(),
        };

        let material = terrain_materials.add(ExtendedMaterial {
            base: StandardMaterial {
                base_color: Color::srgb(1.0, 0.1, 0.1),
                perceptual_roughness: 1.0,
                metallic: 0.0,
                reflectance: 0.0,
                ..default()
            },
            extension: terrain_material,
        });

        // Calculate number of chunks (ceiling division)
        let chunks_x = (width + CHUNK_SIZE - 2) / (CHUNK_SIZE - 1);
        let chunks_y = (height + CHUNK_SIZE - 2) / (CHUNK_SIZE - 1);

        info!(
            "Terrain {}x{} will be split into {}x{} = {} chunks",
            width,
            height,
            chunks_x,
            chunks_y,
            chunks_x * chunks_y
        );

        // Insert components (chunks will be spawned after readback completes)
        // Parent needs Visibility for child InheritedVisibility to work properly
        // TerrainProcessed prevents this system from re-running
        commands.entity(entity).insert((
            Transform::default(),
            GlobalTransform::default(),
            Visibility::Inherited,
            TerrainProcessed,
            PendingTerrainChunks {
                chunks_x,
                chunks_y,
                width,
                height,
                material_handle: material,
                positions: None,
                normals: None,
            },
        ));

        // Store compute data (will be extracted to render world)
        commands.insert_resource(TerrainComputeData {
            heightmap: heightmap.0.clone(),
            position_buffer: position_buffer.clone(),
            normal_buffer: normal_buffer.clone(),
            config: config.clone(),
            needs_recompute: true, // Compute on first frame
        });

        // Spawn readback entities to copy GPU data back to CPU after compute
        commands.spawn((
            Readback::buffer(position_buffer),
            PositionReadback {
                terrain_entity: entity,
            },
        ));

        commands.spawn((
            Readback::buffer(normal_buffer),
            NormalReadback {
                terrain_entity: entity,
            },
        ));
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

// ==== READBACK OBSERVERS ====

fn handle_position_readback(
    trigger: On<ReadbackComplete>,
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    readback_query: Query<&PositionReadback>,
    mut pending_query: Query<&mut PendingTerrainChunks>,
) {
    let Ok(readback) = readback_query.get(trigger.entity) else {
        return;
    };

    // Decode positions from GPU data
    let positions_vec4: Vec<Vec4> = trigger.event().to_shader_type();

    // Convert to mesh attribute format [f32; 3]
    let positions: Vec<[f32; 3]> = positions_vec4
        .iter()
        .map(|v| [v.x, v.y, v.z])
        .collect();

    info!("Received {} computed positions from GPU", positions.len());

    // Store positions and check if we can create chunk meshes
    if let Ok(mut pending) = pending_query.get_mut(readback.terrain_entity) {
        pending.positions = Some(positions);
        try_create_terrain_chunks(
            &mut commands,
            &mut meshes,
            readback.terrain_entity,
            &pending,
        );
    }

    // Cleanup: despawn the readback entity (one-time operation)
    commands.entity(trigger.entity).despawn();
}

fn handle_normal_readback(
    trigger: On<ReadbackComplete>,
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    readback_query: Query<&NormalReadback>,
    mut pending_query: Query<&mut PendingTerrainChunks>,
) {
    let Ok(readback) = readback_query.get(trigger.entity) else {
        return;
    };

    // Decode normals from GPU data
    let normals_vec4: Vec<Vec4> = trigger.event().to_shader_type();

    // Convert to mesh attribute format [f32; 3]
    let normals: Vec<[f32; 3]> = normals_vec4
        .iter()
        .map(|v| [v.x, v.y, v.z])
        .collect();

    info!("Received {} computed normals from GPU", normals.len());

    // Store normals and check if we can create chunk meshes
    if let Ok(mut pending) = pending_query.get_mut(readback.terrain_entity) {
        pending.normals = Some(normals);
        try_create_terrain_chunks(
            &mut commands,
            &mut meshes,
            readback.terrain_entity,
            &pending,
        );
    }

    // Cleanup: despawn the readback entity (one-time operation)
    commands.entity(trigger.entity).despawn();
}

/// Create terrain chunk meshes once both positions and normals are available
fn try_create_terrain_chunks(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    terrain_entity: Entity,
    pending: &PendingTerrainChunks,
) {
    let (Some(positions), Some(normals)) = (&pending.positions, &pending.normals) else {
        return;
    };

    info!(
        "Both readbacks complete - creating {} terrain chunks with {} LOD levels",
        pending.chunks_x * pending.chunks_y,
        LOD_LEVELS
    );

    let width = pending.width;
    let height = pending.height;

    // Spawn chunk entities as children of the terrain entity
    commands.entity(terrain_entity).with_children(|parent| {
        for cy in 0..pending.chunks_y {
            for cx in 0..pending.chunks_x {
                // Calculate global vertex range for this chunk
                // Stride is CHUNK_SIZE - 1 so chunks overlap by 1 vertex
                let start_x = cx * (CHUNK_SIZE - 1);
                let start_y = cy * (CHUNK_SIZE - 1);
                let end_x = (start_x + CHUNK_SIZE).min(width);
                let end_y = (start_y + CHUNK_SIZE).min(height);

                let chunk_width = end_x - start_x;
                let chunk_height = end_y - start_y;

                // Extract vertices for this chunk from global arrays
                let mut chunk_positions = Vec::with_capacity(chunk_width * chunk_height);
                let mut chunk_normals = Vec::with_capacity(chunk_width * chunk_height);

                for local_x in 0..chunk_width {
                    for local_y in 0..chunk_height {
                        let global_x = start_x + local_x;
                        let global_y = start_y + local_y;
                        // Global vertex index uses column-major layout: x * height + y
                        let global_idx = global_x * height + global_y;

                        chunk_positions.push(positions[global_idx]);
                        chunk_normals.push(normals[global_idx]);
                    }
                }

                // Generate meshes for all LOD levels
                let mut lod_meshes: [Handle<Mesh>; LOD_LEVELS] =
                    std::array::from_fn(|_| Handle::default());
                let mut aabb_min = Vec3::splat(f32::MAX);
                let mut aabb_max = Vec3::splat(f32::MIN);

                for lod in 0..LOD_LEVELS {
                    let (mesh, min_pos, max_pos) = generate_lod_mesh(
                        &chunk_positions,
                        &chunk_normals,
                        chunk_width,
                        chunk_height,
                        start_x,
                        start_y,
                        width,
                        height,
                        lod,
                    );
                    lod_meshes[lod] = meshes.add(mesh);

                    // Use LOD 0 bounds for AABB (most accurate)
                    if lod == 0 {
                        aabb_min = min_pos;
                        aabb_max = max_pos;
                    }
                }

                // Calculate AABB from LOD 0 vertex positions
                use bevy::camera::primitives::Aabb;
                let aabb = Aabb::from_min_max(aabb_min, aabb_max);

                // Spawn chunk entity with LOD 0 mesh initially
                parent.spawn((
                    Mesh3d(lod_meshes[0].clone()),
                    MeshMaterial3d(pending.material_handle.clone()),
                    Transform::default(),
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
        pending.chunks_x * pending.chunks_y,
        pending.chunks_x,
        pending.chunks_y
    );

    // Remove pending component now that chunks are created
    commands
        .entity(terrain_entity)
        .remove::<PendingTerrainChunks>();
}

/// Generate a terrain chunk mesh at a specific LOD level.
/// Higher LOD levels sample fewer vertices (stride = 2^lod_level).
fn generate_lod_mesh(
    positions: &[[f32; 3]],
    normals: &[[f32; 3]],
    chunk_width: usize,
    chunk_height: usize,
    start_x: usize,
    start_y: usize,
    full_width: usize,
    full_height: usize,
    lod_level: usize,
) -> (Mesh, Vec3, Vec3) {
    let stride = 1 << lod_level;

    let lod_width = (chunk_width + stride - 1) / stride;
    let lod_height = (chunk_height + stride - 1) / stride;

    let mut lod_positions = Vec::with_capacity(lod_width * lod_height);
    let mut lod_normals = Vec::with_capacity(lod_width * lod_height);
    let mut lod_uvs = Vec::with_capacity(lod_width * lod_height);

    let mut min_pos = Vec3::splat(f32::MAX);
    let mut max_pos = Vec3::splat(f32::MIN);

    // --- Sample LOD vertices ---
    for lod_x in 0..lod_width {
        for lod_y in 0..lod_height {
            // Force edge vertices to be the actual chunk boundary to ensure
            // adjacent chunks share identical edge vertices at all LOD levels
            let local_x = if lod_x == lod_width - 1 {
                chunk_width - 1
            } else {
                lod_x * stride
            };
            let local_y = if lod_y == lod_height - 1 {
                chunk_height - 1
            } else {
                lod_y * stride
            };

            let local_idx = local_x * chunk_height + local_y;

            let pos = positions[local_idx];
            lod_positions.push(pos);
            lod_normals.push(normals[local_idx]);

            let global_x = start_x + local_x;
            let global_y = start_y + local_y;
            lod_uvs.push([
                global_x as f32 / (full_width - 1) as f32,
                global_y as f32 / (full_height - 1) as f32,
            ]);

            let pos_vec = Vec3::from_array(pos);
            min_pos = min_pos.min(pos_vec);
            max_pos = max_pos.max(pos_vec);
        }
    }

    let idx = |x: usize, y: usize| -> u32 {
        (x * lod_height + y) as u32
    };

    // --- Grid indices ---
    let mut lod_indices = Vec::new();
    for lod_x in 0..(lod_width - 1) {
        for lod_y in 0..(lod_height - 1) {
            let tl = idx(lod_x, lod_y);
            let tr = idx(lod_x + 1, lod_y);
            let bl = idx(lod_x, lod_y + 1);
            let br = idx(lod_x + 1, lod_y + 1);

            lod_indices.extend_from_slice(&[tl, bl, tr]);
            lod_indices.extend_from_slice(&[tr, bl, br]);
        }
    }

    // =========================
    // ======= SKIRTS ==========
    // =========================

    let skirt_depth = stride as f32 * 5.0;
    let mut skirt_base = Vec::new();

    // Left
    for y in 0..lod_height {
        skirt_base.push(idx(0, y));
    }
    // Bottom
    for x in 1..lod_width {
        skirt_base.push(idx(x, lod_height - 1));
    }
    // Right
    for y in (0..lod_height - 1).rev() {
        skirt_base.push(idx(lod_width - 1, y));
    }
    // Top
    for x in (1..lod_width - 1).rev() {
        skirt_base.push(idx(x, 0));
    }

    let skirt_start = lod_positions.len() as u32;

    for &base_idx in &skirt_base {
        let pos = Vec3::from_array(lod_positions[base_idx as usize]);
        let skirt_pos = pos - Vec3::Y * skirt_depth;

        lod_positions.push(skirt_pos.to_array());
        lod_normals.push([0.0, 0.0, 0.0]);
        lod_uvs.push(lod_uvs[base_idx as usize]);
    }

    let skirt_len = skirt_base.len();
    for i in 0..skirt_len {
        let base0 = skirt_base[i];
        let base1 = skirt_base[(i + 1) % skirt_len];

        let skirt0 = skirt_start + i as u32;
        let skirt1 = skirt_start + ((i + 1) % skirt_len) as u32;

        lod_indices.extend_from_slice(&[
            base0, base1, skirt0,
            skirt0, base1, skirt1,
        ]);
    }

    min_pos.y -= skirt_depth;

    // --- Build mesh ---
    let mut mesh = Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::RENDER_WORLD,
    );

    mesh.insert_indices(Indices::U32(lod_indices));
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, lod_positions);
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, lod_normals);
    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, lod_uvs);

    (mesh, min_pos, max_pos)
}

/// System to update terrain chunk LOD based on camera distance.
/// Optimizations:
/// - Early exit when camera hasn't moved significantly (threshold-based)
/// - Uses squared distances to avoid sqrt operations
/// - Hysteresis to prevent LOD thrashing at boundaries
fn update_terrain_lod(
    camera_query: Query<&GlobalTransform, With<PlayerCamera>>,
    mut chunk_query: Query<(&mut TerrainChunk, &mut Mesh3d, &bevy::camera::primitives::Aabb)>,
    mut lod_state: ResMut<TerrainLodState>,
) {
    let Ok(camera_transform) = camera_query.single() else {
        return;
    };
    let camera_pos = camera_transform.translation();

    // Early exit if camera hasn't moved enough since last LOD update
    let moved_sq = camera_pos.distance_squared(lod_state.last_camera_pos);
    if moved_sq < lod_state.update_threshold_sq {
        return;
    }
    lod_state.last_camera_pos = camera_pos;

    for (mut chunk, mut mesh3d, aabb) in chunk_query.iter_mut() {
        let chunk_center: Vec3 = aabb.center.into();
        let distance_sq = camera_pos.distance_squared(chunk_center);

        // LOD selection with hysteresis to prevent thrashing at boundaries
        let current = chunk.current_lod;
        let new_lod = if current > 0 {
            // To increase detail (lower LOD), require being 10% closer than threshold
            // 0.81 = 0.9^2 (squared because we're comparing squared distances)
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

// ==== RENDER WORLD SYSTEMS ====

fn setup_compute_pipeline(
    mut commands: Commands,
    asset_server: Res<AssetServer>,
    render_device: Res<RenderDevice>,
    mut pipeline_cache: ResMut<PipelineCache>,
    existing_pipeline: Option<Res<TerrainComputePipeline>>,
) {
    // Only setup once
    if existing_pipeline.is_some() {
        return;
    }

    use bevy::render::render_resource::binding_types::*;

    // Create bind group layout descriptor matching compute shader
    let bind_group_layout = BindGroupLayoutDescriptor::new(
        "terrain_compute_layout",
        &BindGroupLayoutEntries::sequential(
            ShaderStages::COMPUTE,
            (
                texture_2d(TextureSampleType::Float { filterable: true }),
                sampler(SamplerBindingType::Filtering),
                uniform_buffer::<TerrainConfig>(false),
                storage_buffer::<Vec4>(false), // positions
                storage_buffer::<Vec4>(false), // normals
            ),
        ),
    );

    // Load compute shader
    let shader = asset_server.load("shaders/terrain_compute.wgsl");

    // Queue pipeline for compilation
    let pipeline_id = pipeline_cache.queue_compute_pipeline(ComputePipelineDescriptor {
        label: Some("terrain_compute_pipeline".into()),
        layout: vec![bind_group_layout.clone()],
        shader,
        shader_defs: vec![],
        entry_point: Some(Cow::Borrowed("compute_terrain")),
        push_constant_ranges: vec![],
        zero_initialize_workgroup_memory: false,
    });

    commands.insert_resource(TerrainComputePipeline {
        bind_group_layout,
        pipeline_id,
    });
}

fn prepare_compute_bind_groups(
    mut commands: Commands,
    pipeline: Option<Res<TerrainComputePipeline>>,
    pipeline_cache: Res<PipelineCache>,
    render_device: Res<RenderDevice>,
    render_queue: Res<RenderQueue>,
    gpu_buffers: Res<RenderAssets<GpuShaderStorageBuffer>>,
    gpu_images: Res<RenderAssets<GpuImage>>,
    terrain_data: Option<Res<TerrainComputeData>>,
    existing_bind_group: Option<Res<TerrainComputeBindGroup>>,
    mut uniform_buffer: Local<Option<UniformBuffer<TerrainConfig>>>,
    mut bind_group_created: Local<bool>,
) {
    let Some(pipeline) = pipeline else { return };
    let Some(terrain_data) = terrain_data else { return };

    // Skip if bind group already exists
    if *bind_group_created && existing_bind_group.is_some() {
        return;
    }

    // Get GPU resources
    let Some(position_buffer) = gpu_buffers.get(&terrain_data.position_buffer) else {
        return;
    };
    let Some(normal_buffer) = gpu_buffers.get(&terrain_data.normal_buffer) else {
        return;
    };
    let Some(heightmap_image) = gpu_images.get(&terrain_data.heightmap) else {
        return;
    };

    // Create or update uniform buffer (only on first run)
    if uniform_buffer.is_none() {
        *uniform_buffer = Some(UniformBuffer::from(terrain_data.config.clone()));
        let uniform = uniform_buffer.as_mut().unwrap();
        uniform.write_buffer(&render_device, &render_queue);
    }

    // Create bind group (only once)
    let bind_group = render_device.create_bind_group(
        Some("terrain_compute_bind_group"),
        &pipeline_cache.get_bind_group_layout(&pipeline.bind_group_layout),
        &BindGroupEntries::sequential((
            &heightmap_image.texture_view,
            &heightmap_image.sampler,
            uniform_buffer.as_ref().unwrap().binding().unwrap(),
            position_buffer.buffer.as_entire_buffer_binding(),
            normal_buffer.buffer.as_entire_buffer_binding(),
        )),
    );

    commands.insert_resource(TerrainComputeBindGroup(bind_group));
    *bind_group_created = true;
}

struct TerrainComputeNode {
    has_computed: AtomicBool,
}

impl Default for TerrainComputeNode {
    fn default() -> Self {
        Self {
            has_computed: AtomicBool::new(false),
        }
    }
}

impl render_graph::Node for TerrainComputeNode {
    fn run(
        &self,
        _graph: &mut render_graph::RenderGraphContext,
        render_context: &mut RenderContext,
        world: &World,
    ) -> Result<(), render_graph::NodeRunError> {
        // Get resources
        let pipeline_cache = world.resource::<PipelineCache>();
        let pipeline = world.get_resource::<TerrainComputePipeline>();
        let bind_group = world.get_resource::<TerrainComputeBindGroup>();
        let terrain_data = world.get_resource::<TerrainComputeData>();

        // Check all resources exist
        let (Some(pipeline), Some(bind_group), Some(terrain_data)) =
            (pipeline, bind_group, terrain_data)
        else {
            return Ok(()); // Not ready yet
        };

        // Skip if already computed once
        if self.has_computed.load(Ordering::Relaxed) {
            return Ok(());
        }

        // Skip if no recompute needed
        if !terrain_data.needs_recompute {
            return Ok(());
        }

        // Get compiled pipeline
        let Some(compute_pipeline) = pipeline_cache.get_compute_pipeline(pipeline.pipeline_id)
        else {
            return Ok(()); // Still compiling
        };

        // Begin compute pass
        let mut pass = render_context
            .command_encoder()
            .begin_compute_pass(&ComputePassDescriptor {
                label: Some("terrain_compute_pass"),
                timestamp_writes: None,
            });

        pass.set_bind_group(0, &bind_group.0, &[]);
        pass.set_pipeline(compute_pipeline);

        // Calculate workgroups (ceil division by workgroup size of 8)
        let width = terrain_data.config.grid_size.x as u32;
        let height = terrain_data.config.grid_size.y as u32;
        let workgroups_x = (width + 7) / 8;
        let workgroups_y = (height + 7) / 8;

        pass.dispatch_workgroups(workgroups_x, workgroups_y, 1);

        // Mark as computed so we don't run again
        drop(pass);
        self.has_computed.store(true, Ordering::Relaxed);

        Ok(())
    }
}
