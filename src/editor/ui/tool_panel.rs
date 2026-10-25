//! Tool panel: settings for whatever [`EditorTool`] is active.
//!
//! One tab instead of one per tool, so the dock never shows settings for a
//! tool that isn't live. Each tool's body stays in its own module
//! (`place`, `terrain`); this is just the dispatch.

use bevy_egui::egui;

use super::{Panels, place, terrain};
use crate::editor::tool::EditorTool;

pub fn draw(ui: &mut egui::Ui, panels: &mut Panels) {
    match *panels.tool.get() {
        EditorTool::Select => {
            ui.weak("Select tool has no settings. LMB selects · Alt+LMB selects the group.");
        }
        EditorTool::Place => place::draw(ui, panels),
        EditorTool::Terrain => terrain::draw(ui, panels),
    }
}
