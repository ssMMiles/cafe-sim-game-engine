//! Project discovery and active-project tracking.
//!
//! A "project" is a directory under `<workspace>/projects/` containing a
//! `project.toml` manifest. The manifest declares which plugins and worlds
//! belong to the project; plugin sources and world saves still live in the
//! top-level `plugins/` and `worlds/` directories and are referenced by id.
//!
//! On wasm there is no such directory, so manifests live in OPFS under the
//! same `projects/{id}/project.toml` key shape. Discovery there is async, so
//! the registry is seeded with a fallback project immediately and the real
//! contents land a few frames later — see [`ProjectsPlugin`].

use bevy::prelude::*;

#[cfg(not(target_arch = "wasm32"))]
use std::path::Path;

// ---------------------------------------------------------------------------
// Manifest types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct ProjectManifest {
    pub id: String,
    pub name: String,
    #[allow(dead_code)]
    pub description: Option<String>,
    pub plugins: Vec<String>,
    #[allow(dead_code)]
    pub worlds: Vec<String>,
    pub start_world: String,
}

fn parse_manifest(id: &str, source: &str) -> Result<ProjectManifest, String> {
    let value: toml::Value = source
        .parse()
        .map_err(|e: toml::de::Error| format!("toml parse: {e}"))?;
    let table = value
        .as_table()
        .ok_or_else(|| "manifest root must be a table".to_string())?;

    let name = table
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "missing `name`".to_string())?
        .to_owned();

    let description = table
        .get("description")
        .and_then(|v| v.as_str())
        .map(|s| s.to_owned());

    let plugins = table
        .get("plugins")
        .map(|v| {
            v.as_array()
                .ok_or_else(|| "`plugins` must be an array".to_string())?
                .iter()
                .map(|e| {
                    e.as_str()
                        .ok_or_else(|| "`plugins` entries must be strings".to_string())
                        .map(|s| s.to_owned())
                })
                .collect::<Result<Vec<_>, _>>()
        })
        .transpose()?
        .unwrap_or_default();

    let worlds = table
        .get("worlds")
        .map(|v| {
            v.as_array()
                .ok_or_else(|| "`worlds` must be an array".to_string())?
                .iter()
                .map(|e| {
                    e.as_str()
                        .ok_or_else(|| "`worlds` entries must be strings".to_string())
                        .map(|s| s.to_owned())
                })
                .collect::<Result<Vec<_>, _>>()
        })
        .transpose()?
        .unwrap_or_default();

    let start_world = table
        .get("start_world")
        .and_then(|v| v.as_str())
        .map(|s| s.to_owned())
        .or_else(|| worlds.first().cloned())
        .ok_or_else(|| "missing `start_world` and `worlds` is empty".to_string())?;

    Ok(ProjectManifest {
        id: id.to_owned(),
        name,
        description,
        plugins,
        worlds,
        start_world,
    })
}

// ---------------------------------------------------------------------------
// Resources
// ---------------------------------------------------------------------------

#[derive(Resource, Default, Debug)]
pub struct ProjectRegistry {
    pub projects: Vec<ProjectManifest>,
}

impl ProjectRegistry {
    pub fn find(&self, id: &str) -> Option<&ProjectManifest> {
        self.projects.iter().find(|p| p.id == id)
    }
}

#[derive(Resource, Clone, Debug)]
pub struct ActiveProject(pub ProjectManifest);

// ---------------------------------------------------------------------------
// Plugin
// ---------------------------------------------------------------------------

pub struct ProjectsPlugin;

