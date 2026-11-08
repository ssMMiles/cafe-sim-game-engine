//! User-level editor settings: `editor_settings.json`.
//!
//! The dock layout, the outline look ([`EdgeStyle`]), the atmosphere
//! ([`AtmosphereStyle`]) and vsync / the frame cap ([`DisplaySettings`]) are
//! *user* preferences, not level or project data, so they live in their own
//! file — the working directory on native, the same-named OPFS key on web
//! (fire-and-forget writes, like project manifests and prefabs). The file is
//! JSON and every top-level entry is parsed on its own, so an outdated or
//! broken entry (an [`EditorTab`] variant renamed, say) falls back to its
//! default without taking the others with it — never a crash.
//!
//! Saves are debounced: once a second, the settings are serialized and
//! written only if they differ from what's already on disk. Exiting to the
//! menu flushes immediately, so at worst a hard window close loses the final
//! second of edits.
//!
//! **Adding a setting** touches three places in this file, all next to each
//! other: a field on [`EditorSettingsFile`], the matching field on
//! [`Persisted`]/[`PersistedMut`]/[`Targets`], and one `entry` line in
//! [`apply`]. The systems never change.

use bevy::{ecs::system::SystemParam, prelude::*};
use serde::{Serialize, de::DeserializeOwned};

use super::{DockLayout, EditorTab};
use crate::game::gfx::atmosphere::AtmosphereStyle;
use crate::game::gfx::frame_pacing::DisplaySettings;
use crate::game::gfx::post_process::EdgeStyle;
use crate::game::world::terrain::lod::TerrainLodSettings;
use egui_dock::DockState;

/// Native path (working directory) and OPFS key.
const SETTINGS_KEY: &str = "editor_settings.json";

/// On-disk shape, written whole. Read back entry by entry ([`apply`]), so
/// older files — which simply lack a newer entry — keep loading.
#[derive(Serialize)]
struct EditorSettingsFile<'a> {
    dock: &'a DockState<EditorTab>,
    edges: &'a EdgeStyle,
    atmosphere: &'a AtmosphereStyle,
    display: &'a DisplaySettings,
    lod: &'a TerrainLodSettings,
}

/// Every persisted setting, read-only: what a save serializes.
#[derive(SystemParam)]
pub struct Persisted<'w> {
    layout: Res<'w, DockLayout>,
    edges: Res<'w, EdgeStyle>,
    atmosphere: Res<'w, AtmosphereStyle>,
    display: Res<'w, DisplaySettings>,
    lod: Res<'w, TerrainLodSettings>,
}

impl Persisted<'_> {
    fn file(&self) -> EditorSettingsFile<'_> {
        EditorSettingsFile {
            dock: &self.layout.0,
            edges: &self.edges,
            atmosphere: &self.atmosphere,
            display: &self.display,
            lod: &self.lod,
        }
    }
}

/// Every persisted setting, writable: where a load lands.
#[derive(SystemParam)]
pub struct PersistedMut<'w> {
    layout: ResMut<'w, DockLayout>,
    edges: ResMut<'w, EdgeStyle>,
    atmosphere: ResMut<'w, AtmosphereStyle>,
    display: ResMut<'w, DisplaySettings>,
    lod: ResMut<'w, TerrainLodSettings>,
}

impl PersistedMut<'_> {
    fn targets(&mut self) -> Targets<'_> {
        Targets {
            layout: &mut self.layout,
            edges: &mut self.edges,
            atmosphere: &mut self.atmosphere,
            display: &mut self.display,
            lod: &mut self.lod,
        }
    }
}

/// Plain references to the settings a load writes — the form [`apply`]
/// takes, so the tests can drive it without a `World`.
struct Targets<'a> {
    layout: &'a mut DockLayout,
    edges: &'a mut EdgeStyle,
    atmosphere: &'a mut AtmosphereStyle,
    display: &'a mut DisplaySettings,
    lod: &'a mut TerrainLodSettings,
}

#[derive(Resource)]
pub struct SettingsPersist {
    /// What's on disk, serialized — the change detector.
    last_saved: Option<String>,
    timer: Timer,
    #[cfg(target_arch = "wasm32")]
    pending: Option<std::sync::Arc<std::sync::Mutex<Option<String>>>>,
}

impl Default for SettingsPersist {
    fn default() -> Self {
        Self {
            last_saved: None,
            timer: Timer::from_seconds(1.0, TimerMode::Repeating),
            #[cfg(target_arch = "wasm32")]
            pending: None,
        }
    }
}

/// One top-level entry, or `None` (with a warning) when it is missing or no
/// longer parses.
fn entry<T: DeserializeOwned>(file: &serde_json::Value, key: &str) -> Option<T> {
    let value = file.get(key)?.clone();
    serde_json::from_value(value)
        .inspect_err(|e| warn!("{SETTINGS_KEY}: `{key}` didn't parse ({e}); using the default"))
        .ok()
}

