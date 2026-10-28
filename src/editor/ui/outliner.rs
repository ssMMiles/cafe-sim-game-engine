//! Outliner: every placed entity in the loaded chunks, as a tree of groups.
//!
//! Only loaded chunks have entities, so this lists the streaming ring around
//! the camera, not the whole world; the header says so. A member whose group
//! isn't loaded is shown at the top level with a marker rather than hidden.
//!
//! Rows are drag sources and group rows are drop targets (egui's built-in
//! drag-and-drop), the bottom strip drops to the top level, and right-click
//! offers rename (inline), focus, grouping and delete.
//!
//! Rows sense click *and* drag on one widget and set the drag payload from
//! their own response. `Ui::dnd_drag_source` is deliberately not used: it
//! lays a drag-only interaction over its contents, which takes the pointer
//! (grab cursor) and swallows every click and right-click on the label.

use std::collections::{HashMap, HashSet};

use bevy::prelude::*;
use bevy_egui::egui;

use super::Panels;
use crate::editor::actions::EditorAction;
use crate::editor::edits::PlacedEdited;
use crate::game::world::objects::PlacedKind;

#[derive(Resource, Default)]
pub struct OutlinerState {
    pub filter: String,
    /// Group ids whose members are hidden.
    collapsed: HashSet<u64>,
    /// Inline rename in progress: the entity and the text so far.
    renaming: Option<(Entity, String)>,
    /// First pass of a rename: give the field focus.
    rename_focus_pending: bool,
}

impl OutlinerState {
    pub fn start_rename(&mut self, entity: Entity, current: &str) {
        self.renaming = Some((entity, current.to_owned()));
        self.rename_focus_pending = true;
    }
}

#[derive(Clone)]
struct Row {
    entity: Entity,
    id: u64,
    parent: Option<u64>,
    name: String,
    kind: &'static str,
    icon: &'static str,
    is_group: bool,
    scripts: usize,
    pending: bool,
}

fn icon(kind: &PlacedKind) -> &'static str {
    match kind {
        PlacedKind::Mesh { .. } => "▣",
        PlacedKind::PointLight { .. } => "✦",
        PlacedKind::SpotLight { .. } => "◤",
        PlacedKind::Empty => "○",
        PlacedKind::Group => "▤",
    }
}

pub fn draw(ui: &mut egui::Ui, panels: &mut Panels) {
    let mut rows: Vec<Row> = panels
        .placed
        .iter()
        .map(|(entity, obj, _, bindings, pending)| Row {
            entity,
            id: obj.id,
            parent: obj.parent,
            name: Panels::display_name(obj),
            kind: obj.kind.label(),
            icon: icon(&obj.kind),
            is_group: obj.kind.is_group(),
            scripts: bindings.map(|b| b.0.len()).unwrap_or(0),
            pending,
        })
        .collect();
    rows.sort_by(|a, b| {
        b.is_group
            .cmp(&a.is_group)
            .then(a.name.cmp(&b.name))
            .then(a.id.cmp(&b.id))
    });
    let total = rows.len();

    ui.horizontal(|ui| {
        ui.add(
            egui::TextEdit::singleline(&mut panels.outliner.filter)
                .hint_text("filter")
                .desired_width(f32::INFINITY),
        );
    });
    let filter = panels.outliner.filter.trim().to_lowercase();
    ui.weak(format!("{total} entities in loaded chunks"))
        .on_hover_text("Unloaded chunks are not listed; fly closer to stream them in.");
    ui.separator();

    if !filter.is_empty() {
        // Flat filtered list: a tree with most branches pruned reads worse.
        let shown: Vec<Row> = rows
            .into_iter()
            .filter(|r| {
                r.name.to_lowercase().contains(&filter)
                    || r.kind.to_lowercase().contains(&filter)
                    || r.id.to_string() == filter
            })
            .collect();
        for row in &shown {
            draw_row(ui, panels, row, 0, false);
        }
        return;
    }

    // Tree: children keyed by parent id; roots are unparented rows and rows
    // whose group isn't loaded.
    let loaded_ids: HashSet<u64> = rows.iter().map(|r| r.id).collect();
    let mut children: HashMap<u64, Vec<Row>> = HashMap::new();
    let mut roots: Vec<(Row, bool)> = Vec::new();
    for row in rows {
        match row.parent {
            Some(p) if loaded_ids.contains(&p) => children.entry(p).or_default().push(row),
            Some(_) => roots.push((row, true)),
            None => roots.push((row, false)),
        }
    }
    for (row, orphan) in &roots {
        draw_tree(ui, panels, row, &children, 0, *orphan, &mut HashSet::new());
    }

    ui.add_space(6.0);
    // Drop here to move something to the top level.
    let frame = egui::Frame::new()
        .stroke(ui.visuals().widgets.noninteractive.bg_stroke)
        .inner_margin(egui::Margin::same(6));
    let (_, dropped) = ui.dnd_drop_zone::<Entity, ()>(frame, |ui| {
        ui.weak("Drop here for top level");
    });
    if let Some(entity) = dropped {
        panels.actions.write(EditorAction::SetParent {
            child: *entity,
            parent: None,
        });
    }
}

fn draw_tree(
    ui: &mut egui::Ui,
    panels: &mut Panels,
    row: &Row,
    children: &HashMap<u64, Vec<Row>>,
    depth: usize,
    orphan: bool,
    visited: &mut HashSet<u64>,
) {
    if !visited.insert(row.id) {
        return; // a cycle on disk must not recurse forever
    }
    draw_row(ui, panels, row, depth, orphan);
    if row.is_group && !panels.outliner.collapsed.contains(&row.id) {
        if let Some(members) = children.get(&row.id) {
            for member in members {
                draw_tree(ui, panels, member, children, depth + 1, false, visited);
            }
        } else {
            ui.horizontal(|ui| {
                ui.add_space(indent(depth + 1));
                ui.weak("(empty — drag entities here)");
            });
        }
    }
}