impl Plugin for ProjectsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<ProjectRegistry>();
        app.add_systems(Startup, populate_registry);

        // OPFS discovery can't finish inside a startup system, so its result
        // is polled into the registry once the future lands.
        #[cfg(target_arch = "wasm32")]
        {
            app.init_resource::<PendingProjectDiscovery>();
            app.add_systems(Update, poll_project_discovery);
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn populate_registry(mut registry: ResMut<ProjectRegistry>) {
    registry.projects = discover_projects();
    if registry.projects.is_empty() {
        warn!("[projects] no projects discovered under `projects/`");
    } else {
        info!(
            "[projects] discovered {} project(s): {:?}",
            registry.projects.len(),
            registry
                .projects
                .iter()
                .map(|p| p.id.as_str())
                .collect::<Vec<_>>()
        );
    }
}

/// The project shown when storage holds none yet, so the menu is never empty.
/// Not written to OPFS — the first edit (adding a world, say) persists it.
#[cfg(target_arch = "wasm32")]
fn fallback_project() -> ProjectManifest {
    ProjectManifest {
        id: "example-game".to_owned(),
        name: "Example Game".to_owned(),
        description: None,
        plugins: Vec::new(),
        worlds: vec!["default".to_owned()],
        start_world: "default".to_owned(),
    }
}

/// Slot holding an in-flight OPFS project scan.
#[cfg(target_arch = "wasm32")]
#[derive(Resource, Default)]
struct PendingProjectDiscovery(
    Option<std::sync::Arc<std::sync::Mutex<Option<Vec<ProjectManifest>>>>>,
);

#[cfg(target_arch = "wasm32")]
fn populate_registry(
    mut registry: ResMut<ProjectRegistry>,
    mut pending: ResMut<PendingProjectDiscovery>,
) {
    use std::sync::{Arc, Mutex};

    // Seed synchronously so the menu has something to draw this frame; the
    // OPFS scan below replaces this if it finds anything.
    registry.projects = vec![fallback_project()];

    let slot: Arc<Mutex<Option<Vec<ProjectManifest>>>> = Arc::new(Mutex::new(None));
    pending.0 = Some(slot.clone());
    wasm_bindgen_futures::spawn_local(async move {
        *slot.lock().unwrap() = Some(discover_projects_opfs().await);
    });
}

/// Read every `projects/*/project.toml` out of OPFS.
#[cfg(target_arch = "wasm32")]
async fn discover_projects_opfs() -> Vec<ProjectManifest> {
    let storage = crate::storage::wasm::WasmStorage;
    let keys = match storage.list_keys("projects").await {
        Ok(k) => k,
        Err(e) => {
            warn!("[projects] OPFS scan failed: {e}");
            return Vec::new();
        }
    };

    let mut projects = Vec::new();
    for key in keys.iter().filter(|k| k.ends_with("/project.toml")) {
        // `projects/{id}/project.toml`
        let Some(id) = key.split('/').nth(1) else {
            continue;
        };
        let bytes = match storage.load(key).await {
            Ok(Some(b)) => b,
            Ok(None) => continue,
            Err(e) => {
                warn!("[projects] failed to read {key}: {e}");
                continue;
            }
        };
        let source = match String::from_utf8(bytes) {
            Ok(s) => s,
            Err(e) => {
                warn!("[projects] {key} is not valid UTF-8: {e}");
                continue;
            }
        };
        match parse_manifest(id, &source) {
            Ok(m) => projects.push(m),
            Err(e) => warn!("[projects] failed to parse {key}: {e}"),
        }
    }
    projects.sort_by(|a, b| a.id.cmp(&b.id));
    projects
}

/// Swap discovered projects into the registry once the scan completes.
#[cfg(target_arch = "wasm32")]
fn poll_project_discovery(
    mut pending: ResMut<PendingProjectDiscovery>,
    mut registry: ResMut<ProjectRegistry>,
) {
    let Some(slot) = pending.0.as_ref() else {
        return;
    };
    let Some(found) = slot.lock().unwrap().take() else {
        return;
    };
    pending.0 = None;

    if found.is_empty() {
        info!("[projects] no projects in OPFS yet; keeping the fallback project");
        return;
    }
    info!(
        "[projects] loaded {} project(s) from OPFS: {:?}",
        found.len(),
        found.iter().map(|p| p.id.as_str()).collect::<Vec<_>>()
    );
    registry.projects = found;
}

/// Scan `<workspace>/plugins/` for plugin source directories. Returns the
/// list of ids (= directory names) that have a `Cargo.toml`. Used to
/// populate the create-project UI's plugin checkboxes.
#[cfg(not(target_arch = "wasm32"))]
pub fn discover_plugin_ids() -> Vec<String> {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let plugins_dir = Path::new(manifest_dir).join("plugins");
    let entries = match std::fs::read_dir(&plugins_dir) {
        Ok(e) => e,
        Err(e) => {
            warn!(
                "[projects] could not read plugins dir at {}: {e}",
                plugins_dir.display()
            );
            return Vec::new();
        }
    };
    let mut ids = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        if !path.join("Cargo.toml").exists() {
            continue;
        }
        if let Some(name) = path.file_name().and_then(|s| s.to_str()) {
            ids.push(name.to_owned());
        }
    }
    ids.sort();
    ids
}

#[cfg(target_arch = "wasm32")]
pub fn discover_plugin_ids() -> Vec<String> {
    Vec::new()
}

/// Write `manifest` to `projects/<manifest.id>/project.toml`. Used both for
/// new projects and for updating an existing manifest in place (e.g. when
/// adding a world).
///
/// Natively this is a synchronous file write and the error is real. On wasm
/// it hands the bytes to a `spawn_local` OPFS save and returns `Ok`
/// immediately — a failure there is logged, not reported back, the same
/// fire-and-forget bargain the terrain and lightmap saves strike. Callers
/// must therefore not treat `Ok` as "durably stored" on the web.
pub fn write_manifest(manifest: &ProjectManifest) -> Result<(), String> {
    let body = serialize_manifest(manifest);

    #[cfg(not(target_arch = "wasm32"))]
    {
        let manifest_dir = env!("CARGO_MANIFEST_DIR");
        let project_dir = Path::new(manifest_dir).join("projects").join(&manifest.id);
        std::fs::create_dir_all(&project_dir).map_err(|e| e.to_string())?;
        std::fs::write(project_dir.join("project.toml"), body).map_err(|e| e.to_string())?;
    }

    #[cfg(target_arch = "wasm32")]
    {
        let key = format!("projects/{}/project.toml", manifest.id);
        wasm_bindgen_futures::spawn_local(async move {
            if let Err(e) = crate::storage::wasm::WasmStorage
                .save(&key, body.as_bytes())
                .await
            {
                error!("[projects] OPFS manifest save failed for {key}: {e}");
            }
        });
    }

    Ok(())
}

