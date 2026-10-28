//! The asset catalog: everything the editor can browse, and the subset the
//! Place tool can put in the world.
//!
//! Entries come from four sources:
//! 1. the build-time `asset_manifest.manifest` (meshes, textures, audio —
//!    `build.rs` writes it because WASM cannot list directories),
//! 2. the built-in placeables (two light kinds, an empty anchor, a group),
//! 3. the `PrefabLibrary` (runtime-discovered, so never stale),
//! 4. one preset per plugin script that declares a `default_mesh` — picking it
//!    selects that mesh *and* arms stamp mode with the script, so a plugin's
//!    catalog entry is a one-click "mesh + binding" placement.
//!
//! Rebuilt whenever the manifest lands, the script registry changes (plugin
//! load, unload, hot-reload) or the prefab library changes. The Assets tab
//! (`ui::assets`) draws it by category; textures and audio are browse-only
//! (`placed == None`) until something in the level consumes them.

use bevy::{
    asset::{AssetLoader, LoadContext, io::Reader},
    prelude::*,
};
use engine_api::ScriptRegistry;

use crate::game::world::objects::{PlacedKind, PlacementConfig};
use crate::game::world::prefabs::PrefabLibrary;

/// Category of a catalog entry, as the Assets tab groups them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AssetKind {
    Mesh,
    /// Engine primitives that aren't files: lights, the empty anchor, groups.
    Builtin,
    Prefab,
    Texture,
    Audio,
}

impl AssetKind {
    pub const ALL: [AssetKind; 5] = [
        AssetKind::Mesh,
        AssetKind::Builtin,
        AssetKind::Prefab,
        AssetKind::Texture,
        AssetKind::Audio,
    ];

    pub fn label(self) -> &'static str {
        match self {
            AssetKind::Mesh => "Meshes",
            AssetKind::Builtin => "Built-ins",
            AssetKind::Prefab => "Prefabs",
            AssetKind::Texture => "Textures",
            AssetKind::Audio => "Audio",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        match s {
            "mesh" => Some(AssetKind::Mesh),
            "texture" => Some(AssetKind::Texture),
            "audio" => Some(AssetKind::Audio),
            _ => None,
        }
    }
}

/// A raw line from the asset manifest.
#[derive(Clone, Debug)]
pub struct ManifestEntry {
    pub kind: AssetKind,
    pub name: String,
    pub asset_path: String,
}

/// One browsable asset.
#[derive(Clone, Debug)]
pub struct AssetEntry {
    pub kind: AssetKind,
    pub name: String,
    /// Asset path (meshes/textures/audio), prefab name, or a synthetic
    /// `builtin://…` id. Unique within the catalog; keys thumbnails.
    pub path: String,
    /// What selecting this entry arms for placement; `None` for browse-only
    /// kinds (textures, audio).
    pub placed: Option<PlacedKind>,
    /// Set for plugin presets: selecting the entry also arms stamp mode with
    /// this `(plugin, script)`.
    pub preset_script: Option<(String, String)>,
    /// Set for prefab entries: selecting places an instance of this prefab.
    pub prefab: Option<String>,
}

impl AssetEntry {
    fn placeable(
        kind: AssetKind,
        name: impl Into<String>,
        path: impl Into<String>,
        placed: PlacedKind,
    ) -> Self {
        Self {
            kind,
            name: name.into(),
            path: path.into(),
            placed: Some(placed),
            preset_script: None,
            prefab: None,
        }
    }

    pub fn is_placeable(&self) -> bool {
        self.placed.is_some()
    }
}

#[derive(Resource, Default)]
pub struct AssetCatalog {
    /// Manifest lines, kept so the list can be rebuilt when presets or
    /// prefabs change.
    manifest: Vec<ManifestEntry>,
    pub entries: Vec<AssetEntry>,
}

