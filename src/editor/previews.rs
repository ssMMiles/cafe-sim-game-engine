//! Images the editor chrome shows: texture previews now, rendered
//! thumbnails later. egui can only draw a bevy `Image` once it has been
//! registered with the egui context, and that registration needs
//! `EguiContexts`, which only `editor_ui` holds — so this resource is the
//! hand-off: panels put handles in, `editor_ui` binds whatever is new each
//! pass, panels read the resulting `TextureId`s back.

use std::collections::HashMap;

use bevy::asset::AssetId;
use bevy::prelude::*;
use bevy_egui::egui;

#[derive(Resource, Default)]
pub struct PreviewImages {
    /// Full-size texture assets, loaded on first request, by asset path.
    textures: HashMap<String, Handle<Image>>,
    /// egui-side ids for every bound image.
    egui_ids: HashMap<AssetId<Image>, egui::TextureId>,
}

impl PreviewImages {
    /// The texture at `path`, loading it on first use.
    pub fn texture(&mut self, path: &str, asset_server: &AssetServer) -> Handle<Image> {
        self.textures
            .entry(path.to_owned())
            .or_insert_with(|| asset_server.load(path.to_owned()))
            .clone()
    }

    pub fn egui_id(&self, image: &Handle<Image>) -> Option<egui::TextureId> {
        self.egui_ids.get(&image.id()).copied()
    }

    /// Handles not yet known to egui. `editor_ui` binds these each pass.
    pub fn unbound<'a>(
        &'a self,
        extra: impl Iterator<Item = &'a Handle<Image>> + 'a,
    ) -> Vec<Handle<Image>> {
        self.textures
            .values()
            .chain(extra)
            .filter(|h| !self.egui_ids.contains_key(&h.id()))
            .cloned()
            .collect()
    }

    pub fn bound(&mut self, image: &Handle<Image>, id: egui::TextureId) {
        self.egui_ids.insert(image.id(), id);
    }
}
