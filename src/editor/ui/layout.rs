//! Dock layout persistence.
//!
//! The arrangement of the editor's tabs is a *user* preference, not level or
//! project data, so it lives in its own `editor_settings.json` — the working
//! directory on native, the same-named OPFS key on web (fire-and-forget
//! writes, like project manifests and prefabs). The file is JSON so a broken
//! or outdated layout (an [`EditorTab`] variant renamed, say) simply fails to
//! parse and the default layout takes over — never a crash.
//!
//! Saves are debounced: once a second, the current [`DockLayout`] is
//! serialized and written only if it differs from what's already on disk.
//! Exiting to the menu flushes immediately, so at worst a hard window close
//! loses the final second of tab dragging.

use bevy::prelude::*;
use serde::{Deserialize, Serialize};

use super::{DockLayout, EditorTab};
use egui_dock::DockState;

/// Native path (working directory) and OPFS key.
const SETTINGS_KEY: &str = "editor_settings.json";

/// On-disk shape. Dock layout is the only entry today; future user-level
/// editor settings join as `#[serde(default)]` fields so old files keep
/// parsing.
#[derive(Serialize, Deserialize)]
struct EditorSettingsFile {
    dock: DockState<EditorTab>,
}

#[derive(Resource)]
pub struct LayoutPersist {
    /// What's on disk, serialized — the change detector.
    last_saved: Option<String>,
    timer: Timer,
    #[cfg(target_arch = "wasm32")]
    pending: Option<std::sync::Arc<std::sync::Mutex<Option<String>>>>,
}

impl Default for LayoutPersist {
    fn default() -> Self {
        Self {
            last_saved: None,
            timer: Timer::from_seconds(1.0, TimerMode::Repeating),
            #[cfg(target_arch = "wasm32")]
            pending: None,
        }
    }
}

fn parse(text: &str) -> Option<DockState<EditorTab>> {
    match serde_json::from_str::<EditorSettingsFile>(text) {
        Ok(file) => Some(file.dock),
        Err(e) => {
            warn!("{SETTINGS_KEY} didn't parse ({e}); using the default layout");
            None
        }
    }
}

/// Serialize the current layout; write it when it differs from the disk copy.
fn persist(layout: &DockLayout, state: &mut LayoutPersist) {
    let text = match serde_json::to_string_pretty(&EditorSettingsFile {
        dock: layout.0.clone(),
    }) {
        Ok(text) => text,
        Err(e) => {
            warn!("Failed to serialize the dock layout: {e}");
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

/// Debounced save while the editor is in front (the dock only mutates there).
pub fn save_layout(time: Res<Time>, layout: Res<DockLayout>, mut state: ResMut<LayoutPersist>) {
    if state.timer.tick(time.delta()).just_finished() {
        persist(&layout, &mut state);
    }
}

/// Flush on the way out to the menu, so quitting right after a drag keeps it.
pub fn save_layout_now(layout: Res<DockLayout>, mut state: ResMut<LayoutPersist>) {
    persist(&layout, &mut state);
}

/// Native: read the file once at startup, before the first dock pass.
#[cfg(not(target_arch = "wasm32"))]
pub fn load_layout(mut layout: ResMut<DockLayout>, mut state: ResMut<LayoutPersist>) {
    let text = match std::fs::read_to_string(SETTINGS_KEY) {
        Ok(text) => text,
        Err(_) => return, // first run
    };
    if let Some(dock) = parse(&text) {
        layout.0 = dock;
        state.last_saved = Some(text);
    }
}

/// Web: kick off the OPFS read; [`poll_layout`] applies it when it lands
/// (a frame or two in — any tab dragging done before then loses, which is
/// the same trade every OPFS load here makes).
#[cfg(target_arch = "wasm32")]
pub fn load_layout(mut state: ResMut<LayoutPersist>) {
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
            dock: DockLayout::default().0,
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
}

#[cfg(target_arch = "wasm32")]
pub fn poll_layout(mut layout: ResMut<DockLayout>, mut state: ResMut<LayoutPersist>) {
    let Some(slot) = state.pending.as_ref() else {
        return;
    };
    let Some(text) = slot.lock().unwrap().take() else {
        return;
    };
    state.pending = None;
    if let Some(dock) = parse(&text) {
        layout.0 = dock;
        state.last_saved = Some(text);
    }
}
