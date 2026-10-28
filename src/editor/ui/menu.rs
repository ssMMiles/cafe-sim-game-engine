//! Menu bar, toolbar, and modal dialogs.
//!
//! Anything that needs more of the ECS than the panels carry is sent as an
//! [`EditorAction`] and applied in `Update` (`editor::actions`).

use bevy::prelude::*;
use bevy_egui::egui;

use super::Panels;
use crate::editor::actions::EditorAction;
use crate::editor::tool::EditorTool;
use crate::projects;

#[derive(Resource, Default)]
pub struct MenuState {
    pub new_world_open: bool,
    pub new_world_id: String,
    pub new_world_error: Option<String>,
    /// Set by View ▸ Reset layout; consumed by `editor_ui`.
    pub reset_layout: bool,
}

pub fn menu_bar(ui: &mut egui::Ui, panels: &mut Panels) {
    egui::MenuBar::new().ui(ui, |ui| {
        ui.menu_button("File", |ui| {
            if ui.button("Save\t\tShift+S").clicked() {
                panels.actions.write(EditorAction::Save);
                ui.close();
            }
            if ui.button("Bake lightmaps\tShift+B").clicked() {
                panels.actions.write(EditorAction::Bake);
                ui.close();
            }
            ui.separator();
            let current = panels
                .world_config
                .as_deref()
                .map(|c| c.world_id.clone())
                .unwrap_or_default();
            ui.menu_button("Worlds", |ui| {
                let worlds: Vec<String> = panels
                    .active
                    .as_deref()
                    .map(|a| a.0.worlds.clone())
                    .unwrap_or_default();
                for world in worlds {
                    let is_current = world == current;
                    if ui.selectable_label(is_current, &world).clicked() && !is_current {
                        panels.actions.write(EditorAction::SwitchWorld(world));
                        ui.close();
                    }
                }
                ui.separator();
                if ui.button("New world…").clicked() {
                    panels.menu.new_world_open = true;
                    panels.menu.new_world_error = None;
                    ui.close();
                }
            });
            ui.separator();
            if ui.button("Exit to main menu").clicked() {
                panels.actions.write(EditorAction::ExitToMenu);
                ui.close();
            }
        });

        ui.menu_button("Edit", |ui| {
            let undo = panels
                .history
                .undo_label()
                .map(|l| format!("Undo {l}\tCtrl+Z"));
            if ui
                .add_enabled(
                    undo.is_some(),
                    egui::Button::new(undo.unwrap_or_else(|| "Undo\t\tCtrl+Z".into())),
                )
                .clicked()
            {
                panels.actions.write(EditorAction::Undo);
                ui.close();
            }
            let redo = panels
                .history
                .redo_label()
                .map(|l| format!("Redo {l}\tCtrl+Shift+Z"));
            if ui
                .add_enabled(
                    redo.is_some(),
                    egui::Button::new(redo.unwrap_or_else(|| "Redo\t\tCtrl+Shift+Z".into())),
                )
                .clicked()
            {
                panels.actions.write(EditorAction::Redo);
                ui.close();
            }
            ui.separator();
            let has_selection = panels.selection.primary.is_some();
            if ui
                .add_enabled(has_selection, egui::Button::new("Duplicate"))
                .clicked()
            {
                panels.actions.write(EditorAction::DuplicateSelection);
                ui.close();
            }
            if ui
                .add_enabled(has_selection, egui::Button::new("Delete\t\tDelete"))
                .clicked()
            {
                panels.actions.write(EditorAction::DeleteSelection);
                ui.close();
            }
            if ui
                .add_enabled(has_selection, egui::Button::new("Focus\t\tF"))
                .clicked()
            {
                panels.actions.write(EditorAction::FocusSelection);
                ui.close();
            }
            if ui
                .add_enabled(has_selection, egui::Button::new("Deselect\t\tEsc"))
                .clicked()
            {
                panels.actions.write(EditorAction::Deselect);
                ui.close();
            }
        });

        ui.menu_button("View", |ui| {
            let s = &mut panels.settings;
            // Each toggle is a real edit to `UserSettings`, which the sync
            // systems (`sync_lightmap_mode`, …) react to.
            let mut baked = s.use_lightmap;
            if ui.checkbox(&mut baked, "Baked lighting\tShift+L").changed() {
                s.use_lightmap = baked;
            }
            let mut edges = s.edge_detection_enabled;
            if ui.checkbox(&mut edges, "Edge detection\tF2").changed() {
                s.edge_detection_enabled = edges;
            }
            let mut physics = s.physics_debug;
            if ui
                .checkbox(&mut physics, "Physics gizmos\tShift+P")
                .changed()
            {
                s.physics_debug = physics;
            }
            ui.separator();
            if ui.button("Reset layout").clicked() {
                panels.menu.reset_layout = true;
                ui.close();
            }
        });

        ui.menu_button("Tools", |ui| {
            for (tool, key) in [
                (EditorTool::Select, "1"),
                (EditorTool::Place, "2 / Shift+E"),
                (EditorTool::Terrain, "3 / Shift+G"),
            ] {
                let active = *panels.tool.get() == tool;
                if ui
                    .selectable_label(active, format!("{}\t{key}", tool.label()))
                    .clicked()
                {
                    panels.actions.write(EditorAction::SetTool(tool));
                    ui.close();
                }
            }
        });

        ui.menu_button("Plugins", |ui| {
            let declared: Vec<String> = panels
                .active
                .as_deref()
                .map(|a| a.0.plugins.clone())
                .unwrap_or_default();
            if declared.is_empty() {
                ui.weak("No plugins in this project");
            }
            for plugin in &declared {
                let loaded = panels.loaded_plugins.0.contains(plugin);
                ui.horizontal(|ui| {
                    ui.label(plugin);
                    if !loaded {
                        ui.weak("(not loaded — cargo build --workspace)");
                    }
                    if ui.small_button("Remove").clicked() {
                        panels
                            .actions
                            .write(EditorAction::RemovePlugin(plugin.clone()));
                        ui.close();
                    }
                });
            }
            ui.separator();
            ui.menu_button("Add", |ui| {
                let candidates: Vec<String> = projects::discover_plugin_ids()
                    .into_iter()
                    .filter(|id| !declared.contains(id))
                    .collect();
                if candidates.is_empty() {
                    ui.weak("Every plugin under plugins/ is already added");
                }
                for id in candidates {
                    if ui.button(&id).clicked() {
                        panels.actions.write(EditorAction::AddPlugin(id));
                        ui.close();
                    }
                }
            });
            if ui.button("Reload all\tShift+R").clicked() {
                panels.actions.write(EditorAction::ReloadPlugins);
                ui.close();
            }
        });
    });
}

