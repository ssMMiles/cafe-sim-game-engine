//! Terrain chunk storage system using rkyv for zero-copy deserialization.
//!
//! File structure:
//! ```text
//! worlds/
//!   my_world/
//!     world.meta           # World metadata + chunk index
//!     chunks/
//!       chunk_0_0.terrain  # Per-chunk data files
//!       ...
//! ```

use bevy::prelude::*;
use rkyv::{Archive, Deserialize, Serialize, rancor::Error as RkyvError};
use std::{
    collections::{HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
};

// File format constants
const WORLD_MAGIC: &[u8; 4] = b"TWNM";
const CHUNK_MAGIC: &[u8; 4] = b"TCNK";
/// Chunk (`.terrain`) format version. Version 2: chunk_x/chunk_y changed from
/// u32 to i32 for infinite terrain support. Unchanged by the level-save work —
/// the chunk payload itself did not change.
const CHUNK_FORMAT_VERSION: u32 = 2;
/// `world.meta` format version. Version 3 added [`EnvironmentData`],
/// `required_plugins`, and `plugin_data`; version 4 added `next_entity_id`;
/// version 5 switched [`EnvironmentData`] from Bevy's physical light units to
/// lightmap units (`sun_intensity` / `ambient_intensity`) for the fully baked
/// lighting pipeline. Versions 2 and 4 are still readable and are upgraded in
/// memory (see [`legacy`]). Version 3 was never in a release and is **not**
/// readable — the few v3 test worlds are expected to be deleted.
const WORLD_FORMAT_VERSION: u32 = 5;

// ============================================================================
// Serializable Data Structures
// ============================================================================

/// World metadata stored in world.meta
#[derive(Archive, Deserialize, Serialize, Clone, Debug)]
pub struct WorldMetadata {
    /// Format version for migration support
    pub version: u32,
    /// World identifier
    pub world_id: String,
    /// Terrain generation config
    pub terrain_config: TerrainConfigData,
    /// Chunk grid dimensions
    pub chunks_x: u32,
    pub chunks_y: u32,
    /// Index of all chunks with modification status
    pub chunk_index: Vec<ChunkIndexEntry>,
    /// Last save timestamp (Unix epoch seconds)
    pub last_saved: u64,
    /// Global lighting for this level.
    pub environment: EnvironmentData,
    /// Plugin ids referenced by script bindings in this level. Informational:
    /// the project manifest still decides what loads. A level that names a
    /// plugin the active project does not load produces a warning and nothing
    /// more — the engine never edits the project manifest on a level's behalf.
    pub required_plugins: Vec<String>,
    /// Opaque per-plugin level data, written and read by the plugin itself.
    pub plugin_data: Vec<PluginLevelData>,
    /// High-water mark for placed-entity ids: the next id to allocate.
    /// Persisted so a freshly loaded world can't re-allocate an id that
    /// already exists in a not-yet-streamed chunk's `.objects` file (ids key
    /// baked lightmaps on disk and `PropValue::EntityRef` links). Written on
    /// every save from `PlacedObjectRegistry::next_id`; seeded back into the
    /// registry by `seed_placed_ids` on world load.
    pub next_entity_id: u64,
}

/// Global lighting saved with the level.
///
/// Intensities are in *lightmap units*: the value a surface's lightmap texel
/// receives, where 1.0 means "show the full base colour". Every light is
/// baked (see `baking::lightmap`), so these are bake inputs, not real-time
/// light parameters. The sun orientation and position defaults match what
/// `game::world::setup` historically hard-coded.
#[derive(Archive, Deserialize, Serialize, Clone, Debug, PartialEq)]
pub struct EnvironmentData {
    /// Sun orientation as a quaternion, xyzw. Drives the `BakedSun` entity's
    /// transform, which the lightmap baker reads for its sun direction.
    pub sun_rotation: [f32; 4],
    /// Sun position. Purely cosmetic (the debug cone marker sits here); the
    /// bake only uses the rotation.
    pub sun_position: [f32; 3],
    /// Linear RGB.
    pub sun_color: [f32; 3],
    /// Radiance an unshadowed surface facing the sun receives, lightmap units.
    pub sun_intensity: f32,
    /// Linear RGB.
    pub ambient_color: [f32; 3],
    /// Radiance a fully unoccluded surface receives from the sky, lightmap
    /// units. Scaled by baked ambient occlusion per texel.
    pub ambient_intensity: f32,
}

/// The sun placement `game::world::setup` has always used: high and to one
/// side, aimed at the origin.
pub const DEFAULT_SUN_POSITION: [f32; 3] = [20.0, 120.0, -80.0];

impl Default for EnvironmentData {
    fn default() -> Self {
        let pos = bevy::math::Vec3::from(DEFAULT_SUN_POSITION);
        let dir = (bevy::math::Vec3::ZERO - pos).normalize();
        let rot = Quat::from_rotation_arc(bevy::math::Vec3::NEG_Z, dir);
        Self {
            sun_rotation: [rot.x, rot.y, rot.z, rot.w],
            sun_position: DEFAULT_SUN_POSITION,
            sun_color: [1.0, 1.0, 1.0],
            sun_intensity: 1.0,
            ambient_color: [0.9, 0.9, 1.0],
            ambient_intensity: 0.25,
        }
    }
}

impl EnvironmentData {
    pub fn sun_transform(&self) -> Transform {
        Transform {
            translation: Vec3::from(self.sun_position),
            rotation: Quat::from_xyzw(
                self.sun_rotation[0],
                self.sun_rotation[1],
                self.sun_rotation[2],
                self.sun_rotation[3],
            ),
            ..default()
        }
    }

    pub fn sun_color(&self) -> Color {
        Color::linear_rgb(self.sun_color[0], self.sun_color[1], self.sun_color[2])
    }

    pub fn ambient_color(&self) -> Color {
        Color::linear_rgb(
            self.ambient_color[0],
            self.ambient_color[1],
            self.ambient_color[2],
        )
    }

    /// Sun colour × intensity in lightmap units — the bakers' sun term.
    pub fn sun_radiance(&self) -> Vec3 {
        Vec3::from(self.sun_color) * self.sun_intensity
    }

    /// Ambient colour × intensity in lightmap units — the bakers' sky term
    /// before per-texel ambient occlusion.
    pub fn ambient_radiance(&self) -> Vec3 {
        Vec3::from(self.ambient_color) * self.ambient_intensity
    }
}

/// Opaque level-scoped blob belonging to one plugin.
#[derive(Archive, Deserialize, Serialize, Clone, Debug)]
pub struct PluginLevelData {
    /// Plugin id (directory name under `plugins/`).
    pub plugin: String,
    pub data: Vec<u8>,
}

/// Terrain configuration (serializable version of TerrainConfig)
#[derive(Archive, Deserialize, Serialize, Clone, Debug)]
pub struct TerrainConfigData {
    pub grid_width: u32,
    pub grid_height: u32,
    pub tile_size_x: f32,
    pub tile_size_y: f32,
    pub height_scale: f32,
}

/// Entry in the chunk index
#[derive(Archive, Deserialize, Serialize, Clone, Debug)]
pub struct ChunkIndexEntry {
    pub chunk_x: i32,
    pub chunk_y: i32,
    pub state: ChunkState,
}

/// State of a chunk's modifications
#[derive(Archive, Deserialize, Serialize, Clone, Debug, PartialEq, Eq, Default)]
pub enum ChunkState {
    /// No modifications - load from base heightmap
    #[default]
    Unmodified,
    /// Has delta modifications stored
    Modified,
    /// Heavily modified - full state stored
    FullyOverwritten,
}

/// Per-chunk terrain data stored in chunk_X_Y.terrain
#[derive(Archive, Deserialize, Serialize, Clone, Debug)]
pub struct ChunkData {
    /// Chunk coordinates
    pub chunk_x: i32,
    pub chunk_y: i32,
    /// Chunk dimensions
    pub width: u32,
    pub height: u32,
    /// Height data storage
    pub height_data: HeightData,
    /// Material data
    pub material_data: MaterialData,
}

/// Height data storage modes
#[derive(Archive, Deserialize, Serialize, Clone, Debug, Default)]
pub enum HeightData {
    /// No modifications - use base heightmap
    #[default]
    UseBase,
    /// Delta modifications from base heightmap (sparse)
    Delta(Vec<HeightModification>),
    /// Full height array (for heavily modified chunks)
    Full {
        /// Quantized heights as u16 (0-65535 mapped to height range)
        heights: Vec<u16>,
        /// Min height for dequantization
        height_min: f32,
        /// Max height for dequantization
        height_max: f32,
    },
}

/// A single height modification
#[derive(Archive, Deserialize, Serialize, Clone, Copy, Debug)]
pub struct HeightModification {
    /// Packed index: (local_x << 16) | local_y
    pub index: u32,
    /// Height delta (added to base height)
    pub delta: f32,
}

impl HeightModification {
    pub fn new(local_x: u32, local_y: u32, delta: f32) -> Self {
        Self {
            index: (local_x << 16) | (local_y & 0xFFFF),
            delta,
        }
    }

    pub fn local_x(&self) -> u32 {
        self.index >> 16
    }

    pub fn local_y(&self) -> u32 {
        self.index & 0xFFFF
    }
}

/// Material data storage modes
#[derive(Archive, Deserialize, Serialize, Clone, Debug, Default)]
pub enum MaterialData {
    /// Uniform material across chunk
    #[default]
    Uniform,
    /// Per-vertex material (vertex_index, material_id, weight 0-255)
    PerVertex(Vec<VertexMaterial>),
}

/// Per-vertex material assignment
#[derive(Archive, Deserialize, Serialize, Clone, Copy, Debug)]
pub struct VertexMaterial {
    pub vertex_index: u32,
    pub material_id: u8,
    pub weight: u8,
}

// ============================================================================
// Legacy (v2) world metadata
// ============================================================================

/// Older `world.meta` shapes, retained so existing worlds keep loading.
/// Parsed by [`parse_world_meta_from_bytes`] and upgraded in memory; never
/// written.
pub mod legacy {
    use super::{
        ChunkIndexEntry, EnvironmentData, PluginLevelData, TerrainConfigData, WorldMetadata,
    };
    use rkyv::{Archive, Deserialize, Serialize};

    #[derive(Archive, Deserialize, Serialize, Clone, Debug)]
    pub struct WorldMetadataV2 {
        pub version: u32,
        pub world_id: String,
        pub terrain_config: TerrainConfigData,
        pub chunks_x: u32,
        pub chunks_y: u32,
        pub chunk_index: Vec<ChunkIndexEntry>,
        pub last_saved: u64,
    }

    /// v2 worlds had no stored environment, so they adopt the defaults —
    /// which are exactly the values the engine used to hard-code.
    pub fn upgrade(old: WorldMetadataV2) -> WorldMetadata {
        WorldMetadata {
            version: super::WORLD_FORMAT_VERSION,
            world_id: old.world_id,
            terrain_config: old.terrain_config,
            chunks_x: old.chunks_x,
            chunks_y: old.chunks_y,
            chunk_index: old.chunk_index,
            last_saved: old.last_saved,
            environment: EnvironmentData::default(),
            required_plugins: Vec::new(),
            plugin_data: Vec::new(),
            // v2 object files do carry ids but no counter, so 0 could
            // collide with them. Like v3, pre-v4 worlds are dev-era test
            // data expected to be deleted rather than migrated.
            next_entity_id: 0,
        }
    }

    /// The v4 `EnvironmentData` shape: Bevy-physical light units (lux, ambient
    /// brightness) that drove the since-removed real-time directional light.
    #[derive(Archive, Deserialize, Serialize, Clone, Debug)]
    pub struct EnvironmentDataV4 {
        pub sun_rotation: [f32; 4],
        pub sun_position: [f32; 3],
        pub sun_color: [f32; 3],
        pub sun_illuminance: f32,
        pub ambient_color: [f32; 3],
        pub ambient_brightness: f32,
    }

    #[derive(Archive, Deserialize, Serialize, Clone, Debug)]
    pub struct WorldMetadataV4 {
        pub version: u32,
        pub world_id: String,
        pub terrain_config: TerrainConfigData,
        pub chunks_x: u32,
        pub chunks_y: u32,
        pub chunk_index: Vec<ChunkIndexEntry>,
        pub last_saved: u64,
        pub environment: EnvironmentDataV4,
        pub required_plugins: Vec<String>,
        pub plugin_data: Vec<PluginLevelData>,
        pub next_entity_id: u64,
    }

    /// v4 → v5: keep the sun orientation and both colours, drop the physical
    /// intensities. Lux and Bevy ambient brightness have no meaningful mapping
    /// into lightmap units, so upgraded worlds take the default intensities.
    /// Nothing baked ever depended on the old values, so the baked look of a
    /// v4 world is unchanged after a re-bake.
    pub fn upgrade_v4(old: WorldMetadataV4) -> WorldMetadata {
        let defaults = EnvironmentData::default();
        WorldMetadata {
            version: super::WORLD_FORMAT_VERSION,
            world_id: old.world_id,
            terrain_config: old.terrain_config,
            chunks_x: old.chunks_x,
            chunks_y: old.chunks_y,
            chunk_index: old.chunk_index,
            last_saved: old.last_saved,
            environment: EnvironmentData {
                sun_rotation: old.environment.sun_rotation,
                sun_position: old.environment.sun_position,
                sun_color: old.environment.sun_color,
                sun_intensity: defaults.sun_intensity,
                ambient_color: old.environment.ambient_color,
                ambient_intensity: defaults.ambient_intensity,
            },
            required_plugins: old.required_plugins,
            plugin_data: old.plugin_data,
            next_entity_id: old.next_entity_id,
        }
    }
}

// ============================================================================
// Bevy Runtime Resources and Components
// ============================================================================

/// Resource holding the loaded terrain world
#[derive(Resource)]
pub struct TerrainWorld {
    pub metadata: WorldMetadata,
    pub world_path: PathBuf,
    /// Runtime O(1) lookup: chunk state by coordinates. Built from `metadata.chunk_index` on load
    /// and kept in sync on writes. The serialized `chunk_index` Vec is the source of truth on disk.
    pub chunk_state_map: HashMap<(i32, i32), ChunkState>,
}

/// Build a `chunk_state_map` from a `WorldMetadata`. Call this once when constructing
/// `TerrainWorld` from loaded (or freshly-created) metadata.
pub fn build_chunk_state_map(metadata: &WorldMetadata) -> HashMap<(i32, i32), ChunkState> {
    metadata
        .chunk_index
        .iter()
        .map(|e| ((e.chunk_x, e.chunk_y), e.state.clone()))
        .collect()
}

/// Registry tracking loaded chunks
#[derive(Resource, Default)]
pub struct TerrainChunkRegistry {
    /// Map from (chunk_x, chunk_y) to entity
    pub loaded_chunks: HashMap<(i32, i32), Entity>,
    /// Chunks with unsaved modifications
    pub dirty_chunks: HashSet<(i32, i32)>,
}

/// Configuration for the terrain world.
/// Set this resource before the terrain loads to control world size and save directory.
#[derive(Resource, Debug, Clone)]
pub struct WorldConfig {
    /// Directory name inside "worlds/" for this world's save data
    pub world_id: String,
    /// Terrain grid width in tiles
    pub grid_width: u32,
    /// Terrain grid height in tiles
    pub grid_height: u32,
}

impl Default for WorldConfig {
    fn default() -> Self {
        Self {
            world_id: "default".to_string(),
            grid_width: 4096,
            grid_height: 4096,
        }
    }
}

// ============================================================================
// File I/O Functions
// ============================================================================

/// Error type for storage operations
#[derive(Debug)]
pub enum StorageError {
    Io(std::io::Error),
    InvalidMagic,
    VersionMismatch { expected: u32, found: u32 },
    ChecksumMismatch,
    SerializationError(String),
}

impl From<std::io::Error> for StorageError {
    fn from(e: std::io::Error) -> Self {
        StorageError::Io(e)
    }
}

impl std::fmt::Display for StorageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StorageError::Io(e) => write!(f, "I/O error: {}", e),
            StorageError::InvalidMagic => write!(f, "Invalid file magic bytes"),
            StorageError::VersionMismatch { expected, found } => {
                write!(
                    f,
                    "Version mismatch: expected {}, found {}",
                    expected, found
                )
            }
            StorageError::ChecksumMismatch => write!(f, "Checksum mismatch"),
            StorageError::SerializationError(s) => write!(f, "Serialization error: {}", s),
        }
    }
}

