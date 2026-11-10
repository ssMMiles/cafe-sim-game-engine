//! Paint settings (shown in the Tool panel while the Paint tool is active):
//! the world's palette — pick a layer, edit it, add one from the Assets
//! textures — plus the shared brush size and falloff.
//!
//! Every edit goes through `TerrainWorld.metadata.palette` (which
//! `palette_sync` watches) and is recorded as `Change::Palette`; slider
//! drags fold into one transaction via `MergeKey::Palette`.

use bevy_egui::egui;

use super::Panels;
use crate::editor::catalog::AssetKind;
use crate::editor::history::{Change, MergeKey};
use crate::editor::paint_tool::{PAINT_FLOW_RANGE, PaintSubTool};
use crate::editor::terrain_edit::BRUSH_RADIUS_RANGE;
use crate::game::world::terrain::paint::{
    BlendProfile, MAX_LAYERS, TerrainLayerData, TerrainPalette,
};

const CELL: f32 = 44.0;

pub fn draw(ui: &mut egui::Ui, panels: &mut Panels) {
    let hint = match panels.paint.sub_tool {
        PaintSubTool::Paint => {
            "LMB paints the selected layer · Ctrl+LMB erases · Alt+LMB picks · [ / ] resize"
        }
        PaintSubTool::Fill => {
            "LMB fills the disc with the selected layer · Alt+LMB picks · [ / ] resize"
        }
    };
    ui.weak(hint);
    ui.separator();

    ui.horizontal(|ui| {
        ui.label("Mode");
        for sub in [PaintSubTool::Paint, PaintSubTool::Fill] {
            ui.selectable_value(&mut panels.paint.sub_tool, sub, sub.label());
        }
        ui.add_space(8.0);
        if ui
            .button("Fill loaded chunks")
            .on_hover_text("Every loaded chunk becomes the selected layer (undoable)")
            .clicked()
        {
            panels.paint.fill_all_requested = true;
        }
    });
    ui.add_space(4.0);

    let Some(palette) = panels
        .terrain_world
        .as_ref()
        .map(|tw| tw.metadata.palette.clone())
    else {
        ui.weak("No world loaded.");
        return;
    };
    let before = palette.clone();
    let mut palette = palette;

    if panels.paint.selected >= palette.layers.len() {
        panels.paint.selected = 0;
    }

    palette_strip(ui, panels, &palette);
    ui.add_space(4.0);

    let selected = panels.paint.selected;
    if let Some(layer) = palette.layers.get_mut(selected) {
        layer_settings(ui, panels, layer, selected == 0);
    }

    ui.separator();
    egui::Grid::new("palette_settings")
        .num_columns(2)
        .show(ui, |ui| {
            ui.label("Texture size")
                .on_hover_text("Pixels per side every layer texture is resampled to for the GPU");
            egui::ComboBox::from_id_salt("palette_texture_size")
                .selected_text(format!("{} px", palette.texture_size))
                .show_ui(ui, |ui| {
                    for size in [256u32, 512, 1024] {
                        ui.selectable_value(&mut palette.texture_size, size, format!("{size} px"));
                    }
                });
            ui.end_row();

            ui.label("Brush radius");
            ui.add(egui::Slider::new(&mut panels.brush.radius, BRUSH_RADIUS_RANGE).suffix(" m"));
            ui.end_row();

            if panels.paint.sub_tool == PaintSubTool::Paint {
                ui.label("Flow")
                    .on_hover_text("How fast the blend moves towards the layer, per second");
                ui.add(egui::Slider::new(&mut panels.paint.flow, PAINT_FLOW_RANGE).suffix(" /s"));
                ui.end_row();
            }

            ui.label("Falloff");
            ui.add(egui::Slider::new(&mut panels.brush.falloff, 0.0..=1.0))
                .on_hover_text("Fraction of the radius that fades out (0 = hard edge)");
            ui.end_row();
        });

    commit(panels, before, palette, "Edit palette");
}

