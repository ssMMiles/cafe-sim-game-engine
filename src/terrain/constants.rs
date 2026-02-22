use super::mesh::LOD_LEVELS;

/// Chunk size in vertices per side (overlap of 1 between adjacent chunks)
pub(crate) const CHUNK_SIZE: usize = 64;

/// Squared distance thresholds for LOD switching (avoids sqrt in hot path)
pub(crate) const LOD_DISTANCES_SQ: [f32; LOD_LEVELS] = [
    250_000.0,    // 500^2
    1_000_000.0,  // 1000^2
    6_250_000.0,  // 2500^2
    16_000_000.0, // 4000^2
];

pub(crate) const TILE_SIZE: (f32, f32) = (1.0, 1.0);
pub(crate) const HEIGHT_SCALE: f32 = 200.0;