/// Hand-rolled TOML serialization for the manifest schema. Avoids pulling in
/// serde just for one write path. Strings are quoted with `"` and escaped
/// for `\` and `"`.
fn serialize_manifest(m: &ProjectManifest) -> String {
    let mut out = String::new();
    out.push_str(&format!("name = {}\n", quote(&m.name)));
    if let Some(desc) = m.description.as_deref()
        && !desc.is_empty()
    {
        out.push_str(&format!("description = {}\n", quote(desc)));
    }
    out.push_str(&format!("plugins = {}\n", quote_array(&m.plugins)));
    out.push_str(&format!("worlds = {}\n", quote_array(&m.worlds)));
    out.push_str(&format!("start_world = {}\n", quote(&m.start_world)));
    out
}

fn quote(s: &str) -> String {
    let escaped = s.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{escaped}\"")
}

fn quote_array(items: &[String]) -> String {
    let parts: Vec<String> = items.iter().map(|s| quote(s)).collect();
    format!("[{}]", parts.join(", "))
}

#[cfg(not(target_arch = "wasm32"))]
fn discover_projects() -> Vec<ProjectManifest> {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let projects_dir = Path::new(manifest_dir).join("projects");
    let entries = match std::fs::read_dir(&projects_dir) {
        Ok(e) => e,
        Err(e) => {
            warn!(
                "[projects] could not read projects dir at {}: {e}",
                projects_dir.display()
            );
            return Vec::new();
        }
    };

    let mut projects = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let manifest_path = path.join("project.toml");
        if !manifest_path.exists() {
            continue;
        }
        let Some(id) = path.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        let source = match std::fs::read_to_string(&manifest_path) {
            Ok(s) => s,
            Err(e) => {
                warn!("[projects] failed to read {}: {e}", manifest_path.display());
                continue;
            }
        };
        match parse_manifest(id, &source) {
            Ok(m) => projects.push(m),
            Err(e) => warn!(
                "[projects] failed to parse {}: {e}",
                manifest_path.display()
            ),
        }
    }
    projects.sort_by(|a, b| a.id.cmp(&b.id));
    projects
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Manifests are written by hand-rolled TOML and read back by `toml`, and
    /// on wasm that round trip is the only thing standing between "created a
    /// project" and "project still there after a refresh". Anything the
    /// serializer emits must parse back identically.
    #[test]
    fn manifest_survives_a_serialize_parse_round_trip() {
        let original = ProjectManifest {
            id: "my-game".to_owned(),
            name: "My Game".to_owned(),
            description: Some("a description".to_owned()),
            plugins: vec!["cafe-sim".to_owned(), "example".to_owned()],
            worlds: vec!["default".to_owned(), "level-2".to_owned()],
            start_world: "default".to_owned(),
        };

        let parsed = parse_manifest(&original.id, &serialize_manifest(&original)).unwrap();
        assert_eq!(parsed.id, original.id);
        assert_eq!(parsed.name, original.name);
        assert_eq!(parsed.description, original.description);
        assert_eq!(parsed.plugins, original.plugins);
        assert_eq!(parsed.worlds, original.worlds);
        assert_eq!(parsed.start_world, original.start_world);
    }

    /// A world added in the editor is persisted by rewriting the whole
    /// manifest, so the new id has to come back on the next load — this is
    /// the "new worlds aren't persisted" case.
    #[test]
    fn an_added_world_round_trips() {
        let mut manifest = ProjectManifest {
            id: "p".to_owned(),
            name: "P".to_owned(),
            description: None,
            plugins: Vec::new(),
            worlds: vec!["default".to_owned()],
            start_world: "default".to_owned(),
        };
        manifest.worlds.push("new-world".to_owned());

        let parsed = parse_manifest(&manifest.id, &serialize_manifest(&manifest)).unwrap();
        assert_eq!(parsed.worlds, vec!["default", "new-world"]);
        // An absent description must stay absent rather than becoming "".
        assert_eq!(parsed.description, None);
    }

    /// Names are user input and go through a hand-rolled quoter.
    #[test]
    fn quotes_and_backslashes_in_names_survive() {
        let manifest = ProjectManifest {
            id: "weird".to_owned(),
            name: r#"He said "hi" \ bye"#.to_owned(),
            description: None,
            plugins: Vec::new(),
            worlds: vec![r#"w"1"#.to_owned()],
            start_world: r#"w"1"#.to_owned(),
        };

        let parsed = parse_manifest(&manifest.id, &serialize_manifest(&manifest)).unwrap();
        assert_eq!(parsed.name, manifest.name);
        assert_eq!(parsed.worlds, manifest.worlds);
        assert_eq!(parsed.start_world, manifest.start_world);
    }
}