pub fn toolbar(ui: &mut egui::Ui, panels: &mut Panels) {
    ui.horizontal(|ui| {
        for tool in [EditorTool::Select, EditorTool::Place, EditorTool::Terrain] {
            let active = *panels.tool.get() == tool;
            if ui.selectable_label(active, tool.label()).clicked() && !active {
                panels.actions.write(EditorAction::SetTool(tool));
            }
        }
        ui.separator();
        if ui.button("Save").clicked() {
            panels.actions.write(EditorAction::Save);
        }
        if ui.button("Bake").clicked() {
            panels.actions.write(EditorAction::Bake);
        }
        ui.separator();
        if let Some(cfg) = panels.world_config.as_deref() {
            ui.label(format!("world: {}", cfg.world_id));
            ui.separator();
        }
        ui.label(format!("fly {:.0} m/s", panels.cam_settings.fly_speed));
        ui.separator();
        ui.weak("RMB drag: look · WASD/QE: fly · scroll: dolly · F: focus · Esc: cancel");
    });
}

/// Modal-ish windows (only "New world" for now).
pub fn dialogs(ctx: &egui::Context, panels: &mut Panels) {
    if !panels.menu.new_world_open {
        return;
    }
    let mut open = true;
    let mut submit = false;
    let mut cancel = false;
    egui::Window::new("New world")
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
        .show(ctx, |ui| {
            ui.label("World ID (letters, digits, '-', '_'):");
            let response = ui.add(
                egui::TextEdit::singleline(&mut panels.menu.new_world_id)
                    .hint_text("my-world")
                    .desired_width(240.0),
            );
            if response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                submit = true;
            }
            if let Some(err) = &panels.menu.new_world_error {
                ui.colored_label(egui::Color32::from_rgb(255, 120, 120), err);
            }
            ui.horizontal(|ui| {
                if ui.button("Create").clicked() {
                    submit = true;
                }
                if ui.button("Cancel").clicked() {
                    cancel = true;
                }
            });
        });
    if submit {
        let id = panels.menu.new_world_id.trim().to_owned();
        panels.actions.write(EditorAction::CreateWorld(id));
    }
    if cancel || !open {
        panels.menu.new_world_open = false;
        panels.menu.new_world_error = None;
    }
}
