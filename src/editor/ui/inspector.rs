//! Inspector: properties of the selected entity, or the world environment
//! when nothing is selected.
//!
//! Live fields (name, transform, light parameters) write straight into the
//! components and report a [`PlacedEdited`]; the commit system does the chunk
//! and dirty bookkeeping. Script props are drafted locally and committed as
//! one [`ScriptEdit::SetProps`] when the user stops interacting, because a
//! commit re-hydrates the binding and resets the plugin's runtime state.

use bevy::prelude::*;
use bevy_egui::egui;
use engine_api::{PropValue, ScriptBinding, ScriptProp};

use super::Panels;
use crate::editor::actions::EditorAction;
use crate::editor::edits::{EditKind, PlacedEdited, RespawnPlaced, ScriptEdit};
use crate::game::world::objects::{ColliderMode, PlacedKind, PlacedObject};
use crate::game::world::terrain::storage::EnvironmentData;

#[derive(Resource)]
pub struct InspectorState {
    /// Name being typed, committed on Enter / focus loss.
    name_draft: Option<(Entity, String)>,
    /// Euler angles (degrees, X/Y/Z) shown for the rotation, with the
    /// quaternion they were derived from. Re-derived only when the quaternion
    /// changes underneath, so dragging a field doesn't fight the round trip.
    euler: Option<(Entity, Quat, Vec3)>,
    /// Scale fields move together, preserving the current proportions.
    lock_scale: bool,
    /// One draft per binding of the selected entity (an entity can carry
    /// several scripts, each edited independently).
    props: Vec<PropsDraft>,
}

impl Default for InspectorState {
    fn default() -> Self {
        Self {
            name_draft: None,
            euler: None,
            lock_scale: true,
            props: Vec::new(),
        }
    }
}

struct PropsDraft {
    entity: Entity,
    plugin: String,
    script: String,
    props: Vec<ScriptProp>,
    dirty: bool,
}

pub fn draw(ui: &mut egui::Ui, panels: &mut Panels) {
    if panels.grabbed.0.is_some() {
        ui.label("Carrying an entity.");
        ui.weak("LMB drops it at the cursor · R or Esc puts it back · Delete destroys it.");
        return;
    }
    let Some(entity) = panels.selection.primary else {
        environment(ui, panels);
        return;
    };

    // Names of every placed entity, for `EntityRef` props. Collected before the
    // mutable borrow of the selected row below.
    let names: Vec<(u64, String)> = panels
        .placed
        .iter()
        .map(|(_, obj, _, _, _)| (obj.id, Panels::display_name(obj)))
        .collect();

    // The selected row is borrowed mutably only inside this block; the
    // sections after it take `panels` whole.
    let Ok((_, mut obj, mut transform, bindings, pending)) = panels.placed.get_mut(entity) else {
        return;
    };
    let bindings: Vec<ScriptBinding> = bindings.map(|b| b.0.clone()).unwrap_or_default();
    let (is_mesh, id) = {
        let state = &mut *panels.inspector;
        state.props.retain(|d| d.entity == entity);

        // ── Header ───────────────────────────────────────────────────────────────
        state.name_draft = header(
            ui,
            panels_header(entity, &obj, state),
            &mut obj,
            &mut panels.edits,
        )
        .map(|draft| (entity, draft));
        ui.horizontal(|ui| {
            if ui.button("Focus").clicked() {
                panels.actions.write(EditorAction::FocusSelection);
            }
            if ui.button("Duplicate").clicked() {
                panels.actions.write(EditorAction::DuplicateSelection);
            }
            if ui.button("Delete").clicked() {
                panels.actions.write(EditorAction::DeleteSelection);
            }
        });
        ui.separator();

        // ── Transform ────────────────────────────────────────────────────────────
        ui.collapsing_header_default_open("Transform", |ui| {
            if transform_fields(ui, entity, &mut transform, obj.kind.is_mesh(), state) {
                panels.edits.write(PlacedEdited {
                    entity,
                    what: EditKind::Transform,
                });
            }
        });

        // ── Kind ─────────────────────────────────────────────────────────────────
        let kind_title = obj.kind.label();
        ui.collapsing_header_default_open(kind_title, |ui| {
            match kind_fields(ui, &mut obj, &panels.config) {
                KindChange::None => {}
                KindChange::InPlace => {
                    panels.edits.write(PlacedEdited {
                        entity,
                        what: EditKind::Kind,
                    });
                }
                KindChange::Respawn => {
                    panels.respawns.write(RespawnPlaced::InPlace(entity));
                }
            }
        });

        (obj.kind.is_mesh(), obj.id)
    };

    // ── Scripts ──────────────────────────────────────────────────────────────
    ui.collapsing_header_default_open("Scripts", |ui| {
        if pending {
            ui.colored_label(
                egui::Color32::from_rgb(255, 200, 80),
                "Awaiting plugin: a binding here names a plugin that isn't loaded.",
            );
        }
        for binding in &bindings {
            script_section(ui, entity, binding, panels, &names);
        }
        attach_picker(ui, entity, &bindings, panels);
    });

    // ── Lighting ─────────────────────────────────────────────────────────────
    ui.collapsing("Lighting", |ui| {
        if panels.lighting.stale {
            ui.colored_label(
                egui::Color32::from_rgb(255, 200, 80),
                "Baked lighting is out of date.",
            );
        } else {
            ui.weak("Baked lighting matches the level.");
        }
        #[cfg(not(target_arch = "wasm32"))]
        if is_mesh && let Some(tw) = panels.terrain_world.as_deref() {
            use crate::game::world::baking::object_lightmaps::object_lightmap_path;
            let has = object_lightmap_path(&tw.world_path, id, 0).exists();
            ui.weak(if has {
                "This object has a baked lightmap."
            } else {
                "No lightmap baked for this object yet."
            });
        }
        if ui.button("Bake (Shift+B)").clicked() {
            panels.actions.write(EditorAction::Bake);
        }
    });
}

