//! File ▸ Export world as zip — a *world bundle* for sharing a level.
//!
//! A bundle is a zip whose entry names are the repo-relative paths the
//! engine already uses for level data: `worlds/{id}/…`, the project
//! manifest under `projects/{id}/project.toml`, and the prefab definitions
//! the world's instance roots reference under `assets/prefabs/`. Those are
//! exactly the OPFS keys the web build reads, so `index.html` can unpack a
//! bundle straight into the browser's storage with no engine involvement —
//! see `docs/WORLD_SHARING.md`.
//!
//! Only *current-format* lightmaps and probe grids are packed; the stale
//! `.v{n}` files earlier bakes left behind would be skipped by the loader
//! anyway (the format version is in the file name) and are the bulk of a
//! long-lived world directory.
//!
//! The export reads what is on disk: save first. It runs on the compute
//! pool and reports through a status-bar notice. Native only — the browser
//! has no file system to export to — so on wasm the request just posts a
//! notice saying so.

use bevy::prelude::*;

#[cfg(not(target_arch = "wasm32"))]
use std::path::{Path, PathBuf};

use crate::game::world::terrain::storage::TerrainWorld;
use crate::projects::{ActiveProject, ProjectManifest};

/// How long the completion notice stays in the status bar, in seconds.
const NOTICE_SECONDS: f64 = 8.0;

/// What the export task needs, cloned out of the ECS so the task owns it.
#[derive(Clone, Debug)]
pub struct BundleRequest {
    pub world_id: String,
    /// The world directory on disk (`worlds/{id}` natively).
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    pub world_path: std::path::PathBuf,
    /// Written into the bundle with only this world listed, so importing it
    /// creates (or extends) the same project on the other side.
    pub project: Option<ProjectManifest>,
}

#[derive(Debug)]
#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
pub struct ExportReport {
    pub archive: std::path::PathBuf,
    pub files: usize,
    pub bytes: u64,
    /// Lightmaps / probe grids of an older format version, left out.
    pub skipped_stale: usize,
    /// Prefabs the world references that `assets/prefabs/` does not have.
    pub missing_prefabs: Vec<String>,
}

enum ExportState {
    Idle,
    #[cfg(not(target_arch = "wasm32"))]
    Running(bevy::tasks::Task<Result<ExportReport, String>>),
    /// Status-bar text, whether it is an error, and when it expires.
    Notice {
        text: String,
        error: bool,
        until: f64,
    },
}

#[derive(Resource)]
pub struct WorldExport {
    state: ExportState,
}

impl Default for WorldExport {
    fn default() -> Self {
        Self {
            state: ExportState::Idle,
        }
    }
}

impl WorldExport {
    pub fn running(&self) -> bool {
        #[cfg(not(target_arch = "wasm32"))]
        {
            matches!(self.state, ExportState::Running(_))
        }
        #[cfg(target_arch = "wasm32")]
        {
            false
        }
    }

    /// `(text, is_error)` while a completion notice is up.
    pub fn notice(&self) -> Option<(&str, bool)> {
        match &self.state {
            ExportState::Notice { text, error, .. } => Some((text.as_str(), *error)),
            _ => None,
        }
    }

    fn set_notice(&mut self, text: String, error: bool, now: f64) {
        if error {
            warn!("{text}");
        } else {
            info!("{text}");
        }
        self.state = ExportState::Notice {
            text,
            error,
            until: now + NOTICE_SECONDS,
        };
    }

    /// Start an export of the loaded world. Refused (with a notice) while one
    /// is already running or when no world is loaded.
    pub fn request(
        &mut self,
        terrain_world: Option<&TerrainWorld>,
        active: Option<&ActiveProject>,
        now: f64,
    ) {
        if self.running() {
            self.set_notice("Export already in progress".into(), true, now);
            return;
        }
        let Some(world) = terrain_world else {
            self.set_notice("Export: no world is loaded".into(), true, now);
            return;
        };
        let request = BundleRequest {
            world_id: world.metadata.world_id.clone(),
            world_path: world.world_path.clone(),
            project: active.map(|a| a.0.clone()),
        };

        #[cfg(not(target_arch = "wasm32"))]
        {
            info!("Exporting world '{}'…", request.world_id);
            let task = bevy::tasks::AsyncComputeTaskPool::get()
                .spawn(async move { build_bundle(&request, Path::new(".")) });
            self.state = ExportState::Running(task);
        }
        #[cfg(target_arch = "wasm32")]
        {
            let _ = request;
            self.set_notice(
                "Export is only available in the native editor".into(),
                true,
                now,
            );
        }
    }
}