impl std::error::Error for StorageError {}

/// Serialize world metadata into the canonical binary frame (magic + version + len + rkyv + crc32).
/// Use this to produce bytes for writing to any backing store (fs, OPFS, etc.).
pub fn serialize_world(world: &WorldMetadata) -> Result<Vec<u8>, StorageError> {
    let rkyv_bytes = rkyv::to_bytes::<RkyvError>(world)
        .map_err(|e| StorageError::SerializationError(e.to_string()))?;
    let checksum = crc32fast::hash(&rkyv_bytes);

    let mut out = Vec::with_capacity(4 + 4 + 4 + rkyv_bytes.len() + 4);
    out.extend_from_slice(WORLD_MAGIC);
    out.extend_from_slice(&WORLD_FORMAT_VERSION.to_le_bytes());
    out.extend_from_slice(&(rkyv_bytes.len() as u32).to_le_bytes());
    out.extend_from_slice(&rkyv_bytes);
    out.extend_from_slice(&checksum.to_le_bytes());
    Ok(out)
}

/// Serialize chunk data into the canonical binary frame (magic + version + coords + len + rkyv + crc32).
/// Use this to produce bytes for writing to any backing store (fs, OPFS, etc.).
pub fn serialize_chunk(chunk: &ChunkData) -> Result<Vec<u8>, StorageError> {
    let rkyv_bytes = rkyv::to_bytes::<RkyvError>(chunk)
        .map_err(|e| StorageError::SerializationError(e.to_string()))?;
    let checksum = crc32fast::hash(&rkyv_bytes);

    let mut out = Vec::with_capacity(4 + 4 + 4 + 4 + 4 + rkyv_bytes.len() + 4);
    out.extend_from_slice(CHUNK_MAGIC);
    out.extend_from_slice(&CHUNK_FORMAT_VERSION.to_le_bytes());
    out.extend_from_slice(&chunk.chunk_x.to_le_bytes());
    out.extend_from_slice(&chunk.chunk_y.to_le_bytes());
    out.extend_from_slice(&(rkyv_bytes.len() as u32).to_le_bytes());
    out.extend_from_slice(&rkyv_bytes);
    out.extend_from_slice(&checksum.to_le_bytes());
    Ok(out)
}