/// Apply whatever the file holds. Entries that are missing or broken leave
/// the current (default) value alone.
fn apply(text: &str, targets: Targets, state: &mut SettingsPersist) {
    let file: serde_json::Value = match serde_json::from_str(text) {
        Ok(file) => file,
        Err(e) => {
            warn!("{SETTINGS_KEY} didn't parse ({e}); using the defaults");
            return;
        }
    };
    if let Some(dock) = entry(&file, "dock") {
        targets.layout.0 = dock;
    }
    if let Some(style) = entry(&file, "edges") {
        *targets.edges = style;
    }
    if let Some(style) = entry(&file, "atmosphere") {
        *targets.atmosphere = style;
    }
    if let Some(display) = entry(&file, "display") {
        *targets.display = display;
    }
    if let Some(lod) = entry::<TerrainLodSettings>(&file, "lod") {
        *targets.lod = lod.sanitized();
    }
    state.last_saved = Some(text.to_owned());
}

/// Serialize the current settings; write them when they differ from the disk
/// copy.
fn persist(file: &EditorSettingsFile, state: &mut SettingsPersist) {
    let text = match serde_json::to_string_pretty(file) {
        Ok(text) => text,
        Err(e) => {
            warn!("Failed to serialize the editor settings: {e}");
            return;
        }
    };
    if state.last_saved.as_deref() == Some(text.as_str()) {
        return;
    }
    #[cfg(not(target_arch = "wasm32"))]
    if let Err(e) = std::fs::write(SETTINGS_KEY, &text) {
        warn!("Failed to write {SETTINGS_KEY}: {e}");
        return;
    }
    #[cfg(target_arch = "wasm32")]
    {
        let bytes = text.clone().into_bytes();
        wasm_bindgen_futures::spawn_local(async move {
            let s = crate::storage::wasm::WasmStorage;
            if let Err(e) = s.save(SETTINGS_KEY, &bytes).await {
                log::error!("OPFS write of {SETTINGS_KEY} failed: {e}");
            }
        });
    }
    state.last_saved = Some(text);
}

/// Debounced save while the editor is in front (the only place any of these
/// settings is edited).
pub fn save_settings(time: Res<Time>, settings: Persisted, mut state: ResMut<SettingsPersist>) {
    if state.timer.tick(time.delta()).just_finished() {
        persist(&settings.file(), &mut state);
    }
}

/// Flush on the way out to the menu, so quitting right after an edit keeps it.
pub fn save_settings_now(settings: Persisted, mut state: ResMut<SettingsPersist>) {
    persist(&settings.file(), &mut state);
}

/// Native: read the file once at startup, before the first dock pass.
#[cfg(not(target_arch = "wasm32"))]
pub fn load_settings(mut settings: PersistedMut, mut state: ResMut<SettingsPersist>) {
    let Ok(text) = std::fs::read_to_string(SETTINGS_KEY) else {
        return; // first run
    };
    apply(&text, settings.targets(), &mut state);
}

/// Web: kick off the OPFS read; [`poll_settings`] applies it when it lands
/// (a frame or two in — any edit made before then loses, which is the same
/// trade every OPFS load here makes).
#[cfg(target_arch = "wasm32")]
pub fn load_settings(mut state: ResMut<SettingsPersist>) {
    use std::sync::{Arc, Mutex};
    let slot = Arc::new(Mutex::new(None));
    state.pending = Some(slot.clone());
    wasm_bindgen_futures::spawn_local(async move {
        let s = crate::storage::wasm::WasmStorage;
        match s.load(SETTINGS_KEY).await {
            Ok(Some(bytes)) => match String::from_utf8(bytes) {
                Ok(text) => *slot.lock().unwrap() = Some(text),
                Err(e) => log::warn!("{SETTINGS_KEY} isn't UTF-8: {e}"),
            },
            Ok(None) => {} // first run
            Err(e) => log::warn!("OPFS read of {SETTINGS_KEY} failed: {e}"),
        }
    });
}

