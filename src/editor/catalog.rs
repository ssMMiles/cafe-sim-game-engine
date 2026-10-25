//! The placement catalog: everything the Place tool can put in the world.
//!
//! Entries come from three sources:
//! 1. meshes listed in the build-time `meshes/manifest.manifest`,
//! 2. two built-in light kinds (and an empty anchor),
//! 3. one preset per plugin script that declares a `default_mesh` — picking it
//!    selects that mesh *and* arms stamp mode with the script, so a plugin's
//!    catalog entry is a one-click "mesh + binding" placement.
//!
//! Rebuilt whenever the script registry changes (plugin load, unload,
//! hot-reload) so presets track the loaded plugins. The Place panel
//! (`ui::place`) draws it.

use bevy::{
    asset::{AssetLoader, LoadContext, io::Reader},
    prelude::*,
};
use engine_api::ScriptRegistry;

use crate::game::world::objects::{PlacedKind, PlacementConfig};

/// A raw line from the mesh manifest.
#[derive(Clone, Debug)]
pub struct MeshEntry {
    pub name: String,
    pub asset_path: String,
}

/// One selectable entry.
#[derive(Clone, Debug)]
pub struct CatalogEntry {
    pub name: String,
    pub kind: PlacedKind,
    /// Set for plugin presets: selecting the entry also arms stamp mode with
    /// this `(plugin, script)`.
    pub preset_script: Option<(String, String)>,
}

#[derive(Resource, Default)]
pub struct MeshCatalog {
    /// Manifest meshes, kept so the list can be rebuilt when presets change.
    manifest: Vec<MeshEntry>,
    pub entries: Vec<CatalogEntry>,
}

impl MeshCatalog {
    /// Recompose `entries` from the manifest, the built-ins, and the
    /// registry's presets.
    fn rebuild(&mut self, registry: &ScriptRegistry) {
        let mut entries: Vec<CatalogEntry> = self
            .manifest
            .iter()
            .map(|m| CatalogEntry {
                name: m.name.clone(),
                kind: PlacedKind::mesh(m.asset_path.clone()),
                preset_script: None,
            })
            .collect();

        entries.push(CatalogEntry {
            name: "Point Light".into(),
            kind: PlacedKind::point_light(),
            preset_script: None,
        });
        entries.push(CatalogEntry {
            name: "Spot Light".into(),
            kind: PlacedKind::spot_light(),
            preset_script: None,
        });
        entries.push(CatalogEntry {
            name: "Empty".into(),
            kind: PlacedKind::Empty,
            preset_script: None,
        });
        entries.push(CatalogEntry {
            name: "Group".into(),
            kind: PlacedKind::Group,
            preset_script: None,
        });

        let mut presets: Vec<CatalogEntry> = registry
            .scripts()
            .filter_map(|s| {
                let mesh = s.schema.default_mesh?;
                Some(CatalogEntry {
                    name: format!("{}/{}", s.plugin, s.schema.name),
                    kind: PlacedKind::mesh(mesh),
                    preset_script: Some((s.plugin.clone(), s.schema.name.to_owned())),
                })
            })
            .collect();
        // HashMap iteration order is arbitrary; keep the list stable.
        presets.sort_by(|a, b| a.name.cmp(&b.name));
        entries.extend(presets);

        self.entries = entries;
    }

    /// Index of the entry the placement config currently describes.
    pub fn selected_index(&self, config: &PlacementConfig) -> Option<usize> {
        self.entries
            .iter()
            .position(|e| e.kind == config.kind && e.preset_script == config.stamp_script)
            .or_else(|| self.entries.iter().position(|e| e.kind == config.kind))
    }