/// Save world metadata to disk
pub fn save_world(world: &WorldMetadata, world_path: &Path) -> Result<(), StorageError> {
    fs::create_dir_all(world_path)?;
    let meta_path = world_path.join("world.meta");
    let out = serialize_world(world)?;
    fs::write(&meta_path, &out)?;
    Ok(())
}

/// Save chunk data to disk
pub fn save_chunk(chunk: &ChunkData, world_path: &Path) -> Result<(), StorageError> {
    let chunks_dir = world_path.join("chunks");
    fs::create_dir_all(&chunks_dir)?;
    let chunk_path = chunks_dir.join(format!("chunk_{}_{}.terrain", chunk.chunk_x, chunk.chunk_y));
    let out = serialize_chunk(chunk)?;
    fs::write(&chunk_path, &out)?;
    Ok(())
}

/// Parse chunk data from a raw byte slice (same binary format as `load_chunk`).
/// Used by the async `ChunkDataLoader` which reads bytes via Bevy's `AssetServer`.
pub fn parse_chunk_from_bytes(bytes: &[u8]) -> Result<ChunkData, StorageError> {
    use std::io::Read as StdRead;
    let mut cursor = std::io::Cursor::new(bytes);

    let mut magic = [0u8; 4];
    cursor.read_exact(&mut magic)?;
    if &magic != CHUNK_MAGIC {
        return Err(StorageError::InvalidMagic);
    }

    let mut version_bytes = [0u8; 4];
    cursor.read_exact(&mut version_bytes)?;
    let version = u32::from_le_bytes(version_bytes);
    if version != CHUNK_FORMAT_VERSION {
        return Err(StorageError::VersionMismatch {
            expected: CHUNK_FORMAT_VERSION,
            found: version,
        });
    }

    let mut coord_bytes = [0u8; 4];
    cursor.read_exact(&mut coord_bytes)?; // chunk_x
    cursor.read_exact(&mut coord_bytes)?; // chunk_y

    let mut len_bytes = [0u8; 4];
    cursor.read_exact(&mut len_bytes)?;
    let data_len = u32::from_le_bytes(len_bytes) as usize;

    let mut data = vec![0u8; data_len];
    cursor.read_exact(&mut data)?;

    let mut checksum_bytes = [0u8; 4];
    cursor.read_exact(&mut checksum_bytes)?;
    let stored_checksum = u32::from_le_bytes(checksum_bytes);
    if stored_checksum != crc32fast::hash(&data) {
        return Err(StorageError::ChecksumMismatch);
    }

    let archived = rkyv::access::<ArchivedChunkData, RkyvError>(&data)
        .map_err(|e| StorageError::SerializationError(e.to_string()))?;
    let chunk: ChunkData = rkyv::deserialize::<ChunkData, RkyvError>(archived)
        .map_err(|e| StorageError::SerializationError(e.to_string()))?;

    Ok(chunk)
}

