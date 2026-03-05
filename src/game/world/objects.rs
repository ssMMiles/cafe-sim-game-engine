//! World object placement and persistence system.
//!
//! Objects are stored per-chunk alongside terrain data:
//! `worlds/{id}/chunks/chunk_X_Y.objects`

use avian3d::prelude::*;
use bevy::prelude::*;
use rkyv::{Archive, Deserialize, Serialize, rancor::Error as RkyvError};
use std::{
    collections::{HashMap, HashSet},
    fs::{self, File},
    io::{Read, Write as IoWrite},
    path::{Path, PathBuf},
};

use crate::game::{
    GameEntity,
    interaction::{HoveredOver, Interactable, spawn_interactable_scene},
    world::terrain::storage::StorageError,
};

use crate::game::world::terrain::storage::TerrainWorld;

// File format constants
const CHUNK_OBJECTS_MAGIC: &[u8; 4] = b"COBJ";
const CHUNK_OBJECTS_VERSION: u32 = 1;

// Chunk world size in units: (CHUNK_SIZE - 1) * TILE_SIZE = 255 * 0.25
const CHUNK_WORLD_SIZE: f32 = 63.75;

// ============================================================================
// Serializable Data Structures
// ============================================================================

#[derive(Archive, Deserialize, Serialize, Clone, Debug)]
pub struct PlacedObjectRecord {
    pub id: u64,
    pub asset_path: String,
    pub position: [f32; 3],
    pub rotation: [f32; 4], // quaternion xyzw
    pub scale: [f32; 3],
}

#[derive(Archive, Deserialize, Serialize, Clone, Debug)]
pub struct ChunkObjectsFile {
    pub version: u32,
    pub chunk_x: u32,
    pub chunk_y: u32,
    pub objects: Vec<PlacedObjectRecord>,
}

// ============================================================================
// Runtime Types
// ============================================================================

/// Attached to each placed object entity.
#[derive(Component)]
pub struct PlacedObject {
    pub id: u64,
    pub asset_path: String,
    pub chunk: (u32, u32),
}

/// Global registry of all placed objects.
#[derive(Resource, Default)]
pub struct PlacedObjectRegistry {
    pub by_id: HashMap<u64, Entity>,
    pub next_id: u64,
    pub dirty_chunks: HashSet<(u32, u32)>,
}

impl PlacedObjectRegistry {
    pub fn alloc_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }
}

/// One-shot guard: set to true once objects have been loaded for this session.
#[derive(Resource, Default)]
pub struct ObjectsLoaded(pub bool);

/// Configuration for the currently-selected placement object.
#[derive(Resource)]
pub struct PlacementConfig {
    pub asset_path: String,
    pub scale: Vec3,
}

impl Default for PlacementConfig {
    fn default() -> Self {
        Self {
            asset_path: "meshes/mountain/mountain.gltf#Scene0".to_string(),
            scale: Vec3::splat(0.75),
        }
    }
}

// ============================================================================
// Chunk Math
// ============================================================================

pub fn world_pos_to_chunk(world_x: f32, world_z: f32) -> (u32, u32) {
    let cx = (world_x / CHUNK_WORLD_SIZE).floor().max(0.0) as u32;
    let cy = (world_z / CHUNK_WORLD_SIZE).floor().max(0.0) as u32;
    (cx, cy)
}

// ============================================================================
// Spawn Helper
// ============================================================================

pub(super) fn spawn_placed_object(
    commands: &mut Commands,
    asset_server: &AssetServer,
    registry: &mut PlacedObjectRegistry,
    asset_path: String,
    id: u64,
    chunk: (u32, u32),
    transform: Transform,
) -> Entity {
    let handle: Handle<Scene> = asset_server.load(&asset_path);
    let entity = spawn_interactable_scene(
        commands,
        (
            GameEntity,
            PlacedObject {
                id,
                asset_path,
                chunk,
            },
            Interactable {
                name: "Object".to_string(),
                distance: 5.0,
            },
            SceneRoot(handle),
            transform,
            GlobalTransform::default(),
            RigidBody::Static,
            ColliderConstructorHierarchy::new(ColliderConstructor::TrimeshFromMesh),
        ),
    );
    registry.by_id.insert(id, entity);
    registry.dirty_chunks.insert(chunk);
    entity
}