struct HeaderInfo {
    entity: Entity,
    id: u64,
    kind: &'static str,
    chunk: (i32, i32),
    draft: String,
}

fn panels_header(entity: Entity, obj: &PlacedObject, state: &mut InspectorState) -> HeaderInfo {
    let draft = match &state.name_draft {
        Some((e, text)) if *e == entity => text.clone(),
        _ => obj.name.clone(),
    };
    HeaderInfo {
        entity,
        id: obj.id,
        kind: obj.kind.label(),
        chunk: obj.chunk,
        draft,
    }
}

/// Name field + id line. Returns the in-progress draft while the field has
/// focus, `None` once it has been committed (so the field re-syncs with the
/// live name next pass).
fn header(
    ui: &mut egui::Ui,
    info: HeaderInfo,
    obj: &mut PlacedObject,
    edits: &mut MessageWriter<PlacedEdited>,
) -> Option<String> {
    let mut draft = info.draft;
    let response = ui.add(
        egui::TextEdit::singleline(&mut draft)
            .hint_text(info.kind)
            .desired_width(f32::INFINITY)
            .font(egui::TextStyle::Heading),
    );
    ui.weak(format!(
        "{}  ·  id {}  ·  chunk ({}, {})",
        info.kind, info.id, info.chunk.0, info.chunk.1
    ));
    if response.lost_focus() {
        let trimmed = draft.trim();
        if trimmed != obj.name {
            obj.name = trimmed.to_owned();
            edits.write(PlacedEdited {
                entity: info.entity,
                what: EditKind::Name,
            });
        }
        return None;
    }
    response.has_focus().then_some(draft)
}

