//! Runtime home of baked irradiance probes.
//!
//! `ProbeStore` holds every loaded chunk's probe grid (`baking::probes`),
//! fed by the chunk streamer when a chunk arrives and by the bake when it
//! finishes, and emptied as chunks unload. The volume texture that dynamic
//! objects actually sample is assembled from it — see the plan in
//! `docs/DYNAMIC_LIGHTING_PLAN.md` §8.

use bevy::prelude::*;
use std::{collections::HashMap, sync::Arc};

use super::baking::probes::ChunkProbes;

/// Probe grids of every loaded chunk, keyed by chunk coordinates.
#[derive(Resource, Default)]
pub struct ProbeStore {
    grids: HashMap<(i32, i32), Arc<ChunkProbes>>,
    /// Set whenever a grid is added, replaced or removed; whoever consumes
    /// the store clears it.
    pub dirty: bool,
}

impl ProbeStore {
    pub fn insert(&mut self, probes: ChunkProbes) {
        self.grids
            .insert((probes.chunk_x, probes.chunk_y), Arc::new(probes));
        self.dirty = true;
    }

    pub fn remove(&mut self, chunk: (i32, i32)) {
        if self.grids.remove(&chunk).is_some() {
            self.dirty = true;
        }
    }

    pub fn clear(&mut self) {
        if !self.grids.is_empty() {
            self.grids.clear();
            self.dirty = true;
        }
    }
}