// ============================================================================
// Systems
// ============================================================================

/// Runs once after TerrainWorld is available. Scans `chunks/*.objects` and
/// spawns all previously-saved placed objects.
pub fn load_placed_objects(
    mut commands: Commands,
    terrain_world: Option<Res<TerrainWorld>>,
    asset_server: Res<AssetServer>,
    mut registry: ResMut<PlacedObjectRegistry>,
    mut loaded: ResMut<ObjectsLoaded>,
) {
    if loaded.0 || terrain_world.is_none() {
        return;
    }
    loaded.0 = true;

    let world_path = &terrain_world.unwrap().world_path;
    let chunks_dir = world_path.join("chunks");

    let Ok(entries) = fs::read_dir(&chunks_dir) else {
        info!(
            "No chunks directory found at {:?} — skipping object load",
            chunks_dir
        );
        return;
    };

    let mut max_id: u64 = 0;

    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("objects") {
            continue;
        }

        match load_chunk_objects_file(&path) {
            Ok(file) => {
                for record in &file.objects {
                    let transform = Transform {
                        translation: Vec3::from(record.position),
                        rotation: Quat::from_xyzw(
                            record.rotation[0],
                            record.rotation[1],
                            record.rotation[2],
                            record.rotation[3],
                        ),
                        scale: Vec3::from(record.scale),
                    };

                    let chunk = (file.chunk_x, file.chunk_y);
                    let handle: Handle<Scene> = asset_server.load(&record.asset_path);
                    let entity = spawn_interactable_scene(
                        &mut commands,
                        (
                            GameEntity,
                            PlacedObject {
                                id: record.id,
                                asset_path: record.asset_path.clone(),
                                chunk,
                            },
                            Interactable {
                                name: "Object".to_string(),
                                distance: 5.0,
                            },
                            SceneRoot(handle),
                            transform,
                            GlobalTransform::default(),
                            RigidBody::Static,
                            ColliderConstructorHierarchy::new(ColliderConstructor::TrimeshFromMesh),
                        ),
                    );
                    registry.by_id.insert(record.id, entity);
                    if record.id > max_id {
                        max_id = record.id;
                    }
                }
            }
            Err(e) => error!("Failed to load objects file {:?}: {}", path, e),
        }
    }

    // next_id must be above all loaded ids
    registry.next_id = max_id + 1;
    info!("Loaded placed objects; next_id = {}", registry.next_id);
}

/// Saves all dirty object chunks on Shift+S, alongside terrain saving.
pub fn save_placed_objects(
    input: Res<ButtonInput<KeyCode>>,
    terrain_world: Option<Res<TerrainWorld>>,
    mut registry: ResMut<PlacedObjectRegistry>,
    placed_query: Query<(&PlacedObject, &Transform)>,
) {
    if !(input.just_pressed(KeyCode::KeyS)
        && (input.pressed(KeyCode::ShiftLeft) || input.pressed(KeyCode::ShiftRight)))
    {
        return;
    }

    let Some(terrain_world) = terrain_world else {
        warn!("Cannot save objects: world not loaded");
        return;
    };

    if registry.dirty_chunks.is_empty() {
        return;
    }

    let dirty: Vec<(u32, u32)> = registry.dirty_chunks.drain().collect();
    info!("Saving {} dirty object chunk(s)", dirty.len());

    for (cx, cy) in dirty {
        // Collect all objects belonging to this chunk
        let objects: Vec<PlacedObjectRecord> = placed_query
            .iter()
            .filter(|(obj, _)| obj.chunk == (cx, cy))
            .map(|(obj, transform)| {
                let r = transform.rotation;
                PlacedObjectRecord {
                    id: obj.id,
                    asset_path: obj.asset_path.clone(),
                    position: transform.translation.into(),
                    rotation: [r.x, r.y, r.z, r.w],
                    scale: transform.scale.into(),
                }
            })
            .collect();

        let file = ChunkObjectsFile {
            version: 1,
            chunk_x: cx,
            chunk_y: cy,
            objects,
        };

        match save_chunk_objects_file(&file, &terrain_world.world_path) {
            Ok(()) => info!("Saved objects for chunk ({cx}, {cy})"),
            Err(e) => error!("Failed to save objects for chunk ({cx}, {cy}): {e}"),
        }
    }
}