/// Position / rotation / scale drag fields. Returns true if anything changed.
fn transform_fields(
    ui: &mut egui::Ui,
    entity: Entity,
    transform: &mut Transform,
    show_scale: bool,
    state: &mut InspectorState,
) -> bool {
    let mut changed = false;

    let mut euler = match state.euler {
        Some((e, q, eu)) if e == entity && q == transform.rotation => eu,
        _ => {
            let (y, x, z) = transform.rotation.to_euler(EulerRot::YXZ);
            Vec3::new(x.to_degrees(), y.to_degrees(), z.to_degrees())
        }
    };

    egui::Grid::new("transform").num_columns(4).show(ui, |ui| {
        ui.label("Position");
        for axis in 0..3 {
            if ui
                .add(egui::DragValue::new(&mut transform.translation[axis]).speed(0.05))
                .changed()
            {
                changed = true;
            }
        }
        ui.end_row();

        ui.label("Rotation");
        let mut rot_changed = false;
        for axis in 0..3 {
            if ui
                .add(
                    egui::DragValue::new(&mut euler[axis])
                        .speed(1.0)
                        .suffix("°"),
                )
                .changed()
            {
                rot_changed = true;
            }
        }
        if rot_changed {
            transform.rotation = Quat::from_euler(
                EulerRot::YXZ,
                euler.y.to_radians(),
                euler.x.to_radians(),
                euler.z.to_radians(),
            );
            changed = true;
        }
        ui.end_row();

        if show_scale {
            ui.label("Scale");
            let before = transform.scale;
            let mut edited_axis = None;
            for axis in 0..3 {
                if ui
                    .add(
                        egui::DragValue::new(&mut transform.scale[axis])
                            .speed(0.01)
                            .range(0.001..=100.0),
                    )
                    .changed()
                {
                    edited_axis = Some(axis);
                    changed = true;
                }
            }
            ui.toggle_value(&mut state.lock_scale, "🔗")
                .on_hover_text("Keep proportions: scaling one axis scales the others to match");
            if let Some(axis) = edited_axis
                && state.lock_scale
                && before[axis] > 0.0
            {
                let ratio = transform.scale[axis] / before[axis];
                transform.scale = (before * ratio).max(Vec3::splat(0.001));
            }
            ui.end_row();
        }
    });

    state.euler = Some((entity, transform.rotation, euler));
    changed
}

enum KindChange {
    None,
    InPlace,
    Respawn,
}

fn kind_fields(
    ui: &mut egui::Ui,
    obj: &mut PlacedObject,
    config: &crate::game::world::objects::PlacementConfig,
) -> KindChange {
    let mut change = KindChange::None;
    match &mut obj.kind {
        PlacedKind::Mesh {
            asset_path,
            collider,
        } => {
            ui.horizontal_wrapped(|ui| {
                ui.label("Asset");
                ui.monospace(asset_path.as_str());
            });
            if let PlacedKind::Mesh {
                asset_path: catalog_path,
                ..
            } = &config.kind
                && catalog_path != asset_path
                && ui
                    .button(format!("Replace with catalog selection ({catalog_path})"))
                    .clicked()
            {
                *asset_path = catalog_path.clone();
                change = KindChange::Respawn;
            }
            ui.horizontal(|ui| {
                ui.label("Collider");
                let before = *collider;
                egui::ComboBox::from_id_salt("collider")
                    .selected_text(match collider {
                        ColliderMode::Trimesh => "Trimesh",
                        ColliderMode::None => "None",
                    })
                    .show_ui(ui, |ui| {
                        ui.selectable_value(collider, ColliderMode::Trimesh, "Trimesh");
                        ui.selectable_value(collider, ColliderMode::None, "None");
                    });
                if *collider != before {
                    change = KindChange::Respawn;
                }
            });
        }
        PlacedKind::PointLight {
            color,
            intensity,
            range,
            ..
        } => {
            egui::Grid::new("point_light")
                .num_columns(2)
                .show(ui, |ui| {
                    if light_common(ui, color, intensity, range) {
                        change = KindChange::InPlace;
                    }
                });
        }
        PlacedKind::SpotLight {
            color,
            intensity,
            range,
            inner_angle,
            outer_angle,
            ..
        } => {
            egui::Grid::new("spot_light").num_columns(2).show(ui, |ui| {
                if light_common(ui, color, intensity, range) {
                    change = KindChange::InPlace;
                }
                ui.label("Inner angle");
                let mut inner = inner_angle.to_degrees();
                if ui
                    .add(
                        egui::DragValue::new(&mut inner)
                            .speed(0.5)
                            .range(0.0..=89.0)
                            .suffix("°"),
                    )
                    .changed()
                {
                    *inner_angle = inner.to_radians();
                    *outer_angle = outer_angle.max(*inner_angle);
                    change = KindChange::InPlace;
                }
                ui.end_row();
                ui.label("Outer angle");
                let mut outer = outer_angle.to_degrees();
                if ui
                    .add(
                        egui::DragValue::new(&mut outer)
                            .speed(0.5)
                            .range(0.0..=89.0)
                            .suffix("°"),
                    )
                    .changed()
                {
                    *outer_angle = outer.to_radians();
                    *inner_angle = inner_angle.min(*outer_angle);
                    change = KindChange::InPlace;
                }
                ui.end_row();
            });
        }
        PlacedKind::Empty => {
            ui.weak("Transform-only anchor.");
        }
        PlacedKind::Group => {
            ui.weak(
                "Outliner group. Moving, rotating or scaling it carries its loaded \
                 members along; drag entities onto it in the outliner.",
            );
        }
    }
    change
}

