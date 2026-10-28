//! Assets tab: everything the project can place or inspect, by category.
//!
//! One row of category tabs (meshes, built-ins, prefabs, textures, audio), a
//! search box, and a grid of cells. Clicking a placeable cell arms
//! `PlacementConfig` through `AssetCatalog::select` (so plugin preset
//! stamps keep their semantics) and switches to the Place tool; a texture
//! cell opens an inspect-only preview below the grid; audio is listed but
//! not playable yet. Cells show a rendered thumbnail when the thumbnail
//! cache has one for the entry, else a glyph.

use bevy::prelude::*;
use bevy_egui::egui;

use super::Panels;
use crate::editor::actions::EditorAction;
use crate::editor::catalog::{AssetEntry, AssetKind};
use crate::editor::prefab_edit::PrefabAction;
use crate::editor::tool::EditorTool;

const CELL: f32 = 96.0;

#[derive(Resource)]
pub struct AssetsBrowserState {
    pub category: AssetKind,
    pub search: String,
    /// Texture being inspected (asset path).
    pub inspected: Option<String>,
    /// Set by other panels; `editor_ui` focuses the tab and clears it.
    pub focus_requested: bool,
}

impl Default for AssetsBrowserState {
    fn default() -> Self {
        Self {
            category: AssetKind::Mesh,
            search: String::new(),
            inspected: None,
            focus_requested: false,
        }
    }
}

pub fn draw(ui: &mut egui::Ui, panels: &mut Panels) {
    // ── Categories + search ──────────────────────────────────────────────────
    ui.horizontal(|ui| {
        for kind in AssetKind::ALL {
            let count = panels.catalog.of_kind(kind).count();
            let active = panels.assets.category == kind;
            if ui
                .selectable_label(active, format!("{} ({count})", kind.label()))
                .clicked()
            {
                panels.assets.category = kind;
            }
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.add(
                egui::TextEdit::singleline(&mut panels.assets.search)
                    .hint_text("search")
                    .desired_width(140.0),
            );
        });
    });
    ui.separator();

    let category = panels.assets.category;
    let needle = panels.assets.search.trim().to_lowercase();
    let shown: Vec<(usize, AssetEntry)> = panels
        .catalog
        .of_kind(category)
        .filter(|(_, e)| needle.is_empty() || e.name.to_lowercase().contains(&needle))
        .map(|(i, e)| (i, e.clone()))
        .collect();

    if shown.is_empty() {
        ui.weak(empty_text(category, &needle));
    }

    // ── Grid ─────────────────────────────────────────────────────────────────
    let selected = panels.catalog.selected_index(&panels.config);
    let mut picked: Option<usize> = None;
    egui::ScrollArea::vertical()
        .auto_shrink([false, true])
        .show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                for (i, entry) in &shown {
                    let is_selected = match entry.kind {
                        AssetKind::Texture => {
                            panels.assets.inspected.as_deref() == Some(&entry.path)
                        }
                        _ => selected == Some(*i),
                    };
                    let thumb = panels
                        .thumbnails
                        .get(entry.thumbnail_key())
                        .and_then(|h| panels.previews.egui_id(h));
                    if cell(ui, entry, is_selected, thumb).clicked() {
                        picked = Some(*i);
                    }
                    if thumb.is_none() {
                        panels.thumbnails.request(entry);
                    }
                }
            });
        });

    if let Some(i) = picked
        && let Some(entry) = panels.catalog.entries.get(i).cloned()
    {
        match entry.kind {
            AssetKind::Texture => panels.assets.inspected = Some(entry.path.clone()),
            AssetKind::Audio => {}
            _ => {
                panels.catalog.select(i, &mut panels.config);
                if *panels.tool.get() != EditorTool::Place {
                    panels
                        .actions
                        .write(EditorAction::SetTool(EditorTool::Place));
                }
            }
        }
    }

    // ── Category footers ─────────────────────────────────────────────────────
    match category {
        AssetKind::Prefab => prefab_footer(ui, panels),
        AssetKind::Texture => texture_footer(ui, panels),
        AssetKind::Audio => {
            ui.separator();
            ui.weak("Audio preview isn't wired up yet; nothing in the level plays sounds.");
        }
        _ => {}
    }
}

fn empty_text(kind: AssetKind, needle: &str) -> String {
    if !needle.is_empty() {
        return format!("No {} match '{needle}'.", kind.label().to_lowercase());
    }
    match kind {
        AssetKind::Mesh => "No meshes — the asset manifest hasn't loaded yet.".into(),
        AssetKind::Builtin => "No built-ins.".into(),
        AssetKind::Prefab => "No prefabs yet. Select an entity or group and save it below.".into(),
        AssetKind::Texture => "No textures. Drop files into assets/textures/ and rebuild.".into(),
        AssetKind::Audio => "No audio. Drop files into assets/audio/ and rebuild.".into(),
    }
}