/// Delete key while hovering a PlacedObject in edit mode.
pub fn delete_hovered_object(
    mut commands: Commands,
    settings: Res<crate::settings::UserSettings>,
    input: Res<ButtonInput<KeyCode>>,
    mut registry: ResMut<PlacedObjectRegistry>,
    hovered: Query<(Entity, &PlacedObject), With<HoveredOver>>,
) {
    if !settings.edit_mode || !input.just_pressed(KeyCode::Delete) {
        return;
    }

    for (entity, obj) in hovered.iter() {
        registry.by_id.remove(&obj.id);
        registry.dirty_chunks.insert(obj.chunk);
        commands.entity(entity).despawn_recursive();
        info!(
            "Deleted placed object id={} from chunk {:?}",
            obj.id, obj.chunk
        );
    }
}

// ============================================================================
// Private I/O
// ============================================================================

fn chunk_objects_path(world_path: &Path, cx: u32, cy: u32) -> PathBuf {
    world_path
        .join("chunks")
        .join(format!("chunk_{}_{}.objects", cx, cy))
}

pub(super) fn save_chunk_objects_file(
    file: &ChunkObjectsFile,
    world_path: &Path,
) -> Result<(), StorageError> {
    let chunks_dir = world_path.join("chunks");
    fs::create_dir_all(&chunks_dir)?;

    let path = chunk_objects_path(world_path, file.chunk_x, file.chunk_y);

    let bytes = rkyv::to_bytes::<RkyvError>(file)
        .map_err(|e| StorageError::SerializationError(e.to_string()))?;
    let checksum = crc32fast::hash(&bytes);

    let mut f = File::create(&path)?;
    f.write_all(CHUNK_OBJECTS_MAGIC)?;
    f.write_all(&CHUNK_OBJECTS_VERSION.to_le_bytes())?;
    f.write_all(&file.chunk_x.to_le_bytes())?;
    f.write_all(&file.chunk_y.to_le_bytes())?;
    f.write_all(&(bytes.len() as u32).to_le_bytes())?;
    f.write_all(&bytes)?;
    f.write_all(&checksum.to_le_bytes())?;

    Ok(())
}

fn load_chunk_objects_file(path: &Path) -> Result<ChunkObjectsFile, StorageError> {
    let mut f = File::open(path)?;

    let mut magic = [0u8; 4];
    f.read_exact(&mut magic)?;
    if &magic != CHUNK_OBJECTS_MAGIC {
        return Err(StorageError::InvalidMagic);
    }

    let mut version_bytes = [0u8; 4];
    f.read_exact(&mut version_bytes)?;
    let version = u32::from_le_bytes(version_bytes);
    if version != CHUNK_OBJECTS_VERSION {
        return Err(StorageError::VersionMismatch {
            expected: CHUNK_OBJECTS_VERSION,
            found: version,
        });
    }

    // Read chunk coords (informational only)
    let mut coord = [0u8; 4];
    f.read_exact(&mut coord)?;
    let _cx = u32::from_le_bytes(coord);
    f.read_exact(&mut coord)?;
    let _cy = u32::from_le_bytes(coord);

    let mut len_bytes = [0u8; 4];
    f.read_exact(&mut len_bytes)?;
    let data_len = u32::from_le_bytes(len_bytes) as usize;

    let mut data = vec![0u8; data_len];
    f.read_exact(&mut data)?;

    let mut checksum_bytes = [0u8; 4];
    f.read_exact(&mut checksum_bytes)?;
    let stored = u32::from_le_bytes(checksum_bytes);
    if stored != crc32fast::hash(&data) {
        return Err(StorageError::ChecksumMismatch);
    }

    let archived = rkyv::access::<ArchivedChunkObjectsFile, RkyvError>(&data)
        .map_err(|e| StorageError::SerializationError(e.to_string()))?;
    let file: ChunkObjectsFile = rkyv::deserialize::<ChunkObjectsFile, RkyvError>(archived)
        .map_err(|e| StorageError::SerializationError(e.to_string()))?;

    Ok(file)
}
