//! World object placement and persistence system.
//!
//! Objects are stored per-chunk alongside terrain data:
//! `worlds/{id}/chunks/chunk_X_Y.objects`

use avian3d::prelude::*;
use bevy::prelude::*;
use rkyv::{Archive, Deserialize, Serialize, rancor::Error as RkyvError};
use std::collections::{HashMap, HashSet};
#[cfg(not(all(target_arch = "wasm32", feature = "opfs")))]
use std::fs;

use crate::game::{
    GameEntity,
    gltf_loading::{ConvertToToon, Interactable, spawn_interactable_scene},
    world::terrain::storage::StorageError,
};

use crate::game::world::terrain::storage::TerrainWorld;
use crate::game::world::terrain::{CHUNK_SIZE, TILE_SIZE};

// File format constants
const CHUNK_OBJECTS_MAGIC: &[u8; 4] = b"COBJ";
// Version 2: chunk_x/chunk_y changed from u32 to i32 for infinite terrain support.
const CHUNK_OBJECTS_VERSION: u32 = 2;

// Chunk world size in units: (CHUNK_SIZE - 1) * TILE_SIZE.
pub const CHUNK_WORLD_SIZE: f32 = (CHUNK_SIZE - 1) as f32 * TILE_SIZE.0;

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
    pub chunk_x: i32,
    pub chunk_y: i32,
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
    pub chunk: (i32, i32),
}

/// Global registry of all placed objects.
#[derive(Resource, Default)]
pub struct PlacedObjectRegistry {
    pub by_id: HashMap<u64, Entity>,
    pub next_id: u64,
    pub dirty_chunks: HashSet<(i32, i32)>,
}

impl PlacedObjectRegistry {
    pub fn alloc_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }
}

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

pub fn world_pos_to_chunk(world_x: f32, world_z: f32) -> (i32, i32) {
    let cx = (world_x / CHUNK_WORLD_SIZE).floor() as i32;
    let cy = (world_z / CHUNK_WORLD_SIZE).floor() as i32;
    (cx, cy)
}

// ============================================================================
// Spawn / Despawn Helpers
// ============================================================================

pub fn spawn_placed_object(
    commands: &mut Commands,
    asset_server: &AssetServer,
    registry: &mut PlacedObjectRegistry,
    asset_path: String,
    id: u64,
    chunk: (i32, i32),
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
            ConvertToToon,
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

/// Spawn all objects described in a `ChunkObjectsFile` and register them.
pub fn spawn_chunk_objects(
    commands: &mut Commands,
    asset_server: &AssetServer,
    registry: &mut PlacedObjectRegistry,
    chunk_objects: &ChunkObjectsFile,
) {
    let chunk = (chunk_objects.chunk_x, chunk_objects.chunk_y);
    let mut max_id: u64 = 0;

    for record in &chunk_objects.objects {
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

        let handle: Handle<Scene> = asset_server.load(&record.asset_path);
        let entity = spawn_interactable_scene(
            commands,
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
                ConvertToToon,
                SceneRoot(handle),
                transform,
                GlobalTransform::default(),
                RigidBody::Static,
                ColliderConstructorHierarchy::new(ColliderConstructor::TrimeshFromMesh),
            ),
        );
        registry.by_id.insert(record.id, entity);
        max_id = max_id.max(record.id);
    }

    // Advance next_id past any loaded ids so new allocations don't collide.
    if max_id >= registry.next_id {
        registry.next_id = max_id + 1;
    }
}

/// Despawn all placed objects belonging to `chunk` and remove them from the registry.
pub fn despawn_chunk_objects(
    commands: &mut Commands,
    registry: &mut PlacedObjectRegistry,
    placed_query: &Query<(Entity, &PlacedObject)>,
    chunk: (i32, i32),
) {
    let to_despawn: Vec<(u64, Entity)> = placed_query
        .iter()
        .filter(|(_, obj)| obj.chunk == chunk)
        .map(|(entity, obj)| (obj.id, entity))
        .collect();

    for (id, entity) in to_despawn {
        registry.by_id.remove(&id);
        commands.entity(entity).despawn();
    }
}

// ============================================================================
// Systems
// ============================================================================