#[cfg(target_arch = "wasm32")]
pub fn poll_settings(mut settings: PersistedMut, mut state: ResMut<SettingsPersist>) {
    let Some(slot) = state.pending.as_ref() else {
        return;
    };
    let Some(text) = slot.lock().unwrap().take() else {
        return;
    };
    state.pending = None;
    apply(&text, settings.targets(), &mut state);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The default layout is transcribed from a hand-arranged
    /// `editor_settings.json`. The stored `fraction` of a split node is the
    /// *left/top* child's share (egui_dock renders `midpoint = min + size *
    /// fraction`), which `split_left` et al. store verbatim — this test pins
    /// both the semantics and the curated numbers, so a regression back to
    /// "old node keeps the fraction" thinking (which once gave the outliner
    /// 82% of the window) fails loudly.
    #[test]
    fn the_default_layout_matches_the_curated_one() {
        let json = serde_json::to_value(&EditorSettingsFile {
            dock: &DockLayout::default().0,
            edges: &EdgeStyle::default(),
            atmosphere: &AtmosphereStyle::default(),
            display: &DisplaySettings::default(),
            lod: &TerrainLodSettings::default(),
        })
        .unwrap();
        let nodes = &json["dock"]["surfaces"][0]["Main"]["nodes"];

        let fraction = |i: usize, kind: &str| {
            nodes[i][kind]["fraction"]
                .as_f64()
                .unwrap_or_else(|| panic!("node {i} is not a {kind} split"))
        };
        let tabs = |i: usize| nodes[i]["Leaf"]["tabs"].clone();

        // Left strip: Outliner over History at 17.8% of the window.
        assert!((fraction(0, "Horizontal") - 0.178).abs() < 1e-6);
        assert_eq!(tabs(1), serde_json::json!(["Outliner", "History"]));
        // The viewport column keeps 81.1% against the inspector column.
        assert!((fraction(2, "Horizontal") - 0.811).abs() < 1e-6);
        // Viewport over Assets; Inspector over Tool.
        assert!((fraction(5, "Vertical") - 0.725).abs() < 1e-6);
        assert!((fraction(6, "Vertical") - 0.528).abs() < 1e-6);
        assert_eq!(tabs(11), serde_json::json!(["Viewport"]));
        assert_eq!(tabs(12), serde_json::json!(["Assets"]));
        assert_eq!(tabs(13), serde_json::json!(["Inspector"]));
        assert_eq!(tabs(14), serde_json::json!(["Tool"]));
    }

    /// Everything a load produces, compared as one value.
    type Loaded = (
        usize,
        EdgeStyle,
        AtmosphereStyle,
        DisplaySettings,
        TerrainLodSettings,
    );

    fn load(value: &serde_json::Value) -> Loaded {
        let mut layout = DockLayout(DockState::new(vec![EditorTab::Viewport]));
        let mut edges = EdgeStyle::default();
        let mut atmosphere = AtmosphereStyle::default();
        let mut display = DisplaySettings::default();
        let mut lod = TerrainLodSettings::default();
        apply(
            &value.to_string(),
            Targets {
                layout: &mut layout,
                edges: &mut edges,
                atmosphere: &mut atmosphere,
                display: &mut display,
                lod: &mut lod,
            },
            &mut SettingsPersist::default(),
        );
        (
            layout.0.iter_all_tabs().count(),
            edges,
            atmosphere,
            display,
            lod,
        )
    }

    /// Files written before a setting existed hold only the older entries;
    /// they must keep those. A broken entry must not take the others down.
    #[test]
    fn entries_load_independently() {
        let edges = EdgeStyle {
            crease_angle: 12.0,
            ..default()
        };
        let air = AtmosphereStyle {
            fog_density: 0.042,
            ..default()
        };
        let display = DisplaySettings {
            vsync: true,
            frame_cap: 90,
        };
        let lod = TerrainLodSettings {
            distances: [240.0, 480.0, 960.0],
        };
        let full = serde_json::to_string(&EditorSettingsFile {
            dock: &DockLayout::default().0,
            edges: &edges,
            atmosphere: &air,
            display: &display,
            lod: &lod,
        })
        .unwrap();
        let mut value: serde_json::Value = serde_json::from_str(&full).unwrap();
        // A never-drawn DockState holds infinite rects, whose `x`/`y` JSON
        // writes as `null` and can't read back; every real save comes from a
        // drawn layout, whose rects are finite. Make the fixture look like one.
        fn finite(v: &mut serde_json::Value) {
            match v {
                serde_json::Value::Array(items) => items.iter_mut().for_each(finite),
                serde_json::Value::Object(map) => {
                    for (key, v) in map.iter_mut() {
                        if v.is_null() && (key == "x" || key == "y") {
                            *v = serde_json::json!(0.0);
                        }
                        finite(v);
                    }
                }
                _ => {}
            }
        }
        finite(&mut value["dock"]);
        let default_tabs = DockLayout::default().0.iter_all_tabs().count();

        assert_eq!(load(&value), (default_tabs, edges, air, display, lod));

        // An older file holds only `dock`; the rest fall back to defaults.
        let dock_only = serde_json::json!({ "dock": value["dock"].clone() });
        assert_eq!(
            load(&dock_only),
            (
                default_tabs,
                EdgeStyle::default(),
                AtmosphereStyle::default(),
                DisplaySettings::default(),
                TerrainLodSettings::default()
            )
        );

        // Broken dock: the other entries still load, the layout stays put.
        value["dock"] = serde_json::json!("not a layout");
        assert_eq!(load(&value), (1, edges, air, display, lod));

        // A field added to a style later is simply absent: the rest load.
        let partial = serde_json::json!({
            "edges": { "crease_angle": 12.0 },
            "atmosphere": { "fog_density": 0.042 },
            "display": { "vsync": true, "frame_cap": 90 },
            // Out of order on disk (a hand edit): loads sanitized.
            "lod": { "distances": [960.0, 480.0, 240.0] },
        });
        let (_, e, a, d, l) = load(&partial);
        assert_eq!((e, a, d), (edges, air, display));
        assert_eq!(l.distances, [960.0, 960.0, 960.0]);
    }
}
