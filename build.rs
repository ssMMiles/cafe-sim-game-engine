/// Build script that generates `assets/meshes/manifest.manifest` from the
/// contents of `assets/meshes/`.
///
/// Each non-character sub-directory is scanned for the first `.gltf` or `.glb`
/// file.  The result is written as pipe-delimited lines: `name|asset_path`.
///
/// The manifest is consumed at runtime by `MeshManifestLoader` so that mesh
/// discovery works on platforms (e.g. Cloudflare Pages / WASM) that do not
/// support filesystem directory listing.
use std::{fs, path::Path};

fn main() {
    // Re-run this script whenever the mesh directory tree changes.
    println!("cargo:rerun-if-changed=assets/meshes");

    let meshes_dir = Path::new("assets/meshes");
    let manifest_path = meshes_dir.join("manifest.manifest");

    let Ok(top_entries) = fs::read_dir(meshes_dir) else {
        println!("cargo:warning=Could not read assets/meshes/ — manifest not generated");
        return;
    };

    let mut dirs: Vec<_> = top_entries
        .flatten()
        .filter(|e| e.path().is_dir())
        .collect();
    dirs.sort_by_key(|e| e.file_name());

    let mut lines = String::new();

    for dir_entry in dirs {
        let dir_path = dir_entry.path();
        let dir_name = dir_path.file_name().unwrap().to_string_lossy().to_string();

        if dir_name == "character" {
            continue;
        }

        let Ok(files) = fs::read_dir(&dir_path) else {
            continue;
        };

        let mut files: Vec<_> = files.flatten().collect();
        files.sort_by_key(|e| e.file_name());

        for file_entry in files {
            let file_path = file_entry.path();
            let ext = file_path.extension().and_then(|e| e.to_str()).unwrap_or("");
            if ext != "gltf" && ext != "glb" {
                continue;
            }

            let file_name = file_path.file_name().unwrap().to_string_lossy().to_string();
            let asset_path = format!("meshes/{dir_name}/{file_name}#Scene0");
            lines.push_str(&format!("{dir_name}|{asset_path}\n"));
            break; // one mesh per directory
        }
    }

    fs::write(&manifest_path, &lines).expect("Failed to write mesh manifest");
    let count = lines.lines().count();
    println!("cargo:warning=Generated mesh manifest with {count} entries");
}
