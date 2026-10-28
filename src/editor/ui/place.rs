//! Place settings (shown in the Tool panel while the Place tool is active):
//! the catalog, placement scale/rotation, and the stamp picker.

use bevy::prelude::*;
use bevy_egui::egui;

use super::Panels;
use crate::editor::prefab_edit::PrefabAction;

pub fn draw(ui: &mut egui::Ui, panels: &mut Panels) {
    ui.weak("LMB places at the cursor · R grabs · Shift+Scroll scale · Alt+Scroll rotate");
    ui.separator();

    // ── Catalog / Prefabs sub-tabs ───────────────────────────────────────────
    ui.horizontal(|ui| {
        let show_prefabs = panels.prefab_edit.show_prefabs;
        if ui.selectable_label(!show_prefabs, "Catalog").clicked() {
            panels.prefab_edit.show_prefabs = false;
        }
        if ui
            .selectable_label(
                show_prefabs,
                format!("Prefabs ({})", panels.prefabs.names().len()),
            )
            .clicked()
        {
            panels.prefab_edit.show_prefabs = true;
        }
    });
    ui.add_space(4.0);
    if panels.prefab_edit.show_prefabs {
        prefab_tab(ui, panels);
        return;
    }

    let selected = panels.catalog.selected_index(&panels.config);
    let mut pick: Option<usize> = None;
    ui.horizontal_wrapped(|ui| {
        for (i, entry) in panels.catalog.entries.iter().enumerate() {
            let caption = if entry.preset_script.is_some() {
                format!("{}\npreset", entry.name)
            } else if !entry.kind.is_mesh() {
                format!("{}\n{}", entry.name, entry.kind.label())
            } else {
                entry.name.clone()
            };
            let response = ui.add_sized(
                [110.0, 44.0],
                egui::Button::selectable(selected == Some(i), caption),
            );
            if response.clicked() {
                pick = Some(i);
            }
        }
    });
    if let Some(i) = pick {
        panels.catalog.select(i, &mut panels.config);
    }
    if panels.catalog.entries.is_empty() {
        ui.weak("Catalog is empty — the mesh manifest hasn't loaded yet.");
    }

    ui.separator();
    placement_settings(ui, panels);
}

/// The Prefabs tab: every saved prefab as a placement entry, plus "save the
/// selection as a new prefab".
fn prefab_tab(ui: &mut egui::Ui, panels: &mut Panels) {
    let names = panels.prefabs.names();
    if names.is_empty() {
        ui.weak("No prefabs yet. Select an entity or group below and save it as one.");
    }
    let mut pick: Option<String> = None;
    ui.horizontal_wrapped(|ui| {
        for name in &names {
            let entities = panels
                .prefabs
                .get(name)
                .map(|d| d.entities.len())
                .unwrap_or(0);
            let active = panels.config.prefab.as_deref() == Some(name.as_str());
            let caption = format!(
                "◆ {name}
{entities} entities"
            );
            if ui
                .add_sized([110.0, 44.0], egui::Button::selectable(active, caption))
                .clicked()
            {
                pick = Some(name.clone());
            }
        }
    });
    if let Some(name) = pick
        && let Some(def) = panels.prefabs.get(&name)
    {
        // A real change: the preview rebuilds with the prefab's ghosts.
        panels.config.kind = def.entities[0].kind.clone();
        panels.config.prefab = Some(name);
        panels.config.scale = Vec3::ONE;
        panels.config.stamp_script = None;
    }

    ui.separator();
    ui.label("Save selection as prefab");
    let has_selection = panels.selection.primary.is_some();
    ui.horizontal(|ui| {
        let mut name = panels.prefab_edit.new_name.clone();
        ui.add(
            egui::TextEdit::singleline(&mut name)
                .hint_text("prefab-name")
                .desired_width(160.0),
        );
        panels.prefab_edit.new_name = name;
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

    ui.separator();
    placement_settings(ui, panels);
}

fn placement_settings(ui: &mut egui::Ui, panels: &mut Panels) {
    // ── Placement settings ───────────────────────────────────────────────────
    // Scale/rotation tweaks go through `bypass_change_detection`: a change to
    // `PlacementConfig` respawns the preview, which is only wanted when the
    // *kind* changes. `update_placement_preview` copies these onto the
    // preview every frame.
    let scalable = panels.config.kind.is_mesh() || panels.config.prefab.is_some();
    let config = panels.config.bypass_change_detection();
    egui::Grid::new("placement_settings")
        .num_columns(2)
        .show(ui, |ui| {
            ui.label("Scale");
            ui.add_enabled_ui(scalable, |ui| {
                let mut s = config.scale.x;
                if ui
                    .add(
                        egui::DragValue::new(&mut s)
                            .speed(0.01)
                            .range(0.001..=100.0),
                    )
                    .changed()
                {
                    config.scale = Vec3::splat(s);
                }
            });
            ui.end_row();

            ui.label("Yaw");
            let (yaw, _, _) = config.rotation.to_euler(EulerRot::YXZ);
            let mut degrees = yaw.to_degrees();
            if ui
                .add(egui::DragValue::new(&mut degrees).speed(1.0).suffix("°"))
                .changed()
            {
                config.rotation = Quat::from_rotation_y(degrees.to_radians());
            }
            ui.end_row();

            ui.label("Stamp script");
            let mut scripts: Vec<(String, String)> = panels
                .script_registry
                .scripts()
                .map(|s| (s.plugin.clone(), s.schema.name.to_owned()))
                .collect();
            scripts.sort();
            let current = config
                .stamp_script
                .as_ref()
                .map(|(p, s)| format!("{p}/{s}"))
                .unwrap_or_else(|| "none".to_owned());
            egui::ComboBox::from_id_salt("stamp_script")
                .selected_text(current)
                .show_ui(ui, |ui| {
                    if ui
                        .selectable_label(config.stamp_script.is_none(), "none")
                        .clicked()
                    {
                        config.stamp_script = None;
                    }
                    for (plugin, script) in scripts {
                        let label = format!("{plugin}/{script}");
                        let is =
                            config.stamp_script.as_ref() == Some(&(plugin.clone(), script.clone()));
                        if ui.selectable_label(is, label).clicked() {
                            config.stamp_script = Some((plugin, script));
                        }
                    }
                });
            ui.end_row();
        });
    ui.weak("A stamp attaches that script (with schema defaults) to everything placed.");
}