/// Colour / intensity / range rows shared by both light kinds. Colours are
/// linear RGB on both sides (egui's `Rgba` is linear).
fn light_common(
    ui: &mut egui::Ui,
    color: &mut [f32; 3],
    intensity: &mut f32,
    range: &mut f32,
) -> bool {
    let mut changed = false;
    ui.label("Colour");
    if ui.color_edit_button_rgb(color).changed() {
        changed = true;
    }
    ui.end_row();
    ui.label("Intensity")
        .on_hover_text("Lightmap units: 1.0 lights a facing surface to full base colour.");
    if ui
        .add(
            egui::DragValue::new(intensity)
                .speed(0.01)
                .range(0.0..=100.0),
        )
        .changed()
    {
        changed = true;
    }
    ui.end_row();
    ui.label("Range");
    if ui
        .add(
            egui::DragValue::new(range)
                .speed(0.1)
                .range(0.1..=500.0)
                .suffix(" m"),
        )
        .changed()
    {
        changed = true;
    }
    ui.end_row();
    changed
}

fn script_section(
    ui: &mut egui::Ui,
    entity: Entity,
    binding: &ScriptBinding,
    panels: &mut Panels,
    names: &[(u64, String)],
) {
    let title = format!("{}/{}", binding.plugin, binding.script);
    let known = panels
        .script_registry
        .get(&binding.plugin, &binding.script)
        .is_some();

    egui::CollapsingHeader::new(&title)
        .default_open(true)
        .show(ui, |ui| {
            if !known {
                ui.colored_label(
                    egui::Color32::from_rgb(255, 200, 80),
                    "Plugin not loaded — props shown raw.",
                );
            }

            // Draft: the props being edited for this binding. While it is
            // clean it simply mirrors the live values.
            let state = &mut *panels.inspector;
            let slot = state.props.iter().position(|d| {
                d.entity == entity && d.plugin == binding.plugin && d.script == binding.script
            });
            let slot = match slot {
                Some(i) if state.props[i].dirty => i,
                Some(i) => {
                    state.props[i].props = binding.props.clone();
                    i
                }
                None => {
                    state.props.push(PropsDraft {
                        entity,
                        plugin: binding.plugin.clone(),
                        script: binding.script.clone(),
                        props: binding.props.clone(),
                        dirty: false,
                    });
                    state.props.len() - 1
                }
            };
            let draft = &mut state.props[slot];

            let mut interacting = false;
            let mut touched = false;
            if draft.props.is_empty() {
                ui.weak("No props.");
            }
            egui::Grid::new(("props", &title))
                .num_columns(2)
                .show(ui, |ui| {
                    for prop in &mut draft.props {
                        ui.label(&prop.key);
                        let (changed, busy) = prop_widget(ui, &mut prop.value, names);
                        touched |= changed;
                        interacting |= busy;
                        ui.end_row();
                    }
                });
            if touched {
                draft.dirty = true;
            }
            let popup_open = egui::Popup::is_any_open(ui.ctx());
            if draft.dirty && !interacting && !popup_open && !ui.ctx().is_using_pointer() {
                draft.dirty = false;
                panels.script_edits.write(ScriptEdit::SetProps {
                    entity,
                    plugin: binding.plugin.clone(),
                    script: binding.script.clone(),
                    props: draft.props.clone(),
                });
            } else if draft.dirty {
                ui.weak("Applies when you let go (re-hydrates the script).");
            }

            if ui.small_button("Detach").clicked() {
                panels.script_edits.write(ScriptEdit::Detach {
                    entity,
                    plugin: binding.plugin.clone(),
                    script: binding.script.clone(),
                });
            }
        });
}