/// Parse world metadata from a raw byte slice (same binary format as `load_world`).
/// Accepts v2 (upgraded in memory) and v4; v3 was never released and is rejected.
/// Used by the async `WorldMetaLoader` which reads bytes via Bevy's `AssetServer`.
pub fn parse_world_meta_from_bytes(bytes: &[u8]) -> Result<WorldMetadata, StorageError> {
    use std::io::Read as StdRead;
    let mut cursor = std::io::Cursor::new(bytes);

    let mut magic = [0u8; 4];
    cursor.read_exact(&mut magic)?;
    if &magic != WORLD_MAGIC {
        return Err(StorageError::InvalidMagic);
    }

    let mut version_bytes = [0u8; 4];
    cursor.read_exact(&mut version_bytes)?;
    let version = u32::from_le_bytes(version_bytes);
    if version != 2 && version != 4 && version != WORLD_FORMAT_VERSION {
        return Err(StorageError::VersionMismatch {
            expected: WORLD_FORMAT_VERSION,
            found: version,
        });
    }

    let mut len_bytes = [0u8; 4];
    cursor.read_exact(&mut len_bytes)?;
    let data_len = u32::from_le_bytes(len_bytes) as usize;

    let mut data = vec![0u8; data_len];
    cursor.read_exact(&mut data)?;

    let mut checksum_bytes = [0u8; 4];
    cursor.read_exact(&mut checksum_bytes)?;
    let stored_checksum = u32::from_le_bytes(checksum_bytes);
    if stored_checksum != crc32fast::hash(&data) {
        return Err(StorageError::ChecksumMismatch);
    }

    if version == 2 {
        let archived = rkyv::access::<legacy::ArchivedWorldMetadataV2, RkyvError>(&data)
            .map_err(|e| StorageError::SerializationError(e.to_string()))?;
        let old: legacy::WorldMetadataV2 =
            rkyv::deserialize::<legacy::WorldMetadataV2, RkyvError>(archived)
                .map_err(|e| StorageError::SerializationError(e.to_string()))?;
        return Ok(legacy::upgrade(old));
    }

    if version == 4 {
        let archived = rkyv::access::<legacy::ArchivedWorldMetadataV4, RkyvError>(&data)
            .map_err(|e| StorageError::SerializationError(e.to_string()))?;
        let old: legacy::WorldMetadataV4 =
            rkyv::deserialize::<legacy::WorldMetadataV4, RkyvError>(archived)
                .map_err(|e| StorageError::SerializationError(e.to_string()))?;
        return Ok(legacy::upgrade_v4(old));
    }

    let archived = rkyv::access::<ArchivedWorldMetadata, RkyvError>(&data)
        .map_err(|e| StorageError::SerializationError(e.to_string()))?;
    let metadata: WorldMetadata = rkyv::deserialize::<WorldMetadata, RkyvError>(archived)
        .map_err(|e| StorageError::SerializationError(e.to_string()))?;

    Ok(metadata)
}