/// Saves all dirty object chunks alongside terrain saving — runs whenever the
/// editor sends a `SaveTerrainMessage`.
pub fn save_placed_objects(
    mut messages: MessageReader<crate::game::world::terrain::save::SaveTerrainMessage>,
    terrain_world: Option<Res<TerrainWorld>>,
    mut registry: ResMut<PlacedObjectRegistry>,
    placed_query: Query<(&PlacedObject, &Transform)>,
    #[cfg(all(target_arch = "wasm32", feature = "opfs"))] _storage: Res<crate::storage::Storage>,
) {
    if messages.read().count() == 0 {
        return;
    }

    let Some(terrain_world) = terrain_world else {
        warn!("Cannot save objects: world not loaded");
        return;
    };

    if registry.dirty_chunks.is_empty() {
        return;
    }

    let dirty: Vec<(i32, i32)> = registry.dirty_chunks.drain().collect();
    info!("Saving {} dirty object chunk(s)", dirty.len());

    // Build the list of (file, serialized_bytes) pairs once, shared by both paths.
    let world_id = terrain_world.metadata.world_id.clone();
    let mut writes: Vec<(String, Vec<u8>, i32, i32)> = Vec::new();

    for (cx, cy) in &dirty {
        let (cx, cy) = (*cx, *cy);
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
            version: CHUNK_OBJECTS_VERSION,
            chunk_x: cx,
            chunk_y: cy,
            objects,
        };

        match serialize_chunk_objects(&file) {
            Ok(bytes) => {
                let key = format!("worlds/{}/chunks/chunk_{}_{}.objects", world_id, cx, cy);
                writes.push((key, bytes, cx, cy));
            }
            Err(e) => error!("Failed to serialize objects for chunk ({cx}, {cy}): {e}"),
        }
    }

    #[cfg(not(all(target_arch = "wasm32", feature = "opfs")))]
    {
        let world_path = terrain_world.world_path.clone();
        for (_, bytes, cx, cy) in writes {
            let chunks_dir = world_path.join("chunks");
            if let Err(e) = fs::create_dir_all(&chunks_dir) {
                error!("Failed to create chunks dir: {e}");
                continue;
            }
            let path = world_path
                .join("chunks")
                .join(format!("chunk_{}_{}.objects", cx, cy));
            match fs::write(&path, &bytes) {
                Ok(()) => info!("Saved objects for chunk ({cx}, {cy})"),
                Err(e) => error!("Failed to save objects for chunk ({cx}, {cy}): {e}"),
            }
        }
    }

    #[cfg(all(target_arch = "wasm32", feature = "opfs"))]
    {
        use wasm_bindgen_futures::spawn_local;
        let s = crate::storage::wasm::WasmStorage;
        spawn_local(async move {
            for (key, bytes, cx, cy) in writes {
                if let Err(e) = s.save(&key, &bytes).await {
                    log::error!("OPFS objects save failed for chunk ({cx}, {cy}): {e}");
                } else {
                    log::info!("Saved objects for chunk ({cx}, {cy}) to OPFS");
                }
            }
        });
    }
}

// ============================================================================
// I/O Helpers
// ============================================================================

/// Serialize a `ChunkObjectsFile` into the canonical binary frame
/// (magic + version + coords + len + rkyv + crc32).
/// Use this to produce bytes for writing to any backing store (fs, OPFS, etc.).
pub fn serialize_chunk_objects(file: &ChunkObjectsFile) -> Result<Vec<u8>, StorageError> {
    let rkyv_bytes = rkyv::to_bytes::<RkyvError>(file)
        .map_err(|e| StorageError::SerializationError(e.to_string()))?;
    let checksum = crc32fast::hash(&rkyv_bytes);

    let mut out = Vec::with_capacity(4 + 4 + 4 + 4 + 4 + rkyv_bytes.len() + 4);
    out.extend_from_slice(CHUNK_OBJECTS_MAGIC);
    out.extend_from_slice(&CHUNK_OBJECTS_VERSION.to_le_bytes());
    out.extend_from_slice(&file.chunk_x.to_le_bytes());
    out.extend_from_slice(&file.chunk_y.to_le_bytes());
    out.extend_from_slice(&(rkyv_bytes.len() as u32).to_le_bytes());
    out.extend_from_slice(&rkyv_bytes);
    out.extend_from_slice(&checksum.to_le_bytes());
    Ok(out)
}

/// Parse a `ChunkObjectsFile` from raw bytes (same binary format as `load_chunk_objects_file`).
/// Used by the async `ChunkObjectsLoader` which reads bytes via Bevy's `AssetServer`.
pub fn parse_chunk_objects_from_bytes(bytes: &[u8]) -> Result<ChunkObjectsFile, StorageError> {
    use std::io::Read as StdRead;
    let mut cursor = std::io::Cursor::new(bytes);

    let mut magic = [0u8; 4];
    cursor.read_exact(&mut magic)?;
    if &magic != CHUNK_OBJECTS_MAGIC {
        return Err(StorageError::InvalidMagic);
    }

    let mut version_bytes = [0u8; 4];
    cursor.read_exact(&mut version_bytes)?;
    let version = u32::from_le_bytes(version_bytes);
    if version != CHUNK_OBJECTS_VERSION {
        return Err(StorageError::VersionMismatch {
            expected: CHUNK_OBJECTS_VERSION,
            found: version,
        });
    }

    let mut coord = [0u8; 4];
    cursor.read_exact(&mut coord)?; // chunk_x (i32)
    cursor.read_exact(&mut coord)?; // chunk_y (i32)

    let mut len_bytes = [0u8; 4];
    cursor.read_exact(&mut len_bytes)?;
    let data_len = u32::from_le_bytes(len_bytes) as usize;

    let mut data = vec![0u8; data_len];
    cursor.read_exact(&mut data)?;

    let mut checksum_bytes = [0u8; 4];
    cursor.read_exact(&mut checksum_bytes)?;
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
