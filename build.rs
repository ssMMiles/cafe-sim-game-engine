/// Build script that generates `assets/asset_manifest.manifest` from the
/// contents of `assets/`.
///
/// One line per asset, pipe-delimited: `kind|name|asset_path`, where `kind`
/// is `mesh`, `texture` or `audio`:
///
/// - **mesh**: each sub-directory of `assets/meshes/` (except `character/`)
///   contributes its first `.gltf`/`.glb` as `meshes/{dir}/{file}#Scene0`.
/// - **texture**: `assets/textures/*.{png,jpg,jpeg}`.
/// - **audio**: `assets/audio/*.{ogg,wav,mp3}` (directory optional).
///
/// Prefabs are deliberately *not* listed: they are user-created at runtime
/// and discovered by the `PrefabLibrary` (directory scan natively, OPFS on
/// web), so a build-time list would only go stale.
///
/// The manifest is consumed at runtime by `AssetManifestLoader` so that asset
/// discovery works on platforms (e.g. Cloudflare Pages / WASM) that do not
/// support filesystem directory listing.
use std::{fs, path::Path, process::Command};

const MANIFEST: &str = "assets/asset_manifest.manifest";
/// Pre-manifest-generalisation location; removed so no stale copy lingers.
const LEGACY_MESH_MANIFEST: &str = "assets/meshes/manifest.manifest";

fn main() {
    for dir in ["assets/meshes", "assets/textures", "assets/audio"] {
        // Cargo treats a watched path that doesn't exist as always stale, so
        // watching the optional `assets/audio` made every build re-run this
        // script and recompile the engine crate. A missing directory is
        // simply not watched; creating it later needs `touch build.rs`.
        if Path::new(dir).is_dir() {
            println!("cargo:rerun-if-changed={dir}");
        }
    }
    stamp_build_commit();

    let mut lines = String::new();
    scan_meshes(&mut lines);
    scan_flat(
        "texture",
        "assets/textures",
        &["png", "jpg", "jpeg"],
        &mut lines,
    );
    scan_flat("audio", "assets/audio", &["ogg", "wav", "mp3"], &mut lines);

    // Leave an unchanged manifest alone, so its mtime only moves with content.
    let unchanged = fs::read_to_string(MANIFEST).is_ok_and(|old| old == lines);
    if !unchanged && let Err(e) = fs::write(MANIFEST, &lines) {
        println!("cargo:warning=Could not write {MANIFEST}: {e}");
        return;
    }
    let _ = fs::remove_file(LEGACY_MESH_MANIFEST);
    let count = lines.lines().count();
    println!("cargo:warning=Generated asset manifest with {count} entries");
}

fn sorted_entries(dir: &Path) -> Vec<fs::DirEntry> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut entries: Vec<_> = entries.flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    entries
}

fn extension_of(path: &Path) -> String {
    path.extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
}

fn scan_meshes(lines: &mut String) {
    let meshes_dir = Path::new("assets/meshes");
    if !meshes_dir.is_dir() {
        println!("cargo:warning=Could not read assets/meshes/ — no meshes in the manifest");
        return;
    }
    for dir_entry in sorted_entries(meshes_dir) {
        let dir_path = dir_entry.path();
        if !dir_path.is_dir() {
            continue;
        }
        let dir_name = dir_path.file_name().unwrap().to_string_lossy().to_string();
        if dir_name == "character" {
            continue;
        }
        // One mesh per directory: the first glTF.
        if let Some(file) = sorted_entries(&dir_path).into_iter().find(|f| {
            let ext = extension_of(&f.path());
            ext == "gltf" || ext == "glb"
        }) {
            let file_name = file.file_name().to_string_lossy().to_string();
            lines.push_str(&format!(
                "mesh|{dir_name}|meshes/{dir_name}/{file_name}#Scene0\n"
            ));
        }
    }
}

/// Every file with one of `exts` directly inside `dir`, named by its stem.
fn scan_flat(kind: &str, dir: &str, exts: &[&str], lines: &mut String) {
    let dir_path = Path::new(dir);
    let Some(dir_name) = dir_path.file_name().and_then(|n| n.to_str()) else {
        return;
    };
    for entry in sorted_entries(dir_path) {
        let path = entry.path();
        if !path.is_file() || !exts.contains(&extension_of(&path).as_str()) {
            continue;
        }
        let file_name = path.file_name().unwrap().to_string_lossy().to_string();
        let stem = path.file_stem().unwrap().to_string_lossy().to_string();
        lines.push_str(&format!("{kind}|{stem}|{dir_name}/{file_name}\n"));
    }
}

/// Expose the commit this binary was built from as `BUILD_COMMIT`, so a perf
/// snapshot records which build produced it (`gfx::system_timing`).
///
/// Several checkouts of this repo write `perf-snapshot-*.md` into their own
/// working directory with the same naming, and parallel branches of the same
/// work have near-identical system names — the hash is the only thing that
/// tells two tables apart with certainty.
///
/// Uncommitted edits are **not** reflected: the value is the HEAD commit, and
/// a dirty tree still reports it. Recording a `-dirty` marker here would be
/// worse than nothing, because this script only re-runs when one of its
/// watched paths changes, so the marker would routinely be stale in the
/// direction that misleads (saying "clean" after an edit).
fn stamp_build_commit() {
    // Re-run when HEAD moves: checkout moves HEAD, commits move the branch
    // ref it names. `git rev-parse --git-path` resolves both correctly in a
    // worktree, where HEAD is private but branch refs live in the shared git
    // dir. Only existing files are watched — cargo treats a missing watched
    // path as always stale, which rebuilt the engine on every `cargo run`.
    // A packed ref has no file of its own; `packed-refs` stands in for it.
    let head = git_path("HEAD");
    if let Some(head) = &head {
        watch_if_exists(head);
    }
    if let Some(reference) = head
        .and_then(|h| fs::read_to_string(h).ok())
        .and_then(|c| c.strip_prefix("ref:").map(|r| r.trim().to_string()))
    {
        match git_path(&reference) {
            Some(ref_file) if Path::new(&ref_file).is_file() => watch_if_exists(&ref_file),
            _ => {
                if let Some(packed) = git_path("packed-refs") {
                    watch_if_exists(&packed);
                }
            }
        }
    }

    let commit = git(&["rev-parse", "--short", "HEAD"]).unwrap_or_else(|| "unknown".into());
    println!("cargo:rustc-env=BUILD_COMMIT={commit}");
}

fn git(args: &[&str]) -> Option<String> {
    Command::new("git")
        .args(args)
        .output()
        .ok()
        .filter(|out| out.status.success())
        .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
}

fn git_path(path: &str) -> Option<String> {
    git(&["rev-parse", "--git-path", path])
}

fn watch_if_exists(path: &str) {
    if Path::new(path).exists() {
        println!("cargo:rerun-if-changed={path}");
    }
}
