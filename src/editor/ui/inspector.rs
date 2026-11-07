//! Inspector: properties of the selected entity, or the world environment
//! when nothing is selected.
//!
//! Live fields (name, transform, light parameters) write straight into the
//! components and report a [`PlacedEdited`]; the commit system does the chunk
//! and dirty bookkeeping. Script props are drafted locally and committed as
//! one [`ScriptEdit::SetProps`] when the user stops interacting, because a
//! commit re-hydrates the binding and resets the plugin's runtime state.

use bevy::diagnostic::{DiagnosticsStore, FrameTimeDiagnosticsPlugin};
use bevy::prelude::*;
use bevy_egui::egui;
use engine_api::{PropDef, PropValue, ScriptBinding, ScriptProp};

use super::Panels;
use crate::editor::actions::EditorAction;
use crate::editor::edits::{PlacedEdited, RespawnPlaced, ScriptEdit};
use crate::editor::history::{Change, MergeKey};
use crate::editor::prefab_edit::PrefabAction;
use crate::game::gfx::atmosphere::{AtmosphereStyle, MAX_RAY_SAMPLES};
#[cfg(not(target_arch = "wasm32"))]
use crate::game::gfx::frame_pacing::FRAME_CAP_MAX;
use crate::game::gfx::frame_pacing::{
    DisplaySettings, FrameBreakdown, FrameLimit, FrameSample, vsync_divisor,
};
use crate::game::gfx::post_process::EdgeStyle;
use crate::game::gfx::profiling::{BenchEffect, EffectBench, GpuTimingSupport};
use crate::game::gfx::system_timing::{SystemTimings, short_name};
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
    let header_info = {
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
            let before = *transform;
            if transform_fields(ui, entity, &mut transform, obj.kind.is_mesh(), state) {
                panels.edits.write(PlacedEdited::transform(entity, before));
            }
        });

        // ── Kind ─────────────────────────────────────────────────────────────────
        let kind_title = obj.kind.label();
        ui.collapsing_header_default_open(kind_title, |ui| {
            let before = obj.kind.clone();
            match kind_fields(ui, &mut obj, &panels.config) {
                KindChange::None => {}
                KindChange::InPlace => {
                    panels.edits.write(PlacedEdited::kind(entity, before));
                }
                KindChange::Respawn => {
                    // The respawn itself records nothing; the kind change is
                    // the fact to remember.
                    panels.history.push(
                        format!("Edit '{}'", obj.display_name()),
                        vec![Change::Kind {
                            id: obj.id,
                            before,
                            after: obj.kind.clone(),
                        }],
                    );
                    panels.respawns.write(RespawnPlaced::InPlace(entity));
                }
            }
        });

        (
            obj.kind.is_mesh(),
            obj.id,
            obj.prefab.as_ref().map(|i| i.name.clone()),
        )
    };
    let (is_mesh, id, instance_of) = header_info;

    // ── Prefab instance ──────────────────────────────────────────────────────
    if let Some(name) = &instance_of {
        prefab_root_section(ui, entity, name, panels);
    } else if let Some((root, root_name)) = prefab_root_of(id, panels) {
        ui.horizontal(|ui| {
            ui.label(format!("◆ Part of prefab '{root_name}'"));
            if ui.small_button("Select instance root").clicked() {
                panels.selection.primary = Some(root);
            }
        });
        ui.weak("Edits here become this instance's overrides on save.");
        ui.separator();
    }

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
    // One line only; bake state and the trigger live in the menu/status bar.
    if is_mesh {
        let dynamic = panels.dynamic_lit.get(entity).unwrap_or(false);
        ui.label(format!(
            "Lighting: {}",
            if dynamic { "Dynamic" } else { "Baked" }
        ));
    }
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
            let before = std::mem::replace(&mut obj.name, trimmed.to_owned());
            edits.write(PlacedEdited::name(info.entity, before));
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
            falloff,
            ..
        } => {
            egui::Grid::new("point_light")
                .num_columns(2)
                .show(ui, |ui| {
                    if light_common(ui, color, intensity, range, falloff) {
                        change = KindChange::InPlace;
                    }
                });
        }
        PlacedKind::SpotLight {
            color,
            intensity,
            range,
            falloff,
            inner_angle,
            outer_angle,
            ..
        } => {
            egui::Grid::new("spot_light").num_columns(2).show(ui, |ui| {
                if light_common(ui, color, intensity, range, falloff) {
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

/// Colour / intensity / range / falloff rows shared by both light kinds.
/// Colours are linear RGB on both sides (egui's `Rgba` is linear).
fn light_common(
    ui: &mut egui::Ui,
    color: &mut [f32; 3],
    intensity: &mut f32,
    range: &mut f32,
    falloff: &mut f32,
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
    ui.label("Falloff").on_hover_text(
        "How the light fades inside its range: attenuation = (1 − (d/r)²)^falloff. \
         Lower spreads it out to the range, higher pulls it in around the lamp; \
         2 is the original curve. The pool's radius at half brightness is roughly \
         r·√(1 − 0.5^(1/falloff)).",
    );
    if ui
        .add(egui::DragValue::new(falloff).speed(0.02).range(0.25..=8.0))
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
    // `id` is the stable machine identity (what the binding stores); the
    // schema's display name is presentation only, so widget ids key off `id`.
    let id = format!("{}/{}", binding.plugin, binding.script);
    let schema = panels
        .script_registry
        .get(&binding.plugin, &binding.script)
        .map(|r| r.schema);
    let title = schema.map_or(id.as_str(), |s| s.title());

    let section = egui::CollapsingHeader::new(title)
        .id_salt(&id)
        .default_open(true)
        .show(ui, |ui| {
            if schema.is_none() {
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
            egui::Grid::new(("props", &id))
                .num_columns(2)
                .show(ui, |ui| {
                    for prop in &mut draft.props {
                        let def = schema
                            .as_ref()
                            .and_then(|s| s.props.iter().find(|d| d.key == prop.key));
                        let label = ui.label(def.map_or(prop.key.as_str(), |d| d.title()));
                        if let Some(doc) = def.and_then(|d| d.doc) {
                            label.on_hover_text(doc);
                        }
                        let (changed, busy) = prop_widget(ui, &mut prop.value, names, def);
                        touched |= changed;
                        interacting |= busy;
                        ui.end_row();
                    }
                });
            if touched {
                draft.dirty = true;
            }
            let popup_open = egui::Popup::is_any_open(ui.ctx());
            if draft.dirty && !interacting && !popup_open && !ui.ctx().egui_is_using_pointer() {
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
    if schema.is_some_and(|s| s.display_name.is_some()) {
        section.header_response.on_hover_text(&id);
    }
}

/// One widget per `PropValue` variant. Returns `(changed, still_interacting)`.
/// `def` is the schema's definition when the plugin is loaded: `Int`/`Float`
/// widgets take their drag speed and bounds from it.
fn prop_widget(
    ui: &mut egui::Ui,
    value: &mut PropValue,
    names: &[(u64, String)],
    def: Option<&PropDef>,
) -> (bool, bool) {
    let speed = |fallback: f64| def.and_then(|d| d.speed).unwrap_or(fallback);
    let range = def.and_then(|d| d.range);
    match value {
        PropValue::Bool(v) => {
            let r = ui.checkbox(v, "");
            (r.changed(), false)
        }
        PropValue::Int(v) => {
            let mut w = egui::DragValue::new(v).speed(speed(0.1));
            if let Some((min, max)) = range {
                w = w.range(min as i64..=max as i64);
            }
            let r = ui.add(w);
            (r.changed(), r.dragged() || r.has_focus())
        }
        PropValue::Float(v) => {
            let mut w = egui::DragValue::new(v).speed(speed(0.01));
            if let Some((min, max)) = range {
                w = w.range(min..=max);
            }
            let r = ui.add(w);
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
    let mut available: Vec<(String, String, String)> = panels
        .script_registry
        .scripts()
        .map(|s| {
            (
                s.plugin.clone(),
                s.schema.name.to_owned(),
                s.schema.title().to_owned(),
            )
        })
        .filter(|(p, s, _)| !bindings.iter().any(|b| &b.plugin == p && &b.script == s))
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
            for (plugin, script, title) in available {
                let r = ui
                    .button(&title)
                    .on_hover_text(format!("{plugin}/{script}"));
                if r.clicked() {
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

// ── Prefab instance section ──────────────────────────────────────────────────

/// The instance root (and prefab name) a member entity belongs to, if any.
fn prefab_root_of(id: u64, panels: &Panels) -> Option<(Entity, String)> {
    panels.placed.iter().find_map(|(e, obj, _, _, _)| {
        let inst = obj.prefab.as_ref()?;
        inst.member_world_ids()
            .any(|m| m == id)
            .then(|| (e, inst.name.clone()))
    })
}

fn prefab_root_section(ui: &mut egui::Ui, entity: Entity, name: &str, panels: &mut Panels) {
    let known = panels.prefabs.get(name).is_some();
    ui.horizontal(|ui| {
        ui.label(format!("◆ Instance of prefab '{name}'"));
    });
    if !known {
        ui.colored_label(
            egui::Color32::from_rgb(255, 120, 120),
            "Prefab file missing — members not expanded. Restore the file or Unpack.",
        );
    }
    ui.horizontal(|ui| {
        if ui
            .add_enabled(known, egui::Button::new("Apply to prefab"))
            .on_hover_text("Rewrite the prefab from this instance; every instance follows.")
            .clicked()
        {
            panels.prefab_actions.write(PrefabAction::Apply(entity));
        }
        if ui
            .add_enabled(known, egui::Button::new("Revert"))
            .on_hover_text("Drop this instance's deviations and match the prefab again.")
            .clicked()
        {
            panels.prefab_actions.write(PrefabAction::Revert(entity));
        }
        if ui
            .button("Unpack")
            .on_hover_text("Sever the link: entities stay, saved as plain level data.")
            .clicked()
        {
            panels.prefab_actions.write(PrefabAction::Unpack(entity));
        }
    });
    if let Some(err) = &panels.prefab_edit.error {
        ui.colored_label(egui::Color32::from_rgb(255, 120, 120), err);
    }
    ui.separator();
}

// ── Environment (nothing selected) ───────────────────────────────────────────

fn environment(ui: &mut egui::Ui, panels: &mut Panels) {
    ui.heading("World");
    ui.weak("Nothing selected. Click a placed entity in the viewport or the outliner.");
    ui.separator();

    // Read through `&`: a `ResMut` deref marks `TerrainWorld` changed, and
    // everything that follows the environment (`apply_environment`, the sky,
    // a full probe-volume refill) keys off that. Only the write-back below
    // takes the mutable borrow.
    let Some(tw) = panels.terrain_world.as_deref() else {
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

    ui.collapsing_header_default_open("Shading", |ui| {
        ui.weak("How baked radiance becomes a displayed brightness. Costs no re-bake.");
        let mut curve = env.toon_curve();
        egui::Grid::new("toon").num_columns(2).show(ui, |ui| {
            ui.label("Bands")
                .on_hover_text("Number of discrete shading levels.");
            // A whole band per pixel of drag. `ToonCurve::sanitized` rounds
            // this, and the row is rebuilt from the sanitised value every
            // frame, so a sub-unit speed would be snapped away mid-drag and
            // the control would feel stuck.
            ui.add(
                egui::DragValue::new(&mut curve.bands)
                    .speed(1.0)
                    .fixed_decimals(0)
                    .range(1.0..=16.0),
            );
            ui.end_row();

            ui.label("Floor").on_hover_text(
                "Darkest a surface can get, as a fraction of its base colour. \
                 0 is pure black.",
            );
            ui.add(
                egui::DragValue::new(&mut curve.floor)
                    .speed(0.002)
                    .range(0.0..=1.0),
            );
            ui.end_row();

            ui.label("White").on_hover_text(
                "Exposure: the lightmap radiance that reads as fully lit. \
                 Anything brighter pins at the top band.",
            );
            ui.horizontal(|ui| {
                ui.add(
                    egui::DragValue::new(&mut curve.white)
                        .speed(0.01)
                        .range(0.01..=20.0),
                );
                let suggestion = env.suggested_white();
                if ui
                    .small_button("Auto")
                    .on_hover_text(format!(
                        "Set to sun + ambient ({suggestion:.3}), which is what an \
                         unshadowed surface facing the sun receives."
                    ))
                    .clicked()
                {
                    curve.white = suggestion;
                }
            });
            ui.end_row();

            ui.label("Gamma").on_hover_text(
                "How the bands are spread. 1 quantises radiance directly, which \
                 spends four of five bands on the brightest octave; higher values \
                 space them by perceived brightness so shade gets real bands.",
            );
            ui.add(
                egui::DragValue::new(&mut curve.gamma)
                    .speed(0.02)
                    .range(0.1..=8.0),
            );
            ui.end_row();
        });

        // The ramp is hard to judge from four numbers, and the boundaries
        // are where a world is actually won or lost: these are the lightmap
        // values each displayed level starts at.
        let curve = curve.sanitized();
        let bands = curve.bands as u32;
        ui.weak(format!(
            "levels {}",
            curve
                .levels()
                .iter()
                .map(|l| format!("{l:.2}"))
                .collect::<Vec<_>>()
                .join(" ")
        ));
        ui.weak(format!(
            "at radiance {}",
            (1..=bands)
                .map(|k| format!("{:.2}", curve.threshold(k)))
                .collect::<Vec<_>>()
                .join(" ")
        ));
        env.set_toon_curve(curve);
    });

    edges(ui, &mut panels.edge_style);
    atmosphere(ui, &mut panels.atmosphere);
    let display = primary_display(&panels.monitors);
    performance(
        ui,
        Perf {
            diagnostics: &panels.diagnostics,
            bench: panels.bench.reborrow(),
            gpu_timing: &panels.gpu_timing,
            settings: panels.display.reborrow(),
            breakdown: &panels.frame_breakdown,
            system_timings: &panels.system_timings,
            display,
        },
    );

    if env != before {
        // The curve is a display setting — the shaders read it straight from
        // the material uniform — so only a change to what the *bakers* read
        // invalidates the lightmaps. Compare against `before` with the curve
        // masked out rather than listing the bake inputs, so a field added
        // later is treated as bake-relevant until someone says otherwise.
        let bake_inputs_changed = EnvironmentData {
            toon_curve: before.toon_curve,
            ..env.clone()
        } != before;
        if bake_inputs_changed {
            panels.lighting.stale = true;
        }
        // Writing through `ResMut` is what `apply_environment` listens for.
        if let Some(tw) = panels.terrain_world.as_deref_mut() {
            tw.metadata.environment = env.clone();
        }
        panels.history.merge_or_push(
            MergeKey::Environment,
            "Edit environment",
            Change::Environment { before, after: env },
        );
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

/// The outline post-process (`EdgeStyle`). Knobs and failure modes:
/// `docs/EDGE_DETECTION.md`.
fn edges(ui: &mut egui::Ui, style: &mut ResMut<EdgeStyle>) {
    ui.collapsing_header_default_open("Edges", |ui| {
        ui.weak("Outline post-process (F2 toggles). Saved per user in editor_settings.json.");
        let mut s = **style;
        egui::Grid::new("edges").num_columns(2).show(ui, |ui| {
            ui.label("Depth step").on_hover_text(
                "Smallest jump in distance that draws a line, as 1 − near/far: \
                 0.05 means the surface behind is 5% further away. Catches \
                 silhouettes against surfaces facing the same way, like a table \
                 top over the floor seen from above.",
            );
            ui.add(
                egui::DragValue::new(&mut s.depth_threshold)
                    .speed(0.001)
                    .range(0.001..=1.0),
            );
            ui.end_row();

            ui.label("Grazing bias").on_hover_text(
                "Raises the depth threshold by the surface's own depth slope, so \
                 bumpy ground seen edge-on doesn't sprout lines. 0 = off.",
            );
            ui.add(
                egui::DragValue::new(&mut s.grazing_bias)
                    .speed(0.01)
                    .range(0.0..=10.0),
            );
            ui.end_row();

            ui.label("Crease angle").on_hover_text(
                "Smallest angle between neighbouring surface normals that draws a line.",
            );
            ui.add(
                egui::DragValue::new(&mut s.crease_angle)
                    .speed(0.5)
                    .range(1.0..=180.0)
                    .suffix("°"),
            );
            ui.end_row();

            ui.label("Softness").on_hover_text(
                "How gradually a line fades in around the thresholds. 0 is a hard \
                 cut, which pops as edges cross it.",
            );
            ui.add(
                egui::DragValue::new(&mut s.softness)
                    .speed(0.005)
                    .range(0.0..=0.99),
            );
            ui.end_row();

            ui.label("Thickness").on_hover_text(
                "Sample offset in pixels; lines are about twice this wide. Above \
                 1, things thinner than the offset can be skipped.",
            );
            ui.add(
                egui::DragValue::new(&mut s.thickness)
                    .speed(0.05)
                    .fixed_decimals(0)
                    .range(1.0..=8.0),
            );
            ui.end_row();

            ui.label("Fade")
                .on_hover_text("Lines fade out between these distances from the camera.");
            ui.horizontal(|ui| {
                ui.add(
                    egui::DragValue::new(&mut s.fade_start)
                        .speed(1.0)
                        .range(0.0..=2000.0)
                        .suffix(" m"),
                );
                ui.label("to");
                ui.add(
                    egui::DragValue::new(&mut s.fade_end)
                        .speed(1.0)
                        .range(0.0..=2000.0)
                        .suffix(" m"),
                );
            });
            ui.end_row();

            ui.label("Colour");
            let [r, g, b, a] = s.color;
            let mut rgba = egui::Rgba::from_rgba_unmultiplied(r, g, b, a);
            if egui::color_picker::color_edit_button_rgba(
                ui,
                &mut rgba,
                egui::color_picker::Alpha::OnlyBlend,
            )
            .changed()
            {
                s.color = rgba.to_rgba_unmultiplied();
            }
            ui.end_row();
        });
        if ui.button("Reset").clicked() {
            s = EdgeStyle::default();
        }
        style.set_if_neq(s);
    });
}

/// Fog and god rays (`AtmosphereStyle`). The model and the knobs:
/// `docs/ATMOSPHERE.md`.
fn atmosphere(ui: &mut egui::Ui, style: &mut ResMut<AtmosphereStyle>) {
    let mut s = **style;

    ui.collapsing_header_default_open("Fog", |ui| {
        ui.checkbox(&mut s.fog_enabled, "Enabled");
        ui.weak("Applied after shading, from the depth buffer. The sky is left alone.");
        egui::Grid::new("fog").num_columns(2).show(ui, |ui| {
            ui.label("Colour");
            ui.color_edit_button_rgb(&mut s.fog_color);
            ui.end_row();

            ui.label("Density").on_hover_text(
                "Extinction per metre. At 0.001 a surface 1 km away is almost \
                 fully fogged; at 0.01, 100 m is.",
            );
            ui.add(
                egui::DragValue::new(&mut s.fog_density)
                    .speed(0.0001)
                    .range(0.0..=1.0)
                    .custom_formatter(|v, _| format!("{v:.4}")),
            );
            ui.end_row();

            ui.label("Start").on_hover_text(
                "Distance before any fog accumulates, so nearby geometry stays crisp.",
            );
            ui.add(
                egui::DragValue::new(&mut s.fog_start)
                    .speed(0.5)
                    .range(0.0..=1000.0)
                    .suffix(" m"),
            );
            ui.end_row();

            ui.label("Max opacity").on_hover_text(
                "Ceiling on the fog, so the far distance never flattens completely.",
            );
            ui.add(
                egui::DragValue::new(&mut s.fog_max)
                    .speed(0.01)
                    .range(0.0..=1.0),
            );
            ui.end_row();

            ui.label("Height falloff").on_hover_text(
                "Height over which the density falls by 1/e, for mist that pools \
                 in valleys. 0 = the same fog at every height.",
            );
            ui.add(
                egui::DragValue::new(&mut s.fog_height_falloff)
                    .speed(0.25)
                    .range(0.0..=500.0)
                    .suffix(" m"),
            );
            ui.end_row();

            ui.add_enabled_ui(s.fog_height_falloff > 0.0, |ui| {
                ui.label("Height base")
                    .on_hover_text("World height at which the density equals the value above.");
            });
            ui.add_enabled(
                s.fog_height_falloff > 0.0,
                egui::DragValue::new(&mut s.fog_height_base)
                    .speed(0.5)
                    .suffix(" m"),
            );
            ui.end_row();

            ui.label("Sun tint").on_hover_text(
                "How much of the sun's colour the fog picks up when you look into it.",
            );
            ui.add(
                egui::DragValue::new(&mut s.fog_sun_tint)
                    .speed(0.01)
                    .range(0.0..=4.0),
            );
            ui.end_row();

            ui.label("Sun tightness")
                .on_hover_text("Higher keeps that tint in a tighter halo around the sun.");
            ui.add(
                egui::DragValue::new(&mut s.fog_sun_exponent)
                    .speed(0.1)
                    .range(0.01..=64.0),
            );
            ui.end_row();

            ui.label("Steps").on_hover_text(
                "Quantise the fog amount into this many flat steps, the way the \
                 toon curve quantises brightness. 0 leaves it smooth.",
            );
            ui.add(
                egui::DragValue::new(&mut s.fog_steps)
                    .speed(0.25)
                    .range(0..=16),
            );
            ui.end_row();
        });
    });

    ui.collapsing_header_default_open("God rays", |ui| {
        ui.checkbox(&mut s.rays_enabled, "Enabled");
        ui.weak("Light shafts marched through the depth buffer. Experimental.");
        egui::Grid::new("rays").num_columns(2).show(ui, |ui| {
            ui.label("Intensity")
                .on_hover_text("Brightness of the shafts, as a multiple of the sun's colour.");
            ui.add(
                egui::DragValue::new(&mut s.rays_intensity)
                    .speed(0.01)
                    .range(0.0..=5.0),
            );
            ui.end_row();

            ui.label("Decay").on_hover_text(
                "Falloff per sample along the march. Below ~0.9 the shafts stay short.",
            );
            ui.add(
                egui::DragValue::new(&mut s.rays_decay)
                    .speed(0.002)
                    .range(0.0..=1.0),
            );
            ui.end_row();

            ui.label("Spread").on_hover_text(
                "How far towards the sun each pixel marches, as a fraction of the distance.",
            );
            ui.add(
                egui::DragValue::new(&mut s.rays_spread)
                    .speed(0.01)
                    .range(0.0..=2.0),
            );
            ui.end_row();

            ui.label("Samples").on_hover_text(
                "Depth taps per pixel — the whole cost of the effect. Halving it \
                 halves the pass; watch the GPU timings below.",
            );
            ui.add(
                egui::DragValue::new(&mut s.rays_samples)
                    .speed(0.5)
                    .range(1..=MAX_RAY_SAMPLES),
            );
            ui.end_row();
        });
    });

    if ui.button("Reset atmosphere").clicked() {
        s = AtmosphereStyle::default();
    }
    style.set_if_neq(s);
}

/// Frame cost: a frame-time trace, an A/B measurement of one effect, and
/// per-pass GPU times where the adapter can produce them.
///
/// `docs/ATMOSPHERE.md` §7 explains which of these to trust when.
/// What the Performance section reads and edits. Gathered at the call site
/// because `environment` already holds a mutable borrow of another
/// `Panels` field, so the panel can't take `Panels` whole.
struct Perf<'a> {
    diagnostics: &'a DiagnosticsStore,
    // `Mut` rather than `&mut T`, so an untouched setting isn't marked
    // changed just by drawing the panel.
    bench: Mut<'a, EffectBench>,
    gpu_timing: &'a GpuTimingSupport,
    settings: Mut<'a, DisplaySettings>,
    breakdown: &'a FrameBreakdown,
    system_timings: &'a SystemTimings,
    display: Option<Display>,
}

/// The display the editor is most likely on, as the OS reports it.
struct Display {
    name: Option<String>,
    refresh_hz: Option<f32>,
}

/// The primary monitor (bevy can't say which monitor a window is on, and on
/// a laptop the built-in panel is the primary).
fn primary_display(
    monitors: &Query<(&bevy::window::Monitor, Has<bevy::window::PrimaryMonitor>)>,
) -> Option<Display> {
    let (monitor, _) = monitors
        .iter()
        .find(|(_, primary)| *primary)
        .or_else(|| monitors.iter().next())?;
    Some(Display {
        name: monitor.name.clone(),
        refresh_hz: monitor
            .refresh_rate_millihertz
            .filter(|mhz| *mhz > 0)
            .map(|mhz| mhz as f32 / 1000.0),
    })
}

fn performance(ui: &mut egui::Ui, mut p: Perf) {
    egui::CollapsingHeader::new("Performance")
        .default_open(false)
        .show(ui, |ui| {
            frame_time_chart(ui, p.diagnostics);
            ui.add_space(4.0);
            let refresh = p.display.as_ref().and_then(|d| d.refresh_hz);
            frame_breakdown(ui, p.breakdown, refresh, &p.settings);
            ui.add_space(4.0);
            system_times(ui, p.system_timings);
            ui.add_space(4.0);
            display_controls(ui, &mut p.settings, p.display.as_ref());
            ui.add_space(4.0);
            let vsync = p.settings.vsync;
            ab_measurement(ui, &mut p.bench, vsync);
            ui.add_space(4.0);
            gpu_timings(ui, p.diagnostics, p.gpu_timing);
        });
}

/// Where the frame goes: one bar per thread, each the width of the frame.
///
/// The threads run side by side (bevy renders frame N while the game builds
/// frame N+1), so they aren't stacked — each bar answers "how much of the
/// frame was this thread busy?" and the render bar splits off the time spent
/// waiting for a swapchain image, which is where vsync and a GPU bottleneck
/// both land.
fn frame_breakdown(
    ui: &mut egui::Ui,
    breakdown: &FrameBreakdown,
    refresh_hz: Option<f32>,
    settings: &DisplaySettings,
) {
    let Some(m) = breakdown.median() else {
        return;
    };
    let frame = m.frame.max(1e-3);

    let busy = egui::Color32::from_rgb(120, 180, 240);
    let waiting = egui::Color32::from_rgb(110, 110, 120);

    let bar = |ui: &mut egui::Ui, label: &str, parts: &[(f32, egui::Color32)], text: String| {
        ui.horizontal(|ui| {
            ui.add_sized([86.0, 14.0], egui::Label::new(label));
            let width = (ui.available_width() - 4.0).max(40.0);
            let (rect, _) = ui.allocate_exact_size(egui::vec2(width, 12.0), egui::Sense::hover());
            let painter = ui.painter_at(rect);
            painter.rect_filled(rect, 2.0, ui.visuals().extreme_bg_color);
            let mut x = rect.left();
            for (ms, colour) in parts {
                let w = rect.width() * (ms / frame).clamp(0.0, 1.0);
                let segment = egui::Rect::from_min_size(
                    egui::pos2(x, rect.top()),
                    egui::vec2(w.min(rect.right() - x), rect.height()),
                );
                painter.rect_filled(segment, 2.0, *colour);
                x += w;
            }
        })
        .response
        .on_hover_text(text);
    };

    bar(
        ui,
        "Main thread",
        &[(m.main, busy)],
        format!("{:.2} ms busy: game + editor systems", m.main),
    );
    bar(
        ui,
        "Render thread",
        &[(m.render_busy, busy), (m.render_wait, waiting)],
        format!(
            "{:.2} ms busy building GPU work · {:.2} ms waiting for a swapchain image",
            m.render_busy, m.render_wait
        ),
    );
    ui.weak(format!(
        "main {:.1} · render {:.1} + {:.1} waiting · of {:.1} ms",
        m.main, m.render_busy, m.render_wait, m.frame
    ));
    ui.label(verdict(&m, refresh_hz, settings));
}

/// One sentence on what is holding the frame rate down.
fn verdict(m: &FrameSample, refresh_hz: Option<f32>, settings: &DisplaySettings) -> String {
    let divisor = refresh_hz.and_then(|hz| vsync_divisor(m.frame, hz));
    match m.limit() {
        FrameLimit::Presentation => match (divisor.zip(refresh_hz), settings.vsync) {
            (Some((k, hz)), _) => format!(
                "Running at 1/{k} of the display's {hz:.0} Hz: each frame takes longer than \
                 one refresh ({:.1} ms), so presentation waits for the next. With both threads \
                 mostly idle that points at the GPU — or at the display actually running at \
                 {:.0} Hz (on a MacBook, Low Power Mode caps it).",
                1000.0 / hz,
                hz / k as f32
            ),
            (None, true) => {
                "Waiting for the display: vsync holds the frame to the refresh rate. That's \
                 idle time, not cost."
                    .into()
            }
            (None, false) => {
                "Waiting for a swapchain image with vsync off: the GPU is the limit, or — in \
                 a window on macOS — the compositor, which paces to the display regardless."
                    .into()
            }
        },
        FrameLimit::MainThread => "CPU-bound on the main thread (game + editor systems).".into(),
        FrameLimit::RenderThread => {
            "CPU-bound on the render thread (extracting and building GPU commands).".into()
        }
        FrameLimit::Headroom if settings.frame_cap > 0 => {
            format!("Held by the {} fps frame cap.", settings.frame_cap)
        }
        FrameLimit::Headroom => "Neither thread is near the frame time.".into(),
    }
}

/// The most expensive bevy systems, by smoothed CPU self time per frame,
/// across both threads. Fed by the tracing layer in `gfx/system_timing.rs`;
/// bevy only emits the per-system spans in `trace` builds, so outside
/// `cargo run-profiling` this is a hint instead of a table.
///
/// Systems that run whole schedules (`run_main`, `run_render_schedule`, …)
/// are listed apart: their self time is them waiting for systems on other
/// threads, which already have rows of their own.
fn system_times(ui: &mut egui::Ui, timings: &SystemTimings) {
    const ROWS: usize = 20;
    egui::CollapsingHeader::new("Systems (CPU)")
        .default_open(false)
        .show(ui, |ui| {
            if timings.is_empty() {
                ui.weak(
                    "Per-system times need bevy's span instrumentation, which only the \
                     profiling build carries: quit and run `cargo run-profiling`.",
                );
                return;
            }
            ui.weak(format!(
                "{:.2} ms of system work per frame, main + render threads together \
                 (smoothed self time; threads overlap, so this can exceed the frame).",
                timings.total_ms()
            ));
            egui::Grid::new("system_times")
                .striped(true)
                .min_col_width(0.0)
                .show(ui, |ui| {
                    let mut rest = 0.0;
                    let mut hidden = 0;
                    for (i, (name, stat)) in timings.rows().filter(|(_, s)| !s.nests).enumerate() {
                        if i >= ROWS {
                            rest += stat.ms;
                            hidden += 1;
                            continue;
                        }
                        ui.label(short_name(name)).on_hover_text(name);
                        ui.monospace(format!("{:7.3} ms", stat.ms));
                        // Runs per frame: FixedUpdate systems run 0–n times,
                        // render systems exactly once, so only flag the odd ones.
                        if (stat.runs - 1.0).abs() > 0.1 {
                            ui.monospace(format!("×{:.1}", stat.runs));
                        } else {
                            ui.label("");
                        }
                        ui.end_row();
                    }
                    if hidden > 0 {
                        ui.weak(format!("{hidden} more"));
                        ui.monospace(format!("{rest:7.3} ms"));
                        ui.end_row();
                    }
                });
            let runners: Vec<_> = timings.rows().filter(|(_, s)| s.nests).collect();
            if !runners.is_empty() {
                ui.add_space(2.0);
                ui.weak("Schedule runners (time not spent in their systems; not in the total):")
                    .on_hover_text(
                        "These run whole schedules — run_render_schedule is the entire render \
                         frame. Their systems are the rows above; what is left here is mostly \
                         waiting for systems on other threads.",
                    );
                for (name, stat) in runners {
                    ui.weak(format!("{:7.3} ms  {}", stat.ms, short_name(name)))
                        .on_hover_text(name);
                }
            }
        });
}

/// Vsync and the frame cap. Both persist per user.
fn display_controls(
    ui: &mut egui::Ui,
    settings: &mut Mut<DisplaySettings>,
    display: Option<&Display>,
) {
    match display.and_then(|d| d.refresh_hz.map(|hz| (hz, d.name.as_deref()))) {
        Some((hz, name)) => ui.weak(format!(
            "Display: {hz:.0} Hz{} (as the OS reports it)",
            name.map(|n| format!(" · {n}")).unwrap_or_default()
        )),
        None => ui.weak("Display refresh rate: not reported."),
    };

    #[cfg(not(target_arch = "wasm32"))]
    {
        let mut s = **settings;
        ui.checkbox(&mut s.vsync, "Vsync").on_hover_text(
            "On: present in step with the display. Off: as fast as possible — what you \
             want for measuring, since vsync turns any cost below a refresh into idle time.",
        );
        ui.horizontal(|ui| {
            let mut capped = s.frame_cap > 0;
            if ui
                .checkbox(&mut capped, "Frame cap")
                .on_hover_text(
                    "Hold the main loop to a frame rate — saves battery with vsync off. \
                     Sleeps after the frame's work is measured, so it never reads as cost.",
                )
                .changed()
            {
                s.frame_cap = if capped {
                    display
                        .and_then(|d| d.refresh_hz)
                        .map_or(60, |hz| hz.round() as u32)
                } else {
                    0
                };
            }
            // Only built while capped: `DragValue` clamps the *existing*
            // value into its range every frame, even disabled, so showing it
            // over the 0 that means "off" would silently become a 1 fps cap.
            if capped {
                ui.add(
                    egui::DragValue::new(&mut s.frame_cap)
                        .speed(1.0)
                        .range(1..=FRAME_CAP_MAX)
                        .suffix(" fps"),
                );
            } else {
                ui.weak("off");
            }
        });
        settings.set_if_neq(s);
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = settings;
        ui.weak("The browser paces frames to the display; vsync and the cap are native-only.");
    }
}

/// The last ~120 frames as a filled trace, with the median marked.
///
/// A chart rather than a number because the thing you need to know about
/// frame time is its *shape*: a steady 9 ms and a 6 ms average with a 40 ms
/// spike every second feel nothing alike, and read identically as a mean.
fn frame_time_chart(ui: &mut egui::Ui, diagnostics: &DiagnosticsStore) {
    let Some(diagnostic) = diagnostics.get(&FrameTimeDiagnosticsPlugin::FRAME_TIME) else {
        ui.weak("No frame-time diagnostic.");
        return;
    };
    let samples: Vec<f32> = diagnostic.values().map(|v| *v as f32).collect();
    if samples.len() < 2 {
        ui.weak("Collecting frames…");
        return;
    }

    let peak = samples.iter().copied().fold(f32::MIN, f32::max);
    let mut sorted = samples.clone();
    sorted.sort_by(f32::total_cmp);
    let median = sorted[sorted.len() / 2];
    // Headroom so the trace never touches the top of its box.
    let top = (peak * 1.15).max(1.0);

    let (rect, _) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 52.0), egui::Sense::hover());
    let painter = ui.painter_at(rect);
    let visuals = ui.visuals();
    painter.rect_filled(rect, 2.0, visuals.extreme_bg_color);

    let x_of =
        |i: usize| rect.left() + rect.width() * (i as f32 / (samples.len() - 1).max(1) as f32);
    let y_of = |ms: f32| rect.bottom() - rect.height() * (ms / top).clamp(0.0, 1.0);

    // Median line first, so the trace draws over it.
    let median_y = y_of(median);
    painter.line_segment(
        [
            egui::pos2(rect.left(), median_y),
            egui::pos2(rect.right(), median_y),
        ],
        egui::Stroke::new(1.0, visuals.weak_text_color().gamma_multiply(0.6)),
    );
    painter.add(egui::Shape::line(
        samples
            .iter()
            .enumerate()
            .map(|(i, ms)| egui::pos2(x_of(i), y_of(*ms)))
            .collect(),
        egui::Stroke::new(1.0, visuals.text_color()),
    ));

    ui.horizontal(|ui| {
        ui.monospace(format!("{median:.2} ms median"));
        ui.weak(format!(
            "· peak {peak:.1} · {:.0} fps",
            1000.0 / median.max(1e-3)
        ));
    });
}

/// Switch one effect off and on for a few hundred frames and compare the
/// frame times. Works without GPU timers, which is the point.
fn ab_measurement(ui: &mut egui::Ui, bench: &mut Mut<EffectBench>, vsync: bool) {
    if let Some(run) = bench.running() {
        let effect = run.effect();
        let progress = run.progress();
        ui.horizontal(|ui| {
            ui.label(format!("Measuring {}…", effect.label()));
            if ui.small_button("Cancel").clicked() {
                bench.cancel();
            }
        });
        ui.add(egui::ProgressBar::new(progress).desired_height(6.0));
        ui.weak("Hold the camera still and don't resize the viewport.");
        return;
    }

    if vsync {
        ui.colored_label(
            egui::Color32::from_rgb(255, 200, 80),
            "Vsync is on: frame time is pinned to the display, so an A/B only sees a \
             cost big enough to drop a whole refresh. Turn vsync off first.",
        );
    }
    ui.horizontal(|ui| {
        ui.label("A/B measure").on_hover_text(
            "Switches the effect off and on for a few hundred frames and \
             compares frame times. Only sees a cost while the frame is \
             GPU-bound, so it reports the noise alongside the result.",
        );
        for effect in BenchEffect::ALL {
            if ui.small_button(effect.label()).clicked() {
                bench.start(effect);
            }
        }
    });

    let Some(result) = bench.last else {
        return;
    };
    let delta = result.delta_ms();
    if result.significant() {
        let share = if result.with_ms > 1e-3 {
            100.0 * delta / result.with_ms
        } else {
            0.0
        };
        ui.colored_label(
            egui::Color32::from_rgb(140, 200, 255),
            format!(
                "{}: {delta:+.2} ms/frame ({share:+.0}% of {:.2} ms)",
                result.effect.label(),
                result.with_ms
            ),
        );
    } else {
        ui.weak(format!(
            "{}: no measurable cost ({delta:+.2} ms, inside the noise)",
            result.effect.label()
        ));
    }
    ui.weak(format!(
        "on {:.2} ±{:.2} · off {:.2} ±{:.2} · {} frames each",
        result.with_ms,
        result.with_spread,
        result.without_ms,
        result.without_spread,
        result.samples
    ));
}

/// Per-pass GPU time from bevy's render diagnostics, where the adapter can
/// produce it. Note a number, toggle the effect, look again.
fn gpu_timings(ui: &mut egui::Ui, diagnostics: &DiagnosticsStore, support: &GpuTimingSupport) {
    ui.separator();
    ui.weak(support.explain());

    // `render/<pass>/elapsed_gpu`, as recorded by `pass_span`.
    let mut rows: Vec<(String, f64, bool)> = diagnostics
        .iter()
        .filter_map(|d| {
            let path = d.path().as_str();
            let rest = path.strip_prefix("render/")?;
            let (name, field) = rest.rsplit_once('/')?;
            let gpu = match field {
                "elapsed_gpu" => true,
                "elapsed_cpu" => false,
                _ => return None,
            };
            Some((name.replace('/', " › "), d.smoothed()?, gpu))
        })
        .collect();

    if rows.is_empty() {
        return;
    }

    // A pass that reports GPU time has a CPU row too; the GPU one is what
    // anyone pricing an effect wants, so drop the duplicate.
    let timed_on_gpu: std::collections::HashSet<String> = rows
        .iter()
        .filter(|(_, _, gpu)| *gpu)
        .map(|(name, _, _)| name.clone())
        .collect();
    rows.retain(|(name, _, gpu)| *gpu || !timed_on_gpu.contains(name));
    rows.sort_by(|a, b| b.1.total_cmp(&a.1));

    if rows.iter().any(|(_, _, gpu)| !gpu) {
        ui.weak("ᶜ = CPU encode time, not GPU time.");
    }
    egui::Grid::new("gpu_timings")
        .num_columns(2)
        .striped(true)
        .show(ui, |ui| {
            let total: f64 = rows
                .iter()
                .filter(|(_, _, gpu)| *gpu)
                .map(|(_, v, _)| v)
                .sum();
            for (name, value, gpu) in &rows {
                ui.label(if *gpu {
                    name.clone()
                } else {
                    format!("{name} ᶜ")
                });
                ui.monospace(format!("{value:.3}"));
                ui.end_row();
            }
            if total > 0.0 {
                ui.strong("total (GPU)");
                ui.monospace(format!("{total:.3}"));
                ui.end_row();
            }
        });
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
