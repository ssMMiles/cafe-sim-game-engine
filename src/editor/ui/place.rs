//! Place settings (shown in the Tool panel while the Place tool is active):
//! the catalog, placement scale/rotation, and the stamp picker.

use bevy::prelude::*;
use bevy_egui::egui;

use super::Panels;

pub fn draw(ui: &mut egui::Ui, panels: &mut Panels) {
    ui.weak("LMB places at the cursor · R grabs · Shift+Scroll scale · Alt+Scroll rotate");
    ui.separator();

    // What the next click places, as chosen in the Assets tab.
    ui.horizontal(|ui| {
        ui.label("Placing");
        let what = match (&panels.config.prefab, &panels.config.stamp_script) {
            (Some(name), _) => format!("◆ prefab '{name}'"),
            (None, Some((plugin, script))) => {
                format!("{} + {plugin}/{script}", panels.config.kind.label())
            }
            (None, None) => match &panels.config.kind {
                crate::game::world::objects::PlacedKind::Mesh { asset_path, .. } => {
                    asset_path.clone()
                }
                kind => kind.label().to_owned(),
            },
        };
        ui.strong(what);
        if ui.small_button("Assets…").clicked() {
            panels.assets.focus_requested = true;
        }
    });
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