fn indent(depth: usize) -> f32 {
    depth as f32 * 16.0
}

fn draw_row(ui: &mut egui::Ui, panels: &mut Panels, row: &Row, depth: usize, orphan: bool) {
    let is_selected = panels.selection.primary == Some(row.entity);
    let renaming = panels
        .outliner
        .renaming
        .as_ref()
        .is_some_and(|(e, _)| *e == row.entity);

    ui.horizontal(|ui| {
        ui.add_space(indent(depth));

        if row.is_group {
            let collapsed = panels.outliner.collapsed.contains(&row.id);
            if ui.small_button(if collapsed { "▸" } else { "▾" }).clicked() {
                if collapsed {
                    panels.outliner.collapsed.remove(&row.id);
                } else {
                    panels.outliner.collapsed.insert(row.id);
                }
            }
        } else {
            ui.add_space(18.0);
        }

        if renaming {
            rename_field(ui, panels, row);
            return;
        }

        let mut label = format!("{} {}", row.icon, row.name);
        if row.scripts > 0 {
            label.push_str(&format!("  ·{}", row.scripts));
        }
        if orphan {
            label.push_str("  ⤴");
        }

        // One widget, click + drag: a click (press and release without
        // moving) selects, a drag carries the entity to a group row.
        let response = ui
            .add(egui::Button::selectable(is_selected, label).sense(egui::Sense::click_and_drag()));
        response.dnd_set_drag_payload(row.entity);
        let response = response.on_hover_text(format!(
            "{}  id {}{}{}",
            row.kind,
            row.id,
            if row.pending {
                "  (script awaiting plugin)"
            } else {
                ""
            },
            if orphan { "  (group not loaded)" } else { "" }
        ));

        if response.clicked() {
            panels.selection.primary = Some(row.entity);
        }
        if response.double_clicked() {
            panels.selection.primary = Some(row.entity);
            panels.selection.focus_requested = true;
        }

        // Dropping another row onto a group files it there.
        if row.is_group {
            let target = &response;
            if let Some(dragged) = target.dnd_hover_payload::<Entity>()
                && *dragged != row.entity
            {
                ui.painter().rect_stroke(
                    target.rect,
                    2.0,
                    ui.visuals().selection.stroke,
                    egui::StrokeKind::Outside,
                );
            }
            if let Some(dragged) = target.dnd_release_payload::<Entity>()
                && *dragged != row.entity
            {
                panels.actions.write(EditorAction::SetParent {
                    child: *dragged,
                    parent: Some(row.entity),
                });
            }
        }

        response.context_menu(|ui| context_menu(ui, panels, row));
    });
}

fn context_menu(ui: &mut egui::Ui, panels: &mut Panels, row: &Row) {
    let select = |panels: &mut Panels| panels.selection.primary = Some(row.entity);
    if ui.button("Rename").clicked() {
        select(panels);
        let current = if row.name == row.kind { "" } else { &row.name };
        panels.outliner.start_rename(row.entity, current);
        ui.close();
    }
    if ui.button("Focus").clicked() {
        select(panels);
        panels.actions.write(EditorAction::FocusSelection);
        ui.close();
    }
    ui.separator();
    if ui.button("New group with this").clicked() {
        select(panels);
        panels.actions.write(EditorAction::GroupSelection);
        ui.close();
    }
    if row.is_group && ui.button("Ungroup").clicked() {
        select(panels);
        panels.actions.write(EditorAction::UngroupSelection);
        ui.close();
    }
    if row.parent.is_some() && ui.button("Move to top level").clicked() {
        panels.actions.write(EditorAction::SetParent {
            child: row.entity,
            parent: None,
        });
        ui.close();
    }
    ui.separator();
    if ui.button("Duplicate").clicked() {
        select(panels);
        panels.actions.write(EditorAction::DuplicateSelection);
        ui.close();
    }
    if ui
        .button(if row.is_group {
            "Delete (keep members)"
        } else {
            "Delete"
        })
        .clicked()
    {
        select(panels);
        panels.actions.write(EditorAction::DeleteSelection);
        ui.close();
    }
    if row.is_group && ui.button("Delete with members").clicked() {
        select(panels);
        panels.actions.write(EditorAction::DeleteSelectionRecursive);
        ui.close();
    }
}

/// Inline rename: commits on Enter or focus loss, cancels on Escape.
fn rename_field(ui: &mut egui::Ui, panels: &mut Panels, row: &Row) {
    let focus_pending = std::mem::take(&mut panels.outliner.rename_focus_pending);
    let Some((_, text)) = panels.outliner.renaming.as_mut() else {
        return;
    };
    let response = ui.add(
        egui::TextEdit::singleline(text)
            .hint_text(row.kind)
            .desired_width(f32::INFINITY),
    );
    if focus_pending {
        response.request_focus();
        return;
    }
    let escape = ui.input(|i| i.key_pressed(egui::Key::Escape));
    if escape {
        panels.outliner.renaming = None;
        return;
    }
    if response.lost_focus() {
        let new_name = text.trim().to_owned();
        panels.outliner.renaming = None;
        if let Ok((_, mut obj, _, _, _)) = panels.placed.get_mut(row.entity)
            && obj.name != new_name
        {
            let before = std::mem::replace(&mut obj.name, new_name);
            panels.edits.write(PlacedEdited::name(row.entity, before));
        }
    }
}
