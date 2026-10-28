//! Thumbnail cache for the Assets tab. Phase 2 of the browser ships the
//! cache and the request queue; the offscreen renderer that fills it lands
//! in phase 3, so for now every cell draws its glyph.

use std::collections::{HashMap, HashSet, VecDeque};

use bevy::prelude::*;

use super::catalog::{AssetEntry, AssetKind};

#[derive(Resource, Default)]
pub struct ThumbnailCache {
    /// Finished thumbnails by catalog entry path.
    images: HashMap<String, Handle<Image>>,
    /// Entries waiting to be rendered, oldest first; `requested` dedups.
    pub pending: VecDeque<ThumbnailJob>,
    requested: HashSet<String>,
}

#[derive(Debug, Clone)]
pub enum ThumbnailJob {
    Mesh { path: String },
    Prefab { name: String },
}

impl ThumbnailCache {
    pub fn get(&self, path: &str) -> Option<&Handle<Image>> {
        self.images.get(path)
    }

    pub fn images(&self) -> impl Iterator<Item = &Handle<Image>> {
        self.images.values()
    }

    /// Queue a render for `entry` if it can have one and isn't already
    /// queued. Only cells that are actually drawn ask, so a huge catalog
    /// doesn't render everything up front.
    pub fn request(&mut self, entry: &AssetEntry) {
        let key = entry.thumbnail_key().to_owned();
        let job = match (entry.kind, &entry.prefab) {
            (AssetKind::Prefab, Some(name)) => ThumbnailJob::Prefab { name: name.clone() },
            // Presets key on their mesh path, so they share its thumbnail.
            (AssetKind::Mesh, None) => ThumbnailJob::Mesh { path: key.clone() },
            // Built-ins, textures and audio draw glyphs or themselves.
            _ => return,
        };
        if self.requested.insert(key) {
            self.pending.push_back(job);
        }
    }

    pub fn insert(&mut self, path: String, image: Handle<Image>) {
        self.images.insert(path, image);
    }
}
