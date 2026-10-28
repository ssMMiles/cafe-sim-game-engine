//! History tab: every transaction of the active world, oldest first, with the
//! current state marked. Clicking an entry undoes or redoes as many steps as
//! it takes to get there — the same path as repeated `Ctrl+Z`, so one click
//! can never do anything a key could not.

use bevy_egui::egui;

use super::Panels;
use crate::editor::actions::EditorAction;

pub fn draw(ui: &mut egui::Ui, panels: &mut Panels) {
    let undo: Vec<String> = panels
        .history
        .undo_entries()
        .map(|t| t.label.clone())
        .collect();
    let redo: Vec<String> = panels
        .history
        .redo_entries()
        .map(|t| t.label.clone())
        .collect();

    ui.weak("Ctrl+Z undo · Ctrl+Shift+Z / Ctrl+Y redo · click an entry to jump");
    ui.separator();

    if undo.is_empty() && redo.is_empty() {
        ui.weak("Nothing to undo yet.");
        return;
    }

    let mut jump: Option<EditorAction> = None;
    let mut steps: Option<usize> = None;

    for (i, label) in undo.iter().enumerate() {
        // Clicking an entry below the top returns to the state just after it.
        let back = undo.len() - 1 - i;
        if ui.selectable_label(false, label).clicked() && back > 0 {
            jump = Some(EditorAction::Undo);
            steps = Some(back);
        }
    }
    let _ = ui.selectable_label(true, "▶ current state");
    for (i, label) in redo.iter().enumerate() {
        let response = ui.add(egui::Button::selectable(false, label).frame(false));
        if response.clicked() {
            jump = Some(EditorAction::Redo);
            steps = Some(i + 1);
        }
    }

    if let (Some(action), Some(n)) = (jump, steps) {
        // One message per step; `apply_undo_redo` drains them in order within
        // a single frame.
        for _ in 0..n {
            panels.actions.write(action.clone());
        }
    }
}