impl AssetCatalog {
    /// Recompose `entries` from the manifest, the built-ins, the prefab
    /// library and the registry's presets.
    fn rebuild(&mut self, registry: &ScriptRegistry, prefabs: &PrefabLibrary) {
        let mut entries: Vec<AssetEntry> = self
            .manifest
            .iter()
            .map(|m| match m.kind {
                AssetKind::Mesh => AssetEntry::placeable(
                    AssetKind::Mesh,
                    m.name.clone(),
                    m.asset_path.clone(),
                    PlacedKind::mesh(m.asset_path.clone()),
                ),
                kind => AssetEntry {
                    kind,
                    name: m.name.clone(),
                    path: m.asset_path.clone(),
                    placed: None,
                    preset_script: None,
                    prefab: None,
                },
            })
            .collect();

        entries.extend([
            AssetEntry::placeable(
                AssetKind::Builtin,
                "Point Light",
                "builtin://point_light",
                PlacedKind::point_light(),
            ),
            AssetEntry::placeable(
                AssetKind::Builtin,
                "Spot Light",
                "builtin://spot_light",
                PlacedKind::spot_light(),
            ),
            AssetEntry::placeable(
                AssetKind::Builtin,
                "Empty",
                "builtin://empty",
                PlacedKind::Empty,
            ),
            AssetEntry::placeable(
                AssetKind::Builtin,
                "Group",
                "builtin://group",
                PlacedKind::Group,
            ),
        ]);

        for name in prefabs.names() {
            let Some(def) = prefabs.get(&name) else {
                continue;
            };
            entries.push(AssetEntry {
                kind: AssetKind::Prefab,
                name: name.clone(),
                path: format!("prefab://{name}"),
                placed: Some(def.entities[0].kind.clone()),
                preset_script: None,
                prefab: Some(name),
            });
        }

        let mut presets: Vec<AssetEntry> = registry
            .scripts()
            .filter_map(|s| {
                let mesh = s.schema.default_mesh?;
                Some(AssetEntry {
                    kind: AssetKind::Mesh,
                    name: format!("{}/{}", s.plugin, s.schema.name),
                    path: format!("preset://{}/{}", s.plugin, s.schema.name),
                    placed: Some(PlacedKind::mesh(mesh)),
                    preset_script: Some((s.plugin.clone(), s.schema.name.to_owned())),
                    prefab: None,
                })
            })
            .collect();
        // HashMap iteration order is arbitrary; keep the list stable.
        presets.sort_by(|a, b| a.name.cmp(&b.name));
        entries.extend(presets);

        self.entries = entries;
    }

    pub fn of_kind(&self, kind: AssetKind) -> impl Iterator<Item = (usize, &AssetEntry)> {
        self.entries
            .iter()
            .enumerate()
            .filter(move |(_, e)| e.kind == kind)
    }

    /// Index of the entry the placement config currently describes.
    pub fn selected_index(&self, config: &PlacementConfig) -> Option<usize> {
        if let Some(prefab) = &config.prefab {
            return self
                .entries
                .iter()
                .position(|e| e.prefab.as_deref() == Some(prefab.as_str()));
        }
        let matches_kind =
            |e: &AssetEntry| e.prefab.is_none() && e.placed.as_ref() == Some(&config.kind);
        self.entries
            .iter()
            .position(|e| matches_kind(e) && e.preset_script == config.stamp_script)
            .or_else(|| self.entries.iter().position(matches_kind))
    }

    /// Make `entry` the next thing placed.
    ///
    /// A preset owns the stamp for as long as it is selected. Stepping off it
    /// disarms the stamp it armed — otherwise every later plain mesh would
    /// keep stamping that script, so a rock placed after the cafe shell would
    /// come up as a second `CafeShell`. A stamp armed from the Place panel's
    /// own stamp picker is left alone, and so stays sticky across mesh
    /// changes. Prefabs carry their own scripts, so selecting one clears the
    /// stamp outright.
    pub fn select(&self, index: usize, config: &mut PlacementConfig) {
        let Some(entry) = self.entries.get(index) else {
            return;
        };
        let Some(placed) = &entry.placed else {
            return; // browse-only
        };
        let outgoing_preset = self
            .selected_index(config)
            .and_then(|i| self.entries.get(i))
            .and_then(|e| e.preset_script.clone());

        config.kind = placed.clone();
        config.prefab = entry.prefab.clone();
        config.scale = Vec3::ONE;
        if entry.prefab.is_some() {
            config.stamp_script = None;
        } else if entry.preset_script.is_some() {
            config.stamp_script = entry.preset_script.clone();
        } else if config.stamp_script.is_some() && config.stamp_script == outgoing_preset {
            config.stamp_script = None;
        }
    }
}

// ── Manifest asset ────────────────────────────────────────────────────────────

#[derive(Asset, TypePath)]
pub struct AssetManifestAsset(pub Vec<ManifestEntry>);

#[derive(Resource)]
pub struct AssetManifestHandle(pub Handle<AssetManifestAsset>);

#[derive(Default, TypePath)]
pub struct AssetManifestLoader;

impl AssetLoader for AssetManifestLoader {
    type Asset = AssetManifestAsset;
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
        Ok(AssetManifestAsset(parse_manifest(
            &String::from_utf8_lossy(&bytes),
        )))
    }

    fn extensions(&self) -> &[&str] {
        &["manifest"]
    }
}

/// `kind|name|path` per line; unknown kinds are skipped (forward
/// compatibility with a newer `build.rs`).
pub fn parse_manifest(text: &str) -> Vec<ManifestEntry> {
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|line| {
            let mut parts = line.splitn(3, '|');
            let kind = parts.next()?;
            let name = parts.next()?;
            let path = parts.next()?;
            let Some(kind) = AssetKind::parse(kind) else {
                warn!("asset manifest: unknown kind '{kind}' in '{line}' — skipped");
                return None;
            };
            Some(ManifestEntry {
                kind,
                name: name.to_string(),
                asset_path: path.to_string(),
            })
        })
        .collect()
}

