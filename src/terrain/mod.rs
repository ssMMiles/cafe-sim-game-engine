pub(crate) mod bootstrap;
pub(crate) mod constants;
pub(crate) mod edit;
pub(crate) mod lod;
mod material;
mod mesh;
pub(crate) mod plugin;
pub(crate) mod reconcile;
mod save;
pub mod storage;
mod types;

pub use plugin::HeightmapPlugin;
pub use types::{Heightmap, HeightmapData, TerrainChunk};

pub(crate) use constants::{CHUNK_SIZE, HEIGHT_SCALE, TILE_SIZE};
