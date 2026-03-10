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
    fs::{self, File},
    io::{Read, Write as IoWrite},
    path::{Path, PathBuf},
};

// File format constants
const WORLD_MAGIC: &[u8; 4] = b"TWNM";
const CHUNK_MAGIC: &[u8; 4] = b"TCNK";
// Version 2: chunk_x/chunk_y changed from u32 to i32 for infinite terrain support.
const FORMAT_VERSION: u32 = 2;

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
// Bevy Runtime Resources and Components
// ============================================================================

/// Resource holding the loaded terrain world
#[derive(Resource)]
pub struct TerrainWorld {
    pub metadata: WorldMetadata,
    pub world_path: PathBuf,
}

/// Registry tracking loaded chunks
#[derive(Resource, Default)]
pub struct TerrainChunkRegistry {
    /// Map from (chunk_x, chunk_y) to entity
    pub loaded_chunks: HashMap<(i32, i32), Entity>,
    /// Chunks with unsaved modifications
    pub dirty_chunks: HashSet<(i32, i32)>,
}

/// Component for chunks with runtime modifications (used by terrain editing tools)
#[derive(Component, Default)]
pub struct ModifiedTerrainChunk {
    /// Pending height modifications
    pub height_mods: Vec<HeightModification>,
    /// Whether chunk needs GPU recompute
    pub needs_gpu_update: bool,
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

/// Save world metadata to disk
pub fn save_world(world: &WorldMetadata, world_path: &Path) -> Result<(), StorageError> {
    fs::create_dir_all(world_path)?;
    let meta_path = world_path.join("world.meta");

    let bytes = rkyv::to_bytes::<RkyvError>(world)
        .map_err(|e| StorageError::SerializationError(e.to_string()))?;
    let checksum = crc32fast::hash(&bytes);

    let mut file = File::create(&meta_path)?;
    file.write_all(WORLD_MAGIC)?;
    file.write_all(&FORMAT_VERSION.to_le_bytes())?;
    file.write_all(&(bytes.len() as u32).to_le_bytes())?;
    file.write_all(&bytes)?;
    file.write_all(&checksum.to_le_bytes())?;

    Ok(())
}

/// Load world metadata from disk
pub fn load_world(world_path: &Path) -> Result<WorldMetadata, StorageError> {
    let meta_path = world_path.join("world.meta");
    let mut file = File::open(&meta_path)?;

    // Read and validate header
    let mut magic = [0u8; 4];
    file.read_exact(&mut magic)?;
    if &magic != WORLD_MAGIC {
        return Err(StorageError::InvalidMagic);
    }

    let mut version_bytes = [0u8; 4];
    file.read_exact(&mut version_bytes)?;
    let version = u32::from_le_bytes(version_bytes);
    if version != FORMAT_VERSION {
        return Err(StorageError::VersionMismatch {
            expected: FORMAT_VERSION,
            found: version,
        });
    }

    let mut len_bytes = [0u8; 4];
    file.read_exact(&mut len_bytes)?;
    let data_len = u32::from_le_bytes(len_bytes) as usize;

    // Read data
    let mut data = vec![0u8; data_len];
    file.read_exact(&mut data)?;

    // Read and verify checksum
    let mut checksum_bytes = [0u8; 4];
    file.read_exact(&mut checksum_bytes)?;
    let stored_checksum = u32::from_le_bytes(checksum_bytes);
    let computed_checksum = crc32fast::hash(&data);

    if stored_checksum != computed_checksum {
        return Err(StorageError::ChecksumMismatch);
    }

    // Deserialize
    let archived = rkyv::access::<ArchivedWorldMetadata, RkyvError>(&data)
        .map_err(|e| StorageError::SerializationError(e.to_string()))?;
    let metadata: WorldMetadata = rkyv::deserialize::<WorldMetadata, RkyvError>(archived)
        .map_err(|e| StorageError::SerializationError(e.to_string()))?;

    Ok(metadata)
}

/// Save chunk data to disk
pub fn save_chunk(chunk: &ChunkData, world_path: &Path) -> Result<(), StorageError> {
    let chunks_dir = world_path.join("chunks");
    fs::create_dir_all(&chunks_dir)?;

    let chunk_path = chunks_dir.join(format!("chunk_{}_{}.terrain", chunk.chunk_x, chunk.chunk_y));

    let bytes = rkyv::to_bytes::<RkyvError>(chunk)
        .map_err(|e| StorageError::SerializationError(e.to_string()))?;
    let checksum = crc32fast::hash(&bytes);

    let mut file = File::create(&chunk_path)?;
    file.write_all(CHUNK_MAGIC)?;
    file.write_all(&FORMAT_VERSION.to_le_bytes())?;
    file.write_all(&chunk.chunk_x.to_le_bytes())?;
    file.write_all(&chunk.chunk_y.to_le_bytes())?;
    file.write_all(&(bytes.len() as u32).to_le_bytes())?;
    file.write_all(&bytes)?;
    file.write_all(&checksum.to_le_bytes())?;

    Ok(())
}

/// Load chunk data from disk
pub fn load_chunk(
    chunk_x: i32,
    chunk_y: i32,
    world_path: &Path,
) -> Result<ChunkData, StorageError> {
    let chunk_path = world_path
        .join("chunks")
        .join(format!("chunk_{}_{}.terrain", chunk_x, chunk_y));
    let mut file = File::open(&chunk_path)?;

    // Read and validate header
    let mut magic = [0u8; 4];
    file.read_exact(&mut magic)?;
    if &magic != CHUNK_MAGIC {
        return Err(StorageError::InvalidMagic);
    }

    let mut version_bytes = [0u8; 4];
    file.read_exact(&mut version_bytes)?;
    let version = u32::from_le_bytes(version_bytes);
    if version != FORMAT_VERSION {
        return Err(StorageError::VersionMismatch {
            expected: FORMAT_VERSION,
            found: version,
        });
    }

    // Read chunk coordinates (for verification)
    let mut coord_bytes = [0u8; 4];
    file.read_exact(&mut coord_bytes)?;
    let _stored_x = i32::from_le_bytes(coord_bytes);
    file.read_exact(&mut coord_bytes)?;
    let _stored_y = i32::from_le_bytes(coord_bytes);

    let mut len_bytes = [0u8; 4];
    file.read_exact(&mut len_bytes)?;
    let data_len = u32::from_le_bytes(len_bytes) as usize;

    // Read data
    let mut data = vec![0u8; data_len];
    file.read_exact(&mut data)?;

    // Read and verify checksum
    let mut checksum_bytes = [0u8; 4];
    file.read_exact(&mut checksum_bytes)?;
    let stored_checksum = u32::from_le_bytes(checksum_bytes);
    let computed_checksum = crc32fast::hash(&data);

    if stored_checksum != computed_checksum {
        return Err(StorageError::ChecksumMismatch);
    }

    // Deserialize
    let archived = rkyv::access::<ArchivedChunkData, RkyvError>(&data)
        .map_err(|e| StorageError::SerializationError(e.to_string()))?;
    let chunk: ChunkData = rkyv::deserialize::<ChunkData, RkyvError>(archived)
        .map_err(|e| StorageError::SerializationError(e.to_string()))?;

    Ok(chunk)
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
    if version != FORMAT_VERSION {
        return Err(StorageError::VersionMismatch {
            expected: FORMAT_VERSION,
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
    if version != FORMAT_VERSION {
        return Err(StorageError::VersionMismatch {
            expected: FORMAT_VERSION,
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

    let archived = rkyv::access::<ArchivedWorldMetadata, RkyvError>(&data)
        .map_err(|e| StorageError::SerializationError(e.to_string()))?;
    let metadata: WorldMetadata = rkyv::deserialize::<WorldMetadata, RkyvError>(archived)
        .map_err(|e| StorageError::SerializationError(e.to_string()))?;

    Ok(metadata)
}

/// Check if a chunk file exists
pub fn chunk_exists(chunk_x: i32, chunk_y: i32, world_path: &Path) -> bool {
    let chunk_path = world_path
        .join("chunks")
        .join(format!("chunk_{}_{}.terrain", chunk_x, chunk_y));
    chunk_path.exists()
}

/// Check if a world exists at the given path
pub fn world_exists(world_path: &Path) -> bool {
    world_path.join("world.meta").exists()
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
        version: FORMAT_VERSION,
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
    }
}

/// Create default chunk data for an unmodified chunk
pub fn create_default_chunk_data(chunk_x: i32, chunk_y: i32, width: u32, height: u32) -> ChunkData {
    ChunkData {
        chunk_x,
        chunk_y,
        width,
        height,
        height_data: HeightData::UseBase,
        material_data: MaterialData::Uniform,
    }
}

/// Apply height modifications to a base height array
pub fn apply_height_deltas(
    base_heights: &mut [f32],
    chunk_height: u32,
    mods: &[HeightModification],
) {
    for m in mods {
        let local_x = m.local_x();
        let local_y = m.local_y();
        let idx = (local_x * chunk_height + local_y) as usize;
        if idx < base_heights.len() {
            base_heights[idx] += m.delta;
        }
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
        let loaded = load_world(&world_path).unwrap();

        assert_eq!(loaded.world_id, metadata.world_id);
        assert_eq!(loaded.chunks_x, metadata.chunks_x);
        assert_eq!(loaded.chunks_y, metadata.chunks_y);
    }

    #[test]
    fn test_chunk_save_load_roundtrip() {
        let dir = tempdir().unwrap();
        let world_path = dir.path().join("test_world");

        let mut chunk = create_default_chunk_data(2, 3, 64, 64);
        chunk.height_data = HeightData::Delta(vec![
            HeightModification::new(10, 20, 5.0),
            HeightModification::new(30, 40, -2.5),
        ]);

        save_chunk(&chunk, &world_path).unwrap();
        let loaded = load_chunk(2i32, 3i32, &world_path).unwrap();

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