/// One widget per `PropValue` variant. Returns `(changed, still_interacting)`.
fn prop_widget(ui: &mut egui::Ui, value: &mut PropValue, names: &[(u64, String)]) -> (bool, bool) {
    match value {
        PropValue::Bool(v) => {
            let r = ui.checkbox(v, "");
            (r.changed(), false)
        }
        PropValue::Int(v) => {
            let r = ui.add(egui::DragValue::new(v).speed(0.1));
            (r.changed(), r.dragged() || r.has_focus())
        }
        PropValue::Float(v) => {
            let r = ui.add(egui::DragValue::new(v).speed(0.01));
            (r.changed(), r.dragged() || r.has_focus())
        }
        PropValue::Str(v) => {
            let r = ui.add(egui::TextEdit::singleline(v).desired_width(f32::INFINITY));
            (r.changed(), r.has_focus())
        }
        PropValue::Vec3(v) => {
            let mut changed = false;
            let mut busy = false;
            ui.horizontal(|ui| {
                for axis in v.iter_mut() {
                    let r = ui.add(egui::DragValue::new(axis).speed(0.05));
                    changed |= r.changed();
                    busy |= r.dragged() || r.has_focus();
                }
            });
            (changed, busy)
        }
        PropValue::Color(v) => {
            let r = ui.color_edit_button_rgba_unmultiplied(v);
            (r.changed(), false)
        }
        PropValue::EntityRef(id) => {
            let mut changed = false;
            let mut busy = false;
            ui.horizontal(|ui| {
                let r = ui.add(egui::DragValue::new(id).speed(0.1));
                changed |= r.changed();
                busy |= r.dragged() || r.has_focus();
                match names.iter().find(|(nid, _)| nid == id) {
                    Some((_, name)) => ui.weak(name),
                    None => ui.weak("(not loaded)"),
                };
            });
            (changed, busy)
        }
    }
}

fn attach_picker(
    ui: &mut egui::Ui,
    entity: Entity,
    bindings: &[ScriptBinding],
    panels: &mut Panels,
) {
    let mut available: Vec<(String, String)> = panels
        .script_registry
        .scripts()
        .map(|s| (s.plugin.clone(), s.schema.name.to_owned()))
        .filter(|(p, s)| !bindings.iter().any(|b| &b.plugin == p && &b.script == s))
        .collect();
    available.sort();
    if available.is_empty() {
        if panels.script_registry.scripts().next().is_none() {
            ui.weak(empty_reason(panels));
        }
        return;
    }
    egui::ComboBox::from_id_salt("attach_script")
        .selected_text("Attach script…")
        .show_ui(ui, |ui| {
            for (plugin, script) in available {
                if ui.button(format!("{plugin}/{script}")).clicked() {
                    panels.script_edits.write(ScriptEdit::Attach {
                        entity,
                        plugin,
                        script,
                    });
                }
            }
        });
}

/// Why the script list is empty, phrased for the person looking at it.
///
/// The common cause is a plugin the project declares but whose dylib was never
/// built — `cargo run` builds only the host, not the plugin crates — and the
/// loader reports that as a single `warn!` that is easy to miss.
fn empty_reason(panels: &Panels) -> String {
    let Some(active) = panels.active.as_deref() else {
        return "No scripts — no active project".to_owned();
    };
    let missing: Vec<&str> = active
        .0
        .plugins
        .iter()
        .filter(|p| !panels.loaded_plugins.0.contains(p))
        .map(|p| p.as_str())
        .collect();
    if !missing.is_empty() {
        return format!(
            "No scripts — declared but not loaded: {}. Build them: cargo build --workspace",
            missing.join(", ")
        );
    }
    if active.0.plugins.is_empty() {
        return "No scripts — this project declares no plugins (Plugins menu)".to_owned();
    }
    format!(
        "No scripts — {} loaded but none export any scripts",
        active.0.plugins.join(", ")
    )
}

// ── Environment (nothing selected) ───────────────────────────────────────────