// ============================================================================
// Helper Functions
// ============================================================================

/// Create a new world metadata with default chunk index
pub fn create_world_metadata(
    world_id: String,
    grid_width: u32,
    grid_height: u32,
    chunk_size: u32,
    tile_size: (f32, f32),
    height_scale: f32,
) -> WorldMetadata {
    let chunks_x = (grid_width + chunk_size - 2) / (chunk_size - 1);
    let chunks_y = (grid_height + chunk_size - 2) / (chunk_size - 1);

    let mut chunk_index = Vec::with_capacity((chunks_x * chunks_y) as usize);
    for cy in 0..chunks_y {
        for cx in 0..chunks_x {
            chunk_index.push(ChunkIndexEntry {
                chunk_x: cx as i32,
                chunk_y: cy as i32,
                state: ChunkState::Unmodified,
            });
        }
    }

    WorldMetadata {
        version: WORLD_FORMAT_VERSION,
        world_id,
        terrain_config: TerrainConfigData {
            grid_width,
            grid_height,
            tile_size_x: tile_size.0,
            tile_size_y: tile_size.1,
            height_scale,
        },
        chunks_x,
        chunks_y,
        chunk_index,
        last_saved: 0,
        environment: EnvironmentData::default(),
        required_plugins: Vec::new(),
        plugin_data: Vec::new(),
        next_entity_id: 0,
    }
}

