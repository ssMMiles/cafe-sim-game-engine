//! Terrain panel: brush settings.

use bevy_egui::egui;

use super::Panels;
use crate::editor::actions::EditorAction;
use crate::editor::tool::EditorTool;

pub fn draw(ui: &mut egui::Ui, panels: &mut Panels) {
    let sculpting = *panels.tool.get() == EditorTool::Terrain;
    ui.horizontal(|ui| {
        if sculpting {
            ui.weak("LMB raises · Ctrl+LMB lowers · [ / ] resize the brush");
        } else {
            ui.weak("Switch to the Terrain tool to sculpt (3 / Shift+G).");
            if ui.small_button("Terrain tool").clicked() {
                panels
                    .actions
                    .write(EditorAction::SetTool(EditorTool::Terrain));
            }
        }
    });
    ui.separator();

    egui::Grid::new("brush_settings")
        .num_columns(2)
        .show(ui, |ui| {
            ui.label("Radius");
            let mut radius = panels.settings.brush_radius;
            if ui
                .add(egui::Slider::new(&mut radius, 1.0..=50.0).suffix(" m"))
                .changed()
            {
                panels.settings.brush_radius = radius;
            }
            ui.end_row();

            ui.label("Strength");
            ui.add(egui::Slider::new(&mut panels.brush.strength, 5.0..=200.0).suffix(" /s"));
            ui.end_row();
        });
}