    /// Make `entry` the next thing placed.
    ///
    /// A preset owns the stamp for as long as it is selected. Stepping off it
    /// disarms the stamp it armed — otherwise every later plain mesh would
    /// keep stamping that script, so a rock placed after the cafe shell would
    /// come up as a second `CafeShell`. A stamp armed from the Place panel's
    /// own stamp picker is left alone, and so stays sticky across mesh
    /// changes.
    pub fn select(&self, index: usize, config: &mut PlacementConfig) {
        let Some(entry) = self.entries.get(index) else {
            return;
        };
        let outgoing_preset = self
            .selected_index(config)
            .and_then(|i| self.entries.get(i))
            .and_then(|e| e.preset_script.clone());

        config.kind = entry.kind.clone();
        config.scale = Vec3::ONE;
        if entry.preset_script.is_some() {
            config.stamp_script = entry.preset_script.clone();
        } else if config.stamp_script.is_some() && config.stamp_script == outgoing_preset {
            config.stamp_script = None;
        }
    }
}

// ── Manifest asset ────────────────────────────────────────────────────────────

#[derive(Asset, TypePath)]
pub struct MeshManifestAsset(pub Vec<MeshEntry>);

#[derive(Resource)]
pub struct MeshManifestHandle(pub Handle<MeshManifestAsset>);

#[derive(Default, TypePath)]
pub struct MeshManifestLoader;

impl AssetLoader for MeshManifestLoader {
    type Asset = MeshManifestAsset;
    type Settings = ();
    type Error = std::io::Error;

    async fn load(
        &self,
        reader: &mut dyn Reader,
        _settings: &Self::Settings,
        _load_context: &mut LoadContext<'_>,
    ) -> Result<Self::Asset, Self::Error> {
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes).await?;
        let text = String::from_utf8_lossy(&bytes);

        let entries = text
            .lines()
            .filter(|l| !l.trim().is_empty())
            .filter_map(|line| {
                let (name, path) = line.split_once('|')?;
                Some(MeshEntry {
                    name: name.to_string(),
                    asset_path: path.to_string(),
                })
            })
            .collect();

        Ok(MeshManifestAsset(entries))
    }

    fn extensions(&self) -> &[&str] {
        &["manifest"]
    }
}

// ── Systems ───────────────────────────────────────────────────────────────────

pub fn request_mesh_manifest(mut commands: Commands, asset_server: Res<AssetServer>) {
    let handle = asset_server.load::<MeshManifestAsset>("meshes/manifest.manifest");
    commands.insert_resource(MeshManifestHandle(handle));
}

/// Ingest the manifest once it loads, then keep `entries` in step with the
/// script registry so plugin presets appear and disappear with their plugins.
pub fn populate_mesh_catalog(
    mut commands: Commands,
    handle: Option<Res<MeshManifestHandle>>,
    manifests: Res<Assets<MeshManifestAsset>>,
    asset_server: Res<AssetServer>,
    registry: Res<ScriptRegistry>,
    mut catalog: ResMut<MeshCatalog>,
    mut config: ResMut<PlacementConfig>,
) {
    let mut manifest_changed = false;

    if let Some(handle) = handle {
        match asset_server.load_state(&handle.0) {
            bevy::asset::LoadState::Loaded => {
                if let Some(manifest) = manifests.get(&handle.0) {
                    catalog.manifest = manifest.0.clone();
                    manifest_changed = true;
                    info!("Loaded {} meshes from manifest", catalog.manifest.len());
                    commands.remove_resource::<MeshManifestHandle>();
                }
            }
            bevy::asset::LoadState::Failed(err) => {
                warn!("Failed to load mesh manifest: {err:?} — catalog will have no meshes");
                manifest_changed = true;
                commands.remove_resource::<MeshManifestHandle>();
            }
            _ => {}
        }
    }

    if !manifest_changed && !registry.is_changed() {
        return;
    }

    catalog.rebuild(&registry);

    // First rebuild: make sure the config's kind is something the catalog
    // actually offers, so the preview and the highlight agree.
    if manifest_changed
        && catalog.selected_index(&config).is_none()
        && let Some(entry) = catalog.entries.first()
    {
        config.kind = entry.kind.clone();
    }
}