/// Quantize height values to u16 for compact storage
pub fn quantize_heights(heights: &[f32]) -> (Vec<u16>, f32, f32) {
    let min = heights.iter().cloned().fold(f32::INFINITY, f32::min);
    let max = heights.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
    let range = max - min;

    let quantized = if range < f32::EPSILON {
        vec![32768u16; heights.len()]
    } else {
        heights
            .iter()
            .map(|&h| ((h - min) / range * 65535.0) as u16)
            .collect()
    };

    (quantized, min, max)
}

/// Dequantize u16 heights back to f32
pub fn dequantize_heights(quantized: &[u16], min: f32, max: f32) -> Vec<f32> {
    let range = max - min;
    quantized
        .iter()
        .map(|&q| min + (q as f32 / 65535.0) * range)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_world_save_load_roundtrip() {
        let dir = tempdir().unwrap();
        let world_path = dir.path().join("test_world");

        let metadata = create_world_metadata("test".to_string(), 256, 256, 64, (1.0, 1.0), 10.0);

        save_world(&metadata, &world_path).unwrap();
        let bytes = std::fs::read(world_path.join("world.meta")).unwrap();
        let loaded = parse_world_meta_from_bytes(&bytes).unwrap();

        assert_eq!(loaded.world_id, metadata.world_id);
        assert_eq!(loaded.chunks_x, metadata.chunks_x);
        assert_eq!(loaded.chunks_y, metadata.chunks_y);
    }

    #[test]
    fn test_world_v5_environment_plugin_data_and_id_counter_roundtrip() {
        let mut metadata = create_world_metadata("env".to_string(), 128, 128, 64, (1.0, 1.0), 10.0);
        metadata.next_entity_id = 77;
        metadata.environment = EnvironmentData {
            sun_rotation: [0.1, 0.2, 0.3, 0.9],
            sun_position: [1.0, 50.0, -2.0],
            sun_color: [0.9, 0.8, 0.7],
            sun_intensity: 1.5,
            ambient_color: [0.1, 0.2, 0.3],
            ambient_intensity: 0.4,
        };
        metadata.required_plugins = vec!["cafe-sim".to_string()];
        metadata.plugin_data = vec![PluginLevelData {
            plugin: "cafe-sim".to_string(),
            data: vec![1, 2, 3, 4],
        }];

        let bytes = serialize_world(&metadata).unwrap();
        let loaded = parse_world_meta_from_bytes(&bytes).unwrap();

        assert_eq!(loaded.version, WORLD_FORMAT_VERSION);
        assert_eq!(loaded.environment.sun_intensity, 1.5);
        assert_eq!(loaded.environment.ambient_intensity, 0.4);
        assert_eq!(loaded.environment.sun_position, [1.0, 50.0, -2.0]);
        assert_eq!(
            loaded.environment.sun_radiance(),
            Vec3::new(0.9, 0.8, 0.7) * 1.5
        );
        assert_eq!(loaded.required_plugins, vec!["cafe-sim".to_string()]);
        assert_eq!(loaded.plugin_data.len(), 1);
        assert_eq!(loaded.plugin_data[0].plugin, "cafe-sim");
        assert_eq!(loaded.plugin_data[0].data, vec![1, 2, 3, 4]);
        assert_eq!(loaded.next_entity_id, 77);
    }

    /// A v2 `world.meta` must still load, keeping its chunk index and picking
    /// up the default environment (and a zero id counter — pre-v4 worlds are
    /// dev-era data expected to be recreated, not migrated).
    #[test]
    fn test_world_v2_upgrades_to_current() {
        let old = legacy::WorldMetadataV2 {
            version: 2,
            world_id: "legacy".to_string(),
            terrain_config: TerrainConfigData {
                grid_width: 256,
                grid_height: 256,
                tile_size_x: 1.0,
                tile_size_y: 1.0,
                height_scale: 10.0,
            },
            chunks_x: 4,
            chunks_y: 4,
            chunk_index: vec![ChunkIndexEntry {
                chunk_x: -1,
                chunk_y: 2,
                state: ChunkState::FullyOverwritten,
            }],
            last_saved: 99,
        };

        let rkyv_bytes = rkyv::to_bytes::<RkyvError>(&old).unwrap();
        let checksum = crc32fast::hash(&rkyv_bytes);
        let mut bytes = Vec::new();
        bytes.extend_from_slice(WORLD_MAGIC);
        bytes.extend_from_slice(&2u32.to_le_bytes());
        bytes.extend_from_slice(&(rkyv_bytes.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&rkyv_bytes);
        bytes.extend_from_slice(&checksum.to_le_bytes());

        let loaded = parse_world_meta_from_bytes(&bytes).unwrap();

        assert_eq!(loaded.version, WORLD_FORMAT_VERSION);
        assert_eq!(loaded.world_id, "legacy");
        assert_eq!(loaded.chunks_x, 4);
        assert_eq!(loaded.last_saved, 99);
        assert_eq!(loaded.chunk_index.len(), 1);
        assert_eq!(loaded.chunk_index[0].chunk_x, -1);
        assert_eq!(loaded.chunk_index[0].state, ChunkState::FullyOverwritten);
        // Upgraded worlds adopt the engine's historical hard-coded lighting.
        let d = EnvironmentData::default();
        assert_eq!(loaded.environment.ambient_intensity, d.ambient_intensity);
        assert_eq!(loaded.environment.sun_position, d.sun_position);
        assert!(loaded.required_plugins.is_empty());
        assert!(loaded.plugin_data.is_empty());
        assert_eq!(loaded.next_entity_id, 0);
    }

    /// A v4 `world.meta` keeps everything except the physical light
    /// intensities, which become the lightmap-unit defaults.
    #[test]
    fn test_world_v4_upgrades_to_current() {
        let old = legacy::WorldMetadataV4 {
            version: 4,
            world_id: "v4".to_string(),
            terrain_config: TerrainConfigData {
                grid_width: 128,
                grid_height: 128,
                tile_size_x: 1.0,
                tile_size_y: 1.0,
                height_scale: 10.0,
            },
            chunks_x: 2,
            chunks_y: 2,
            chunk_index: vec![ChunkIndexEntry {
                chunk_x: 1,
                chunk_y: 0,
                state: ChunkState::Modified,
            }],
            last_saved: 5,
            environment: legacy::EnvironmentDataV4 {
                sun_rotation: [0.1, 0.2, 0.3, 0.9],
                sun_position: [1.0, 50.0, -2.0],
                sun_color: [0.9, 0.8, 0.7],
                sun_illuminance: 12_345.0,
                ambient_color: [0.1, 0.2, 0.3],
                ambient_brightness: 42.0,
            },
            required_plugins: vec!["cafe-sim".to_string()],
            plugin_data: vec![PluginLevelData {
                plugin: "cafe-sim".to_string(),
                data: vec![9],
            }],
            next_entity_id: 31,
        };

        let rkyv_bytes = rkyv::to_bytes::<RkyvError>(&old).unwrap();
        let checksum = crc32fast::hash(&rkyv_bytes);
        let mut bytes = Vec::new();
        bytes.extend_from_slice(WORLD_MAGIC);
        bytes.extend_from_slice(&4u32.to_le_bytes());
        bytes.extend_from_slice(&(rkyv_bytes.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&rkyv_bytes);
        bytes.extend_from_slice(&checksum.to_le_bytes());

        let loaded = parse_world_meta_from_bytes(&bytes).unwrap();
        let d = EnvironmentData::default();

        assert_eq!(loaded.version, WORLD_FORMAT_VERSION);
        assert_eq!(loaded.world_id, "v4");
        assert_eq!(loaded.chunk_index[0].state, ChunkState::Modified);
        assert_eq!(loaded.environment.sun_rotation, [0.1, 0.2, 0.3, 0.9]);
        assert_eq!(loaded.environment.sun_position, [1.0, 50.0, -2.0]);
        assert_eq!(loaded.environment.sun_color, [0.9, 0.8, 0.7]);
        assert_eq!(loaded.environment.ambient_color, [0.1, 0.2, 0.3]);
        assert_eq!(loaded.environment.sun_intensity, d.sun_intensity);
        assert_eq!(loaded.environment.ambient_intensity, d.ambient_intensity);
        assert_eq!(loaded.required_plugins, vec!["cafe-sim".to_string()]);
        assert_eq!(loaded.plugin_data[0].data, vec![9]);
        assert_eq!(loaded.next_entity_id, 31);
    }

    /// The chunk payload did not change in v3; `.terrain` files stay at
    /// version 2 so existing chunk files keep loading.
    #[test]
    fn test_chunk_format_version_unchanged() {
        let chunk = ChunkData {
            chunk_x: 0,
            chunk_y: 0,
            width: 4,
            height: 4,
            height_data: HeightData::UseBase,
            material_data: MaterialData::Uniform,
        };
        let bytes = serialize_chunk(&chunk).unwrap();
        assert_eq!(u32::from_le_bytes(bytes[4..8].try_into().unwrap()), 2);
        assert!(parse_chunk_from_bytes(&bytes).is_ok());
    }

    #[test]
    fn test_chunk_save_load_roundtrip() {
        let dir = tempdir().unwrap();
        let world_path = dir.path().join("test_world");

        let chunk = ChunkData {
            chunk_x: 2,
            chunk_y: 3,
            width: 64,
            height: 64,
            height_data: HeightData::Delta(vec![
                HeightModification::new(10, 20, 5.0),
                HeightModification::new(30, 40, -2.5),
            ]),
            material_data: MaterialData::Uniform,
        };

        save_chunk(&chunk, &world_path).unwrap();
        let bytes = std::fs::read(world_path.join("chunks/chunk_2_3.terrain")).unwrap();
        let loaded = parse_chunk_from_bytes(&bytes).unwrap();

        assert_eq!(loaded.chunk_x, 2);
        assert_eq!(loaded.chunk_y, 3);
        if let HeightData::Delta(mods) = &loaded.height_data {
            assert_eq!(mods.len(), 2);
            assert_eq!(mods[0].local_x(), 10);
            assert_eq!(mods[0].local_y(), 20);
        } else {
            panic!("Expected Delta height data");
        }
    }

    #[test]
    fn test_height_quantization() {
        let heights = vec![0.0, 5.0, 10.0, 2.5, 7.5];
        let (quantized, min, max) = quantize_heights(&heights);
        let restored = dequantize_heights(&quantized, min, max);

        for (orig, rest) in heights.iter().zip(restored.iter()) {
            assert!((orig - rest).abs() < 0.001);
        }
    }
}
