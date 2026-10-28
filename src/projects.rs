//! Project discovery and active-project tracking.
//!
//! A "project" is a directory under `<workspace>/projects/` containing a
//! `project.toml` manifest. The manifest declares which plugins and worlds
//! belong to the project; plugin sources and world saves still live in the
//! top-level `plugins/` and `worlds/` directories and are referenced by id.

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

#[cfg(target_arch = "wasm32")]
fn populate_registry(mut registry: ResMut<ProjectRegistry>) {
    // Filesystem discovery isn't available on wasm. Seed a single fallback
    // project so the menu has something to show.
    registry.projects = vec![ProjectManifest {
        id: "example-game".to_owned(),
        name: "Example Game".to_owned(),
        description: None,
        plugins: Vec::new(),
        worlds: vec!["default".to_owned()],
        start_world: "default".to_owned(),
    }];
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

/// Write `manifest` to `projects/<manifest.id>/project.toml`, creating the
/// directory if it doesn't exist. Used both for new projects and for
/// updating an existing manifest in-place (e.g. when adding a world).
#[cfg(not(target_arch = "wasm32"))]
pub fn write_manifest(manifest: &ProjectManifest) -> std::io::Result<()> {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let project_dir = Path::new(manifest_dir).join("projects").join(&manifest.id);
    std::fs::create_dir_all(&project_dir)?;
    std::fs::write(
        project_dir.join("project.toml"),
        serialize_manifest(manifest),
    )?;
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