/// One cell per layer: the pattern tinted with the layer colour, name
/// underneath, click to select. Then the "+" that adds one.
fn palette_strip(ui: &mut egui::Ui, panels: &mut Panels, palette: &TerrainPalette) {
    ui.horizontal_wrapped(|ui| {
        for (i, layer) in palette.layers.iter().enumerate() {
            let selected = panels.paint.selected == i;
            let handle = panels
                .previews
                .texture(&layer.texture, &panels.asset_server);
            let tint = tint_color32(layer.tint);
            let (rect, response) =
                ui.allocate_exact_size(egui::vec2(CELL, CELL + 14.0), egui::Sense::click());
            let image_rect = egui::Rect::from_min_size(rect.min, egui::vec2(CELL, CELL));
            let painter = ui.painter();
            match panels.previews.egui_id(&handle) {
                Some(id) => {
                    painter.image(
                        id,
                        image_rect,
                        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                        tint,
                    );
                }
                None => {
                    painter.rect_filled(image_rect, 2.0, tint);
                }
            }
            let stroke = if selected {
                egui::Stroke::new(2.0, ui.visuals().selection.stroke.color)
            } else if response.hovered() {
                egui::Stroke::new(1.0, ui.visuals().widgets.hovered.fg_stroke.color)
            } else {
                egui::Stroke::new(1.0, ui.visuals().widgets.inactive.bg_stroke.color)
            };
            painter.rect_stroke(image_rect, 2.0, stroke, egui::StrokeKind::Inside);
            painter.text(
                egui::pos2(rect.center().x, rect.max.y - 1.0),
                egui::Align2::CENTER_BOTTOM,
                truncate(&layer.name, 7),
                egui::FontId::proportional(10.0),
                ui.visuals().text_color(),
            );
            let response =
                response.on_hover_text(format!("{} · layer {i}\n{}", layer.name, layer.texture));
            if response.clicked() {
                panels.paint.selected = i;
            }
        }

        // Add a layer from any catalog texture.
        let full = palette.layers.len() >= MAX_LAYERS;
        let textures: Vec<String> = panels
            .catalog
            .of_kind(AssetKind::Texture)
            .map(|(_, e)| e.path.clone())
            .collect();
        ui.add_enabled_ui(!full && !textures.is_empty(), |ui| {
            ui.menu_button("+", |ui| {
                for path in &textures {
                    if ui.button(path).clicked() {
                        add_layer(panels, path);
                        ui.close();
                    }
                }
            })
            .response
            .on_hover_text(if full {
                format!("Palette full ({MAX_LAYERS} layers)")
            } else {
                "Add a layer from a texture in assets/textures/".to_owned()
            });
        });
    });
}