fn environment(ui: &mut egui::Ui, panels: &mut Panels) {
    ui.heading("World");
    ui.weak("Nothing selected. Click a placed entity in the viewport or the outliner.");
    ui.separator();

    let Some(tw) = panels.terrain_world.as_deref_mut() else {
        ui.weak("World not loaded yet.");
        return;
    };
    let mut env = tw.metadata.environment.clone();
    let before = env.clone();

    ui.collapsing_header_default_open("Sun", |ui| {
        let (mut azimuth, mut elevation) = sun_angles(&env);
        egui::Grid::new("sun").num_columns(2).show(ui, |ui| {
            ui.label("Azimuth");
            let a = ui.add(egui::DragValue::new(&mut azimuth).speed(1.0).suffix("°"));
            ui.end_row();
            ui.label("Elevation");
            let e = ui.add(
                egui::DragValue::new(&mut elevation)
                    .speed(0.5)
                    .range(-10.0..=90.0)
                    .suffix("°"),
            );
            ui.end_row();
            if a.changed() || e.changed() {
                set_sun_angles(&mut env, azimuth, elevation);
            }
            ui.label("Colour");
            ui.color_edit_button_rgb(&mut env.sun_color);
            ui.end_row();
            ui.label("Intensity")
                .on_hover_text("Lightmap units: radiance a surface facing the sun receives.");
            ui.add(
                egui::DragValue::new(&mut env.sun_intensity)
                    .speed(0.01)
                    .range(0.0..=20.0),
            );
            ui.end_row();
        });
    });
    ui.collapsing_header_default_open("Ambient", |ui| {
        egui::Grid::new("ambient").num_columns(2).show(ui, |ui| {
            ui.label("Colour");
            ui.color_edit_button_rgb(&mut env.ambient_color);
            ui.end_row();
            ui.label("Intensity")
                .on_hover_text("Lightmap units: sky radiance on an unoccluded surface.");
            ui.add(
                egui::DragValue::new(&mut env.ambient_intensity)
                    .speed(0.005)
                    .range(0.0..=5.0),
            );
            ui.end_row();
        });
    });

    if env != before {
        // Writing through `ResMut` is what `apply_environment` listens for.
        tw.metadata.environment = env;
        panels.lighting.stale = true;
    }

    ui.separator();
    if panels.lighting.stale {
        ui.colored_label(
            egui::Color32::from_rgb(255, 200, 80),
            "Baked lighting is out of date.",
        );
    }
    if ui.button("Bake (Shift+B)").clicked() {
        panels.actions.write(EditorAction::Bake);
    }
}

/// Direction *to* the sun as (azimuth, elevation) in degrees.
fn sun_angles(env: &EnvironmentData) -> (f32, f32) {
    let rot = Quat::from_xyzw(
        env.sun_rotation[0],
        env.sun_rotation[1],
        env.sun_rotation[2],
        env.sun_rotation[3],
    );
    let to_sun = -(rot * Vec3::NEG_Z);
    let elevation = to_sun.y.clamp(-1.0, 1.0).asin().to_degrees();
    let azimuth = to_sun.x.atan2(to_sun.z).to_degrees();
    (azimuth, elevation)
}

fn set_sun_angles(env: &mut EnvironmentData, azimuth: f32, elevation: f32) {
    let (az, el) = (azimuth.to_radians(), elevation.to_radians());
    let to_sun = Vec3::new(el.cos() * az.sin(), el.sin(), el.cos() * az.cos()).normalize();
    let rot = Quat::from_rotation_arc(Vec3::NEG_Z, -to_sun);
    env.sun_rotation = [rot.x, rot.y, rot.z, rot.w];
    // The position is cosmetic (debug cone); keep it on the sun's line at the
    // same distance so the marker follows.
    let distance = Vec3::from(env.sun_position).length().max(1.0);
    env.sun_position = (to_sun * distance).into();
}

trait CollapsingDefaultOpen {
    fn collapsing_header_default_open(&mut self, title: &str, add: impl FnOnce(&mut egui::Ui));
}

impl CollapsingDefaultOpen for egui::Ui {
    fn collapsing_header_default_open(&mut self, title: &str, add: impl FnOnce(&mut egui::Ui)) {
        egui::CollapsingHeader::new(title)
            .default_open(true)
            .show(self, add);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sun_angles_round_trip() {
        let mut env = EnvironmentData::default();
        set_sun_angles(&mut env, 35.0, 50.0);
        let (az, el) = sun_angles(&env);
        assert!((az - 35.0).abs() < 0.01, "azimuth {az}");
        assert!((el - 50.0).abs() < 0.01, "elevation {el}");
        // The stored rotation must point the sun's forward *away* from the
        // sun, which is what the baker reads as light direction.
        let rot = Quat::from_array(env.sun_rotation);
        let forward = rot * Vec3::NEG_Z;
        assert!(forward.y < 0.0, "sun shines downward");
    }
}