// ── Systems ───────────────────────────────────────────────────────────────────

pub fn request_asset_manifest(mut commands: Commands, asset_server: Res<AssetServer>) {
    let handle = asset_server.load::<AssetManifestAsset>("asset_manifest.manifest");
    commands.insert_resource(AssetManifestHandle(handle));
}

/// Ingest the manifest once it loads, then keep `entries` in step with the
/// script registry and the prefab library.
pub fn populate_asset_catalog(
    mut commands: Commands,
    handle: Option<Res<AssetManifestHandle>>,
    manifests: Res<Assets<AssetManifestAsset>>,
    asset_server: Res<AssetServer>,
    registry: Res<ScriptRegistry>,
    prefabs: Res<PrefabLibrary>,
    mut catalog: ResMut<AssetCatalog>,
    mut config: ResMut<PlacementConfig>,
) {
    let mut manifest_changed = false;

    if let Some(handle) = handle {
        match asset_server.load_state(&handle.0) {
            bevy::asset::LoadState::Loaded => {
                if let Some(manifest) = manifests.get(&handle.0) {
                    catalog.manifest = manifest.0.clone();
                    manifest_changed = true;
                    info!("Loaded {} assets from the manifest", catalog.manifest.len());
                    commands.remove_resource::<AssetManifestHandle>();
                }
            }
            bevy::asset::LoadState::Failed(err) => {
                warn!("Failed to load the asset manifest: {err:?} — catalog will have no files");
                manifest_changed = true;
                commands.remove_resource::<AssetManifestHandle>();
            }
            _ => {}
        }
    }

    if !manifest_changed && !registry.is_changed() && !prefabs.is_changed() {
        return;
    }

    catalog.rebuild(&registry, &prefabs);

    // First rebuild: make sure the config's kind is something the catalog
    // actually offers, so the preview and the highlight agree.
    if manifest_changed
        && catalog.selected_index(&config).is_none()
        && let Some(entry) = catalog.entries.iter().find(|e| e.is_placeable())
    {
        config.kind = entry.placed.clone().expect("placeable");
        config.prefab = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn catalog_with(manifest: &str) -> AssetCatalog {
        let mut catalog = AssetCatalog {
            manifest: parse_manifest(manifest),
            entries: Vec::new(),
        };
        catalog.rebuild(&ScriptRegistry::default(), &PrefabLibrary::default());
        catalog
    }

    #[test]
    fn manifest_lines_parse_by_kind_and_skip_unknown() {
        let entries = parse_manifest(
            "mesh|rock|meshes/rock/bigrock.gltf#Scene0\ntexture|terrain|textures/terrain.png\nvideo|x|y\n\n",
        );
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].kind, AssetKind::Mesh);
        assert_eq!(entries[1].kind, AssetKind::Texture);
    }

    #[test]
    fn textures_are_browse_only_and_builtins_are_placeable() {
        let catalog =
            catalog_with("mesh|rock|meshes/rock/r.gltf#Scene0\ntexture|t|textures/t.png\n");
        let (_, texture) = catalog.of_kind(AssetKind::Texture).next().unwrap();
        assert!(!texture.is_placeable());
        assert_eq!(catalog.of_kind(AssetKind::Builtin).count(), 4);
        let mut config = PlacementConfig::default();
        let (i, _) = catalog.of_kind(AssetKind::Mesh).next().unwrap();
        catalog.select(i, &mut config);
        assert_eq!(config.kind, PlacedKind::mesh("meshes/rock/r.gltf#Scene0"));
        assert_eq!(catalog.selected_index(&config), Some(i));
    }

    #[test]
    fn stepping_off_a_preset_disarms_its_stamp_but_not_a_manual_one() {
        let mut catalog = catalog_with("mesh|rock|meshes/rock/r.gltf#Scene0\n");
        catalog.entries.push(AssetEntry {
            kind: AssetKind::Mesh,
            name: "p/s".into(),
            path: "preset://p/s".into(),
            placed: Some(PlacedKind::mesh("meshes/p.gltf#Scene0")),
            preset_script: Some(("p".into(), "s".into())),
            prefab: None,
        });
        let preset = catalog.entries.len() - 1;
        let mut config = PlacementConfig::default();
        catalog.select(preset, &mut config);
        assert_eq!(config.stamp_script, Some(("p".into(), "s".into())));
        catalog.select(0, &mut config);
        assert_eq!(config.stamp_script, None, "preset's stamp disarmed");

        config.stamp_script = Some(("manual".into(), "x".into()));
        catalog.select(0, &mut config);
        assert!(config.stamp_script.is_some(), "manual stamp is sticky");
    }
}
