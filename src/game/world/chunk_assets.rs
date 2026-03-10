//! Bevy `Asset` wrappers for terrain chunk and world metadata binary files.
//!
//! `ChunkDataAsset`    ← `.terrain` files (terrain heightmap + modifications)
//! `ChunkObjectsAsset` ← `.objects` files (placed object records)
//! `WorldMetaAsset`    ← `world.meta` files (world metadata + chunk index)
//!
//! All loaders read all bytes from the Bevy `Reader` and then parse them using
//! the same binary format as the synchronous functions in `storage.rs` / `objects.rs`.

use bevy::{
    asset::{AssetLoader, LoadContext, io::Reader},
    prelude::*,
};

use super::{
    objects::{ChunkObjectsFile, parse_chunk_objects_from_bytes},
    terrain::storage::{
        ChunkData, WorldMetadata, parse_chunk_from_bytes, parse_world_meta_from_bytes,
    },
};

// ============================================================================
// Asset types
// ============================================================================

/// Bevy `Asset` wrapping a deserialized `ChunkData` (`.terrain` file).
#[derive(Asset, TypePath)]
pub struct ChunkDataAsset(pub ChunkData);

/// Bevy `Asset` wrapping a deserialized `ChunkObjectsFile` (`.objects` file).
#[derive(Asset, TypePath)]
pub struct ChunkObjectsAsset(pub ChunkObjectsFile);

/// Bevy `Asset` wrapping a deserialized `WorldMetadata` (`world.meta` file).
#[derive(Asset, TypePath)]
pub struct WorldMetaAsset(pub WorldMetadata);

// ============================================================================
// Error type
// ============================================================================

/// Errors produced by chunk asset loaders.
#[derive(Debug)]
pub struct ChunkLoadError(pub String);

impl std::fmt::Display for ChunkLoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Chunk load error: {}", self.0)
    }
}

impl std::error::Error for ChunkLoadError {}

// ============================================================================
// Asset loaders
// ============================================================================

/// AssetLoader for `.terrain` files → `ChunkDataAsset`.
#[derive(Default, TypePath)]
pub struct ChunkDataLoader;

impl AssetLoader for ChunkDataLoader {
    type Asset = ChunkDataAsset;
    type Settings = ();
    type Error = ChunkLoadError;

    async fn load(
        &self,
        reader: &mut dyn Reader,
        _settings: &Self::Settings,
        _load_context: &mut LoadContext<'_>,
    ) -> Result<Self::Asset, Self::Error> {
        let mut bytes = Vec::new();
        reader
            .read_to_end(&mut bytes)
            .await
            .map_err(|e| ChunkLoadError(e.to_string()))?;
        parse_chunk_from_bytes(&bytes)
            .map(ChunkDataAsset)
            .map_err(|e| ChunkLoadError(e.to_string()))
    }

    fn extensions(&self) -> &[&str] {
        &["terrain"]
    }
}

/// AssetLoader for `.objects` files → `ChunkObjectsAsset`.
#[derive(Default, TypePath)]
pub struct ChunkObjectsLoader;

impl AssetLoader for ChunkObjectsLoader {
    type Asset = ChunkObjectsAsset;
    type Settings = ();
    type Error = ChunkLoadError;

    async fn load(
        &self,
        reader: &mut dyn Reader,
        _settings: &Self::Settings,
        _load_context: &mut LoadContext<'_>,
    ) -> Result<Self::Asset, Self::Error> {
        let mut bytes = Vec::new();
        reader
            .read_to_end(&mut bytes)
            .await
            .map_err(|e| ChunkLoadError(e.to_string()))?;
        parse_chunk_objects_from_bytes(&bytes)
            .map(ChunkObjectsAsset)
            .map_err(|e| ChunkLoadError(e.to_string()))
    }

    fn extensions(&self) -> &[&str] {
        &["objects"]
    }
}

/// AssetLoader for `world.meta` files → `WorldMetaAsset`.
///
/// `extensions()` returns empty because we always load via
/// `asset_server.load::<WorldMetaAsset>(path)` with an explicit type, which
/// avoids collisions with Bevy's own `.meta` sidecar files.
#[derive(Default, TypePath)]
pub struct WorldMetaLoader;

impl AssetLoader for WorldMetaLoader {
    type Asset = WorldMetaAsset;
    type Settings = ();
    type Error = ChunkLoadError;

    async fn load(
        &self,
        reader: &mut dyn Reader,
        _settings: &Self::Settings,
        _load_context: &mut LoadContext<'_>,
    ) -> Result<Self::Asset, Self::Error> {
        let mut bytes = Vec::new();
        reader
            .read_to_end(&mut bytes)
            .await
            .map_err(|e| ChunkLoadError(e.to_string()))?;
        parse_world_meta_from_bytes(&bytes)
            .map(WorldMetaAsset)
            .map_err(|e| ChunkLoadError(e.to_string()))
    }

    fn extensions(&self) -> &[&str] {
        &[] // explicit type dispatch only
    }
}