fn layer_settings(
    ui: &mut egui::Ui,
    panels: &mut Panels,
    layer: &mut TerrainLayerData,
    is_default: bool,
) {
    egui::Grid::new("layer_settings")
        .num_columns(2)
        .show(ui, |ui| {
            ui.label("Name");
            ui.text_edit_singleline(&mut layer.name);
            ui.end_row();

            ui.label("Pattern");
            let textures: Vec<String> = panels
                .catalog
                .of_kind(AssetKind::Texture)
                .map(|(_, e)| e.path.clone())
                .collect();
            egui::ComboBox::from_id_salt("layer_texture")
                .selected_text(short_name(&layer.texture))
                .width(160.0)
                .show_ui(ui, |ui| {
                    for path in &textures {
                        ui.selectable_value(&mut layer.texture, path.clone(), short_name(path));
                    }
                });
            ui.end_row();

            ui.label("Tint");
            // Edited in sRGB, stored linear (like every colour the shaders read).
            let srgb =
                bevy::prelude::Color::linear_rgb(layer.tint[0], layer.tint[1], layer.tint[2])
                    .to_srgba();
            let mut rgb = [srgb.red, srgb.green, srgb.blue];
            if ui.color_edit_button_rgb(&mut rgb).changed() {
                let lin = bevy::prelude::Color::srgb(rgb[0], rgb[1], rgb[2]).to_linear();
                layer.tint = [lin.red, lin.green, lin.blue];
            }
            ui.end_row();

            ui.label("Pattern strength")
                .on_hover_text("How far the pattern's dark parts pull the tint towards black");
            ui.add(egui::Slider::new(&mut layer.pattern_strength, 0.0..=1.0));
            ui.end_row();

            ui.label("Tiling");
            ui.add(
                egui::DragValue::new(&mut layer.tiling)
                    .range(0.25..=100.0)
                    .speed(0.05)
                    .suffix(" m"),
            )
            .on_hover_text("World metres per texture repeat");
            ui.end_row();

            ui.label("No-tile");
            ui.checkbox(&mut layer.no_tile, "").on_hover_text(
                "Stochastic sampling that hides the repeat grid (4 taps instead of 1)",
            );
            ui.end_row();

            ui.label("Blend");
            let current = BlendProfile::from_u8(layer.blend_profile);
            egui::ComboBox::from_id_salt("layer_blend")
                .selected_text(current.label())
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut layer.blend_profile, BlendProfile::Soft as u8, "Soft");
                    for p in [
                        BlendProfile::Hard,
                        BlendProfile::Height,
                        BlendProfile::Scatter,
                    ] {
                        ui.add_enabled(
                            false,
                            egui::Button::new(format!("{} (Phase 2)", p.label())),
                        );
                    }
                });
            ui.end_row();
        });
    if is_default {
        ui.weak("Layer 0 is what unpainted ground shows; it can be edited but not removed.");
    }
}

/// Push a new layer using `texture` as its pattern and select it. Shared
/// with the Assets tab's texture footer.
pub fn add_layer(panels: &mut Panels, texture: &str) {
    let Some(before) = panels
        .terrain_world
        .as_ref()
        .map(|tw| tw.metadata.palette.clone())
    else {
        return;
    };
    if before.is_full() {
        return;
    }
    let mut after = before.clone();
    let name = short_name(texture);
    let mut layer = TerrainLayerData::new(unique_name(&after, &name), texture);
    // A fresh layer takes the default's tiling so patterns line up until
    // the user says otherwise.
    if let Some(first) = after.layers.first() {
        layer.tiling = first.tiling;
    }
    after.layers.push(layer);
    panels.paint.selected = after.layers.len() - 1;
    if let Some(tw) = panels.terrain_world.as_deref_mut() {
        tw.metadata.palette = after.clone();
    }
    panels.history.push(
        format!("Add layer '{name}'"),
        vec![Change::Palette { before, after }],
    );
}

/// Write `after` if it differs and record it, merging continuous edits.
fn commit(panels: &mut Panels, before: TerrainPalette, after: TerrainPalette, label: &str) {
    if before == after {
        return;
    }
    if let Some(tw) = panels.terrain_world.as_deref_mut() {
        tw.metadata.palette = after.clone();
    }
    panels
        .history
        .merge_or_push(MergeKey::Palette, label, Change::Palette { before, after });
}

fn tint_color32(tint: [f32; 3]) -> egui::Color32 {
    let srgb = bevy::prelude::Color::linear_rgb(tint[0], tint[1], tint[2]).to_srgba();
    egui::Color32::from_rgb(
        (srgb.red * 255.0).round() as u8,
        (srgb.green * 255.0).round() as u8,
        (srgb.blue * 255.0).round() as u8,
    )
}

/// `textures/grass.png` → `grass`.
fn short_name(path: &str) -> String {
    std::path::Path::new(path)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_owned())
}

fn unique_name(palette: &TerrainPalette, base: &str) -> String {
    if !palette.layers.iter().any(|l| l.name == base) {
        return base.to_owned();
    }
    (2..)
        .map(|n| format!("{base} {n}"))
        .find(|c| !palette.layers.iter().any(|l| &l.name == c))
        .expect("unbounded")
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_owned()
    } else {
        format!("{}…", s.chars().take(max - 1).collect::<String>())
    }
}
