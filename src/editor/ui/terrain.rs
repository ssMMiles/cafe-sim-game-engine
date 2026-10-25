//! Terrain settings (shown in the Tool panel while the Terrain tool is
//! active): brush mode, radius, strength, falloff.

use bevy_egui::egui;

use super::Panels;
use crate::editor::terrain_edit::{BRUSH_RADIUS_RANGE, BRUSH_STRENGTH_RANGE, BrushMode};

pub fn draw(ui: &mut egui::Ui, panels: &mut Panels) {
    let hint = match panels.brush.mode {
        BrushMode::Sculpt => "LMB raises · Ctrl+LMB lowers · [ / ] resize the brush",
        BrushMode::Flatten => {
            "LMB flattens toward the height where the stroke began · [ / ] resize"
        }
        BrushMode::Smooth => "LMB smooths toward the neighbourhood average · [ / ] resize",
    };
    ui.weak(hint);
    ui.separator();

    ui.horizontal(|ui| {
        ui.label("Mode");
        for mode in [BrushMode::Sculpt, BrushMode::Flatten, BrushMode::Smooth] {
            ui.selectable_value(&mut panels.brush.mode, mode, mode.label());
        }
    });
    ui.add_space(4.0);

    egui::Grid::new("brush_settings")
        .num_columns(2)
        .show(ui, |ui| {
            ui.label("Radius");
            ui.add(egui::Slider::new(&mut panels.brush.radius, BRUSH_RADIUS_RANGE).suffix(" m"));
            ui.end_row();

            ui.label("Strength");
            ui.add(
                egui::Slider::new(&mut panels.brush.strength, BRUSH_STRENGTH_RANGE).suffix(" /s"),
            );
            ui.end_row();

            ui.label("Falloff");
            ui.add(egui::Slider::new(&mut panels.brush.falloff, 0.0..=1.0))
                .on_hover_text("Fraction of the radius that fades out (0 = hard edge)");
            ui.end_row();
        });
}
