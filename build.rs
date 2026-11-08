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
        println!("cargo:rerun-if-changed={dir}");
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

    if let Err(e) = fs::write(MANIFEST, &lines) {
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
    // Re-run when HEAD moves (checkout, and commits via the ref it names).
    // `.git` is a *file* in a worktree, pointing at the real git dir.
    let git_dir = match fs::read_to_string(".git") {
        Ok(text) => text
            .strip_prefix("gitdir:")
            .map(|p| p.trim().to_string())
            .unwrap_or_else(|| ".git".into()),
        Err(_) => ".git".into(),
    };
    let head = Path::new(&git_dir).join("HEAD");
    if let Ok(contents) = fs::read_to_string(&head) {
        println!("cargo:rerun-if-changed={}", head.display());
        if let Some(reference) = contents.strip_prefix("ref:") {
            // Commits on this branch move the ref file, not HEAD itself.
            // (A packed ref has no file; then the hash can go stale until
            // something else re-runs this script.)
            println!(
                "cargo:rerun-if-changed={}",
                Path::new(&git_dir).join(reference.trim()).display()
            );
        }
    }

    let commit = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .filter(|out| out.status.success())
        .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string())
        .filter(|hash| !hash.is_empty())
        .unwrap_or_else(|| "unknown".into());
    println!("cargo:rustc-env=BUILD_COMMIT={commit}");
}