/// One grid cell: thumbnail (or glyph) above a caption, selectable.
fn cell(
    ui: &mut egui::Ui,
    entry: &AssetEntry,
    selected: bool,
    thumb: Option<egui::TextureId>,
) -> egui::Response {
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(CELL, CELL + 22.0), egui::Sense::click());
    let visuals = ui.style().interact_selectable(&response, selected);
    let painter = ui.painter();
    painter.rect(
        rect,
        4.0,
        visuals.bg_fill,
        visuals.bg_stroke,
        egui::StrokeKind::Inside,
    );
    let image_rect = egui::Rect::from_min_size(
        rect.min + egui::vec2(4.0, 4.0),
        egui::vec2(CELL - 8.0, CELL - 8.0),
    );
    match thumb {
        Some(id) => {
            painter.image(
                id,
                image_rect,
                egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                egui::Color32::WHITE,
            );
        }
        None => {
            painter.text(
                image_rect.center(),
                egui::Align2::CENTER_CENTER,
                glyph(entry),
                egui::FontId::proportional(28.0),
                visuals.text_color(),
            );
        }
    }
    let caption = if entry.preset_script.is_some() {
        format!("{} ·preset", entry.name)
    } else {
        entry.name.clone()
    };
    painter.text(
        egui::pos2(rect.center().x, rect.max.y - 11.0),
        egui::Align2::CENTER_CENTER,
        truncate(&caption, 14),
        egui::FontId::proportional(12.0),
        visuals.text_color(),
    );
    response.on_hover_text(hover_text(entry))
}

fn glyph(entry: &AssetEntry) -> &'static str {
    match (entry.kind, entry.placed.as_ref()) {
        (AssetKind::Prefab, _) => "◆",
        (AssetKind::Texture, _) => "▦",
        (AssetKind::Audio, _) => "♪",
        (_, Some(k)) if k.is_light() => "◉",
        (_, Some(k)) if k.is_group() => "▤",
        (AssetKind::Builtin, _) => "○",
        _ => "▣",
    }
}

fn hover_text(entry: &AssetEntry) -> String {
    match (&entry.preset_script, &entry.prefab) {
        (Some((plugin, script)), _) => format!(
            "{}\nplaces the mesh and stamps {plugin}/{script}",
            entry.path
        ),
        (_, Some(name)) => format!("prefab '{name}' — click to place an instance"),
        _ if entry.kind == AssetKind::Texture => format!("{}\nclick to inspect", entry.path),
        _ if entry.kind == AssetKind::Audio => format!("{}\n(no preview yet)", entry.path),
        _ => entry.path.clone(),
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_owned()
    } else {
        let head: String = s.chars().take(max - 1).collect();
        format!("{head}…")
    }
}

/// "Save selection as prefab" lives with the prefabs it creates.
fn prefab_footer(ui: &mut egui::Ui, panels: &mut Panels) {
    ui.separator();
    ui.label("Save selection as prefab");
    let has_selection = panels.selection.primary.is_some();
    ui.horizontal(|ui| {
        ui.add(
            egui::TextEdit::singleline(&mut panels.prefab_edit.new_name)
                .hint_text("prefab-name")
                .desired_width(160.0),
        );
        if ui
            .add_enabled(has_selection, egui::Button::new("Save"))
            .clicked()
        {
            let name = panels.prefab_edit.new_name.trim().to_owned();
            panels
                .prefab_actions
                .write(PrefabAction::SaveSelectionAs(name));
            panels.prefab_edit.new_name.clear();
        }
    });
    if !has_selection {
        ui.weak("Select an entity (or a group with members) first.");
    }
    if let Some(err) = &panels.prefab_edit.error {
        ui.colored_label(egui::Color32::from_rgb(255, 120, 120), err);
    }
}

/// Inspect-only: nothing in the level consumes a standalone texture yet.
fn texture_footer(ui: &mut egui::Ui, panels: &mut Panels) {
    let Some(path) = panels.assets.inspected.clone() else {
        return;
    };
    ui.separator();
    let handle = panels.previews.texture(&path, &panels.asset_server);
    let size = panels.images.get(&handle).map(|img| img.size());
    ui.horizontal(|ui| {
        ui.monospace(&path);
        if let Some(size) = size {
            ui.weak(format!("{}×{}", size.x, size.y));
        }
    });
    match panels.previews.egui_id(&handle) {
        Some(id) if size.is_some() => {
            let size = size.expect("checked");
            ui.add(
                egui::Image::from_texture(egui::load::SizedTexture::new(
                    id,
                    egui::vec2(size.x as f32, size.y as f32),
                ))
                .max_size(egui::vec2(ui.available_width(), 240.0))
                .maintain_aspect_ratio(true),
            );
        }
        _ => {
            ui.weak("Loading…");
        }
    }
    ui.weak("Textures aren't placeable yet; this is a preview.");
}
