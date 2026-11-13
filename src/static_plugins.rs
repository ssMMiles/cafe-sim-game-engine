//! Plugins compiled into this binary (`docs/STATIC_PLUGINS_PLAN.md`).
//!
//! The web build always links every plugin (wasm has no dylibs); natively only
//! the `static-plugins` feature does. Otherwise the list is empty and the host
//! loads dylibs. A project's manifest still decides which of these load.

use crate::script_host::StaticPluginCtor;

/// `(plugin id, constructor)` for every plugin linked into this build. Keyed
/// by each crate's own `PLUGIN_NAME`, which is also the id its `ScriptOwned`
/// entities carry.
#[cfg(any(feature = "static-plugins", target_arch = "wasm32"))]
pub fn all() -> Vec<(&'static str, StaticPluginCtor)> {
    vec![
        (cafe_sim::PLUGIN_NAME, cafe_sim::plugin),
        (example::PLUGIN_NAME, example::plugin),
        (example2::PLUGIN_NAME, example2::plugin),
    ]
}

#[cfg(not(any(feature = "static-plugins", target_arch = "wasm32")))]
pub fn all() -> Vec<(&'static str, StaticPluginCtor)> {
    Vec::new()
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use std::collections::BTreeSet;
    use std::path::Path;

    /// Plugin ids = the crate directories under `plugins/`.
    fn plugin_dirs() -> BTreeSet<String> {
        std::fs::read_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("plugins"))
            .expect("plugins/ exists")
            .filter_map(|e| e.ok())
            .filter(|e| e.path().join("Cargo.toml").is_file())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect()
    }

    /// The three places a plugin must be listed — the `static-plugins`
    /// feature, the optional dependencies and the wasm dependency table — all
    /// name exactly the crates under `plugins/`. Runs in the default build,
    /// where `all()` itself is empty, so a new plugin can't be forgotten.
    #[test]
    fn every_plugin_is_wired_for_static_builds() {
        let manifest: toml::Table =
            std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml"))
                .unwrap()
                .parse()
                .unwrap();
        let expected = plugin_dirs();

        let feature: BTreeSet<String> = manifest["features"]["static-plugins"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().trim_start_matches("dep:").to_owned())
            .collect();
        assert_eq!(feature, expected, "static-plugins feature");

        let deps_naming = |table: &toml::Table| -> BTreeSet<String> {
            table
                .iter()
                .filter(|(_, dep)| {
                    dep.get("path")
                        .and_then(|p| p.as_str())
                        .is_some_and(|p| p.starts_with("plugins/"))
                })
                .map(|(name, _)| name.clone())
                .collect()
        };
        let native = manifest["dependencies"].as_table().unwrap();
        assert_eq!(deps_naming(native), expected, "optional [dependencies]");
        let wasm = manifest["target"]["cfg(target_arch = \"wasm32\")"]["dependencies"]
            .as_table()
            .unwrap();
        assert_eq!(deps_naming(wasm), expected, "wasm [dependencies]");
    }

    /// With the plugins actually linked, the list covers each one once.
    #[cfg(feature = "static-plugins")]
    #[test]
    fn static_list_covers_every_plugin() {
        let listed: BTreeSet<String> = super::all().iter().map(|(id, _)| id.to_string()).collect();
        assert_eq!(listed.len(), super::all().len(), "duplicate id");
        assert_eq!(listed, plugin_dirs());
    }
}