/// Finish a completed export (notice), expire old notices.
pub fn poll_world_export(mut export: ResMut<WorldExport>, time: Res<Time>) {
    let now = time.elapsed_secs_f64();
    match &mut export.state {
        ExportState::Idle => {}
        ExportState::Notice { until, .. } => {
            if now > *until {
                export.state = ExportState::Idle;
            }
        }
        #[cfg(not(target_arch = "wasm32"))]
        ExportState::Running(task) => {
            let Some(result) = bevy::tasks::futures::check_ready(task) else {
                return;
            };
            match result {
                Ok(report) => {
                    let mut text = format!(
                        "Exported {} — {} files, {}",
                        report.archive.display(),
                        report.files,
                        human_bytes(report.bytes)
                    );
                    if report.skipped_stale > 0 {
                        text.push_str(&format!(
                            " (skipped {} older-format bake file(s))",
                            report.skipped_stale
                        ));
                    }
                    if !report.missing_prefabs.is_empty() {
                        text.push_str(&format!(
                            " (missing prefab(s): {})",
                            report.missing_prefabs.join(", ")
                        ));
                    }
                    export.set_notice(text, false, now);
                }
                Err(e) => export.set_notice(format!("Export failed: {e}"), true, now),
            }
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn human_bytes(bytes: u64) -> String {
    const MB: f64 = 1024.0 * 1024.0;
    if bytes as f64 >= MB {
        format!("{:.1} MB", bytes as f64 / MB)
    } else {
        format!("{:.0} KB", bytes as f64 / 1024.0)
    }
}

// ============================================================================
// Bundle building (native)
// ============================================================================

/// Which world files belong in a bundle.
#[derive(Debug, PartialEq, Eq)]
enum WorldFile {
    Keep,
    /// A lightmap or probe grid of another format version.
    Stale,
    /// Anything else lying around the directory (`.DS_Store`, scratch).
    Ignore,
}

fn classify_world_file(name: &str) -> WorldFile {
    use crate::game::world::baking::lightmap::LIGHTMAP_FORMAT_VERSION;
    use crate::game::world::baking::probes::PROBE_FORMAT_VERSION;

    if name == "world.meta" || name.ends_with(".terrain") || name.ends_with(".objects") {
        return WorldFile::Keep;
    }
    let current_lightmap = format!("_lightmap.v{LIGHTMAP_FORMAT_VERSION}.exr");
    let current_probes = format!("_probes.v{PROBE_FORMAT_VERSION}.bin");
    if name.ends_with(&current_lightmap) || name.ends_with(&current_probes) {
        return WorldFile::Keep;
    }
    let versioned = |marker: &str, ext: &str| {
        name.rfind(marker)
            .map(|i| &name[i + marker.len()..])
            .and_then(|rest| rest.strip_suffix(ext))
            .is_some_and(|v| !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()))
    };
    if versioned("_lightmap.v", ".exr") || versioned("_probes.v", ".bin") {
        WorldFile::Stale
    } else {
        WorldFile::Ignore
    }
}

/// Write `worlds/{id}.zip` next to the world directory and describe what
/// went in. `root` is where `assets/prefabs/` lives (the cwd in the app;
/// a temp dir in tests).
#[cfg(not(target_arch = "wasm32"))]
pub fn build_bundle(request: &BundleRequest, root: &Path) -> Result<ExportReport, String> {
    use crate::game::world::objects::parse_chunk_objects_from_bytes;
    use crate::game::world::prefabs::prefab_key;
    use std::collections::BTreeSet;
    use std::io::Write;
    use zip::write::SimpleFileOptions;

    let world_dir = &request.world_path;
    if !world_dir.join("world.meta").is_file() {
        return Err(format!(
            "{} has no world.meta — save the world first",
            world_dir.display()
        ));
    }

    // (entry name, source path) — world files first, then everything the
    // world needs from outside its own directory.
    let mut entries: Vec<(String, PathBuf)> = Vec::new();
    let mut skipped_stale = 0;
    let world_prefix = format!("worlds/{}", request.world_id);

    for sub in ["", "chunks", "objects"] {
        let dir = if sub.is_empty() {
            world_dir.clone()
        } else {
            world_dir.join(sub)
        };
        let listing = match std::fs::read_dir(&dir) {
            Ok(l) => l,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(format!("reading {}: {e}", dir.display())),
        };
        for entry in listing {
            let entry = entry.map_err(|e| e.to_string())?;
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            match classify_world_file(name) {
                WorldFile::Keep => {
                    let entry_name = if sub.is_empty() {
                        format!("{world_prefix}/{name}")
                    } else {
                        format!("{world_prefix}/{sub}/{name}")
                    };
                    entries.push((entry_name, path));
                }
                WorldFile::Stale => skipped_stale += 1,
                WorldFile::Ignore => {}
            }
        }
    }
    entries.sort();

    // Prefabs referenced by instance roots in any chunk.
    let mut prefab_names = BTreeSet::new();
    for (name, path) in &entries {
        if !name.ends_with(".objects") {
            continue;
        }
        let bytes = std::fs::read(path).map_err(|e| format!("reading {}: {e}", path.display()))?;
        let file = parse_chunk_objects_from_bytes(&bytes)
            .map_err(|e| format!("parsing {}: {e}", path.display()))?;
        prefab_names.extend(
            file.entities
                .iter()
                .filter_map(|r| r.prefab.as_ref())
                .map(|p| p.name.clone()),
        );
    }
    let mut missing_prefabs = Vec::new();
    for name in prefab_names {
        let key = prefab_key(&name);
        let path = root.join(&key);
        if path.is_file() {
            entries.push((key, path));
        } else {
            missing_prefabs.push(name);
        }
    }

    // The project manifest, narrowed to this world.
    let manifest_entry = request.project.as_ref().map(|p| {
        let narrowed = ProjectManifest {
            worlds: vec![request.world_id.clone()],
            start_world: request.world_id.clone(),
            ..p.clone()
        };
        (
            format!("projects/{}/project.toml", p.id),
            crate::projects::serialize_manifest(&narrowed),
        )
    });

    let archive = world_dir.with_extension("zip");
    let tmp = world_dir.with_extension("zip.tmp");
    let file =
        std::fs::File::create(&tmp).map_err(|e| format!("creating {}: {e}", tmp.display()))?;
    let mut zip = zip::ZipWriter::new(std::io::BufWriter::new(file));
    let options = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);

    let mut bytes = 0u64;
    let mut files = 0usize;
    let zip_err = |e: zip::result::ZipError| e.to_string();
    for (name, path) in &entries {
        let data = std::fs::read(path).map_err(|e| format!("reading {}: {e}", path.display()))?;
        zip.start_file(name, options).map_err(zip_err)?;
        zip.write_all(&data).map_err(|e| e.to_string())?;
        bytes += data.len() as u64;
        files += 1;
    }
    if let Some((name, body)) = manifest_entry {
        zip.start_file(name, options).map_err(zip_err)?;
        zip.write_all(body.as_bytes()).map_err(|e| e.to_string())?;
        bytes += body.len() as u64;
        files += 1;
    }
    zip.finish()
        .map_err(zip_err)?
        .flush()
        .map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, &archive)
        .map_err(|e| format!("moving {} into place: {e}", tmp.display()))?;

    Ok(ExportReport {
        archive,
        files,
        bytes,
        skipped_stale,
        missing_prefabs,
    })
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;
    use crate::game::world::baking::lightmap::LIGHTMAP_FORMAT_VERSION;
    use crate::game::world::baking::probes::PROBE_FORMAT_VERSION;
    use crate::game::world::objects::{
        CHUNK_OBJECTS_VERSION, ChunkObjectsFile, PlacedEntityRecord, PlacedKind,
        serialize_chunk_objects,
    };
    use crate::game::world::prefabs::PrefabInstanceData;
    use std::io::Read;

    #[test]
    fn classifies_world_files() {
        assert_eq!(classify_world_file("world.meta"), WorldFile::Keep);
        assert_eq!(classify_world_file("chunk_-1_2.terrain"), WorldFile::Keep);
        assert_eq!(classify_world_file("chunk_0_0.objects"), WorldFile::Keep);
        let cur = format!("chunk_0_0_lightmap.v{LIGHTMAP_FORMAT_VERSION}.exr");
        assert_eq!(classify_world_file(&cur), WorldFile::Keep);
        let cur = format!("obj_7_1_lightmap.v{LIGHTMAP_FORMAT_VERSION}.exr");
        assert_eq!(classify_world_file(&cur), WorldFile::Keep);
        let cur = format!("chunk_0_0_probes.v{PROBE_FORMAT_VERSION}.bin");
        assert_eq!(classify_world_file(&cur), WorldFile::Keep);
        let old = format!("chunk_0_0_lightmap.v{}.exr", LIGHTMAP_FORMAT_VERSION - 1);
        assert_eq!(classify_world_file(&old), WorldFile::Stale);
        let old = format!("chunk_0_0_probes.v{}.bin", PROBE_FORMAT_VERSION + 1);
        assert_eq!(classify_world_file(&old), WorldFile::Stale);
        assert_eq!(classify_world_file(".DS_Store"), WorldFile::Ignore);
        assert_eq!(classify_world_file("notes.txt"), WorldFile::Ignore);
    }

    fn write(path: &Path, bytes: &[u8]) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }

    #[test]
    fn bundle_holds_current_world_files_prefabs_and_a_narrowed_manifest() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path();
        let world = root.join("worlds").join("hill");
        write(&world.join("world.meta"), b"meta");
        write(&world.join(".DS_Store"), b"junk");
        write(&world.join("chunks/chunk_0_0.terrain"), b"terrain");
        write(
            &world.join(format!(
                "chunks/chunk_0_0_lightmap.v{LIGHTMAP_FORMAT_VERSION}.exr"
            )),
            b"lm",
        );
        write(&world.join("chunks/chunk_0_0_lightmap.v3.exr"), b"old");
        write(
            &world.join(format!(
                "chunks/chunk_0_0_probes.v{PROBE_FORMAT_VERSION}.bin"
            )),
            b"pr",
        );
        write(&world.join("chunks/chunk_0_0_probes.v2.bin"), b"old");
        write(
            &world.join(format!(
                "objects/obj_1_0_lightmap.v{LIGHTMAP_FORMAT_VERSION}.exr"
            )),
            b"olm",
        );
        write(&world.join("objects/obj_1_0_lightmap.v1.exr"), b"old");

        let mut root_record = PlacedEntityRecord::new(1, PlacedKind::Group, &Transform::IDENTITY);
        root_record.prefab = Some(PrefabInstanceData {
            name: "bench".into(),
            member_ids: vec![],
            overrides: vec![],
        });
        let mut other = PlacedEntityRecord::new(2, PlacedKind::Group, &Transform::IDENTITY);
        other.prefab = Some(PrefabInstanceData {
            name: "lost".into(),
            member_ids: vec![],
            overrides: vec![],
        });
        let objects = ChunkObjectsFile {
            version: CHUNK_OBJECTS_VERSION,
            chunk_x: 0,
            chunk_y: 0,
            entities: vec![root_record, other],
        };
        write(
            &world.join("chunks/chunk_0_0.objects"),
            &serialize_chunk_objects(&objects).unwrap(),
        );
        write(&root.join("assets/prefabs/bench.prefab"), b"prefab");

        let request = BundleRequest {
            world_id: "hill".into(),
            world_path: world.clone(),
            project: Some(ProjectManifest {
                id: "demo".into(),
                name: "Demo".into(),
                description: None,
                plugins: vec!["cafe-sim".into()],
                worlds: vec!["other".into(), "hill".into()],
                start_world: "other".into(),
            }),
        };
        let report = build_bundle(&request, root).unwrap();
        assert_eq!(report.archive, root.join("worlds").join("hill.zip"));
        assert_eq!(report.skipped_stale, 3);
        assert_eq!(report.missing_prefabs, vec!["lost".to_string()]);
        assert!(!root.join("worlds").join("hill.zip.tmp").exists());

        let file = std::fs::File::open(&report.archive).unwrap();
        let mut zip = zip::ZipArchive::new(file).unwrap();
        let mut names: Vec<String> = zip.file_names().map(|n| n.unwrap().into_owned()).collect();
        names.sort();
        let mut expected = vec![
            "assets/prefabs/bench.prefab".to_string(),
            "projects/demo/project.toml".into(),
            "worlds/hill/chunks/chunk_0_0.objects".into(),
            format!("worlds/hill/chunks/chunk_0_0_lightmap.v{LIGHTMAP_FORMAT_VERSION}.exr"),
            format!("worlds/hill/chunks/chunk_0_0_probes.v{PROBE_FORMAT_VERSION}.bin"),
            "worlds/hill/chunks/chunk_0_0.terrain".into(),
            format!("worlds/hill/objects/obj_1_0_lightmap.v{LIGHTMAP_FORMAT_VERSION}.exr"),
            "worlds/hill/world.meta".into(),
        ];
        expected.sort();
        assert_eq!(names, expected);
        assert_eq!(report.files, names.len());

        let mut manifest = String::new();
        zip.by_name("projects/demo/project.toml")
            .unwrap()
            .read_to_string(&mut manifest)
            .unwrap();
        assert!(manifest.contains("worlds = [\"hill\"]"), "{manifest}");
        assert!(manifest.contains("start_world = \"hill\""), "{manifest}");
        assert!(manifest.contains("plugins = [\"cafe-sim\"]"), "{manifest}");

        let mut terrain = Vec::new();
        zip.by_name("worlds/hill/chunks/chunk_0_0.terrain")
            .unwrap()
            .read_to_end(&mut terrain)
            .unwrap();
        assert_eq!(terrain, b"terrain");
    }

    /// Dev tool: export a real world from `./worlds/` into a temp dir (via a
    /// symlink, so nothing is copied or overwritten) and print the archive
    /// path — feed it to `node tools/bundle-check.mjs` to prove the page's
    /// reader agrees with this writer. `WORLD=cafe cargo test --release
    /// export_world_fixture -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn export_world_fixture() {
        let world_id = std::env::var("WORLD").unwrap_or_else(|_| "cafe".into());
        let real = std::env::current_dir()
            .unwrap()
            .join("worlds")
            .join(&world_id);
        assert!(
            real.join("world.meta").is_file(),
            "{} is not a saved world",
            real.display()
        );
        let dir = std::env::temp_dir().join("engine-export-fixture");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("worlds")).unwrap();
        let link = dir.join("worlds").join(&world_id);
        #[cfg(unix)]
        std::os::unix::fs::symlink(&real, &link).unwrap();
        #[cfg(windows)]
        std::os::windows::fs::symlink_dir(&real, &link).unwrap();
        let request = BundleRequest {
            world_id: world_id.clone(),
            world_path: link,
            project: Some(ProjectManifest {
                id: "fixture".into(),
                name: "Fixture".into(),
                description: None,
                plugins: vec![],
                worlds: vec![world_id.clone()],
                start_world: world_id,
            }),
        };
        let report = build_bundle(&request, &std::env::current_dir().unwrap()).unwrap();
        println!(
            "{} — {} files, {} bytes, {} stale skipped, missing prefabs {:?}",
            report.archive.display(),
            report.files,
            report.bytes,
            report.skipped_stale,
            report.missing_prefabs
        );
    }

    #[test]
    fn bundle_refuses_an_unsaved_world() {
        let root = tempfile::tempdir().unwrap();
        let request = BundleRequest {
            world_id: "fresh".into(),
            world_path: root.path().join("worlds/fresh"),
            project: None,
        };
        let err = build_bundle(&request, root.path()).unwrap_err();
        assert!(err.contains("world.meta"), "{err}");
    }
}
