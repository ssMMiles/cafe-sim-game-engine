//! Grab (`R`) — move, rotate, and rescale an entity that is already placed.
//!
//! Grabbing lifts a placed entity back into the placement preview, so the
//! normal placement controls (cursor position, Shift+Scroll scale, Alt+Scroll
//! rotate) apply to it, and drops it again under its **original id** — which
//! matters, because the id keys the entity's baked lightmap files and any
//! `PropValue::EntityRef` pointing at it.
//!
//! `R` works from the Select tool too: it switches to Place for the duration
//! of the carry and switches back on drop or cancel.
//!
//! The carried entity does not exist in the world while held: it lives in
//! [`GrabbedEntity`] as a [`PlacedEntityRecord`], so every exit path has to
//! put it back or it is silently lost. See [`release_grab`].
//!
//! Known limitation: position follows the cursor while carrying, so there is
//! no way to rotate an object strictly in place. The inspector's transform
//! fields (Phase 3) and later a gizmo are the answer.

use bevy::prelude::*;
use engine_api::ScriptBindings;

use crate::editor::history::{Change, History};
use crate::editor::object_placement::PlacementPreview;
use crate::editor::picking::{PlacedPicker, ViewportInput};
use crate::editor::selection::Selection;
use crate::editor::tool::EditorTool;
use crate::game::world::objects::{
    PlacedEntityRecord, PlacedObject, PlacedObjectRegistry, PlacementConfig,
    refresh_instance_overrides, spawn_entity_record, world_pos_to_chunk,
};
use crate::game::world::prefabs::PrefabLibrary;

/// The entity currently held by the editor, if any.
#[derive(Resource, Default)]
pub struct GrabbedEntity(pub Option<GrabPayload>);

pub struct GrabPayload {
    /// Full state of the lifted entity, including its id, name, and script
    /// bindings with their authored prop values.
    record: PlacedEntityRecord,
    /// Chunk it came from. Dirtied on grab (it lost an entity) and used to
    /// restore it on cancel.
    origin_chunk: (i32, i32),
    /// Placement settings to put back when the grab ends, so picking up a
    /// rock doesn't silently change what the next fresh placement will be.
    previous_config: PlacementConfig,
    /// Tool to return to when the grab ends.
    previous_tool: EditorTool,
}

/// `R` grabs or releases; `Delete` destroys what is being carried.
///
/// One system rather than three, because grab/cancel share the `R` key and
/// destroy/delete share `Delete`. As separate systems their relative order
/// would be unconstrained, so a grab could be cancelled by the very press
/// that created it.
pub fn handle_grab_keys(
    mut commands: Commands,
    input: Res<ButtonInput<KeyCode>>,
    asset_server: Res<AssetServer>,
    tool: Res<State<EditorTool>>,
    mut next_tool: ResMut<NextState<EditorTool>>,
    mut grabbed: ResMut<GrabbedEntity>,
    mut registry: ResMut<PlacedObjectRegistry>,
    mut config: ResMut<PlacementConfig>,
    mut selection: ResMut<Selection>,
    mut history: ResMut<History>,
    prefabs: Res<PrefabLibrary>,
    mut picker: PlacedPicker,
    placed_query: Query<(&PlacedObject, &Transform, Option<&ScriptBindings>)>,
) {
    let shift = input.pressed(KeyCode::ShiftLeft) || input.pressed(KeyCode::ShiftRight);

    // Shift+R is the plugin hot-reload; don't eat it.
    if input.just_pressed(KeyCode::KeyR) && !shift {
        if grabbed.0.is_some() {
            restore_grabbed(
                &mut commands,
                &asset_server,
                &mut registry,
                &prefabs,
                &mut config,
                Some(&mut next_tool),
                &mut selection,
                &mut grabbed,
                "Grab cancelled",
            );
        } else {
            // Prefer what the cursor is on; fall back to the selection so `R`
            // also works right after picking something in the outliner.
            let Some(root) = picker.pick().or(selection.primary) else {
                return;
            };
            grab_entity(
                root,
                &mut commands,
                &mut grabbed,
                &mut registry,
                &prefabs,
                &mut config,
                *tool.get(),
                &mut next_tool,
                &placed_query,
            );
        }
        return;
    }

    // Destroying the carried entity. `delete_selected` bails while anything is
    // carried, so Delete can't both destroy the carried entity and delete the
    // selection.
    if input.just_pressed(KeyCode::Delete)
        && let Some(payload) = grabbed.0.take()
    {
        info!(
            "Destroyed grabbed {} id={}",
            payload.record.kind.label(),
            payload.record.id
        );
        history.push(
            format!("Delete '{}'", record_display_name(&payload.record)),
            vec![Change::Despawn {
                chunk: payload.origin_chunk,
                record: payload.record,
            }],
        );
        finish_grab(
            &mut config,
            Some(&mut next_tool),
            payload.previous_config,
            payload.previous_tool,
        );
    }
}

/// Lift a placed entity into the preview.
fn grab_entity(
    root: Entity,
    commands: &mut Commands,
    grabbed: &mut GrabbedEntity,
    registry: &mut PlacedObjectRegistry,
    prefabs: &PrefabLibrary,
    config: &mut PlacementConfig,
    current_tool: EditorTool,
    next_tool: &mut NextState<EditorTool>,
    placed_query: &Query<(&PlacedObject, &Transform, Option<&ScriptBindings>)>,
) {
    let Ok((obj, transform, bindings)) = placed_query.get(root) else {
        return;
    };
    if obj.kind.is_group() {
        info!("Groups move through the inspector's transform fields, not grab");
        return;
    }

    let mut record = obj.to_record(transform, bindings);
    // A single-entity instance root rides along with fresh overrides so its
    // re-expansion on drop keeps any unsaved edits.
    refresh_instance_overrides(&mut record, prefabs, registry, placed_query);
    let origin_chunk = obj.chunk;

    registry.by_id.remove(&record.id);
    registry.dirty_chunks.insert(origin_chunk);
    commands.entity(root).despawn();

    // Point the preview at what we just picked up. This is a real change, so
    // `rebuild_preview_on_config_change` respawns the preview with the
    // grabbed mesh — which is exactly what we want under the cursor.
    let previous_config = config.clone();
    config.kind = record.kind.clone();
    config.prefab = None;
    config.scale = Vec3::from(record.scale);
    config.rotation = Quat::from_array(record.rotation);
    // Stamp mode must not fire on drop: the entity already carries its own
    // bindings, and stamping would duplicate them.
    config.stamp_script = None;

    if current_tool != EditorTool::Place {
        next_tool.set(EditorTool::Place);
    }

    info!(
        "Grabbed placed {} id={} from chunk {origin_chunk:?}",
        record.kind.label(),
        record.id
    );

    grabbed.0 = Some(GrabPayload {
        record,
        origin_chunk,
        previous_config,
        previous_tool: current_tool,
    });
}

/// LMB drops the carried entity at the preview's transform.
///
/// Ordered before `handle_place_click` and clears the click, so the same
/// click cannot also place a fresh entity on top of the one just dropped.
pub fn place_grabbed_object(
    mut commands: Commands,
    mut viewport: ResMut<ViewportInput>,
    mut grabbed: ResMut<GrabbedEntity>,
    mut registry: ResMut<PlacedObjectRegistry>,
    mut config: ResMut<PlacementConfig>,
    mut next_tool: ResMut<NextState<EditorTool>>,
    mut selection: ResMut<Selection>,
    mut history: ResMut<History>,
    prefabs: Res<PrefabLibrary>,
    asset_server: Res<AssetServer>,
    preview_query: Query<(&Transform, &Visibility), With<PlacementPreview>>,
) {
    if grabbed.0.is_none() || viewport.clicked_px.is_none() {
        return;
    }

    // Same rule as fresh placement: the preview is the only source of truth
    // for where a click lands, and a hidden preview means nowhere valid.
    let Some((preview_transform, vis)) = preview_query.iter().next() else {
        return;
    };
    if *vis == Visibility::Hidden {
        return;
    }

    viewport.clicked_px = None;

    let Some(payload) = grabbed.0.take() else {
        return;
    };
    let mut record = payload.record;
    let before = record.transform();

    record.position = preview_transform.translation.into();
    record.rotation = preview_transform.rotation.to_array();
    record.scale = preview_transform.scale.into();

    let chunk = world_pos_to_chunk(
        preview_transform.translation.x,
        preview_transform.translation.z,
    );
    let entity = spawn_entity_record(
        &mut commands,
        &asset_server,
        &mut registry,
        &prefabs,
        chunk,
        &record,
    );
    registry.dirty_chunks.insert(chunk);
    registry.dirty_chunks.insert(payload.origin_chunk);
    selection.primary = Some(entity);
    // The id survived the carry, so this is a move; undo re-derives the
    // chunk from the restored transform.
    history.push(
        format!("Move '{}'", record_display_name(&record)),
        vec![Change::Transform {
            id: record.id,
            before,
            after: *preview_transform,
        }],
    );

    info!(
        "Dropped placed {} id={} into chunk {chunk:?}",
        record.kind.label(),
        record.id
    );

    finish_grab(
        &mut config,
        Some(&mut next_tool),
        payload.previous_config,
        payload.previous_tool,
    );
}

/// Put a carried entity back. `OnExit(EditorTool::Place)` and
/// `OnExit(EditorMode::Editor)`: without this, switching tool or mode while
/// holding an object would destroy it with no undo — the entity exists only
/// inside [`GrabbedEntity`] while carried.
///
/// This runs *because* the tool or mode changed, so it must not fight that
/// change by restoring the previous tool — hence no `NextState` here.
pub fn release_grab(
    mut commands: Commands,
    mut grabbed: ResMut<GrabbedEntity>,
    mut registry: ResMut<PlacedObjectRegistry>,
    mut config: ResMut<PlacementConfig>,
    mut selection: ResMut<Selection>,
    prefabs: Res<PrefabLibrary>,
    asset_server: Res<AssetServer>,
) {
    if grabbed.0.is_none() {
        return;
    }
    restore_grabbed(
        &mut commands,
        &asset_server,
        &mut registry,
        &prefabs,
        &mut config,
        None,
        &mut selection,
        &mut grabbed,
        "Place tool left while carrying an object",
    );
}

/// Respawn the carried entity at its original transform and chunk.
fn restore_grabbed(
    commands: &mut Commands,
    asset_server: &AssetServer,
    registry: &mut PlacedObjectRegistry,
    prefabs: &PrefabLibrary,
    config: &mut PlacementConfig,
    next_tool: Option<&mut NextState<EditorTool>>,
    selection: &mut Selection,
    grabbed: &mut GrabbedEntity,
    reason: &str,
) {
    let Some(payload) = grabbed.0.take() else {
        return;
    };
    let entity = spawn_entity_record(
        commands,
        asset_server,
        registry,
        prefabs,
        payload.origin_chunk,
        &payload.record,
    );
    selection.primary = Some(entity);
    info!(
        "{reason}: restored {} id={} to chunk {:?}",
        payload.record.kind.label(),
        payload.record.id,
        payload.origin_chunk
    );
    finish_grab(
        config,
        next_tool,
        payload.previous_config,
        payload.previous_tool,
    );
}

fn record_display_name(record: &PlacedEntityRecord) -> String {
    if record.name.is_empty() {
        record.kind.label().to_owned()
    } else {
        record.name.clone()
    }
}

/// `next_tool` is `None` when the grab is ending because the tool already
/// changed underneath it.
fn finish_grab(
    config: &mut PlacementConfig,
    next_tool: Option<&mut NextState<EditorTool>>,
    previous_config: PlacementConfig,
    previous_tool: EditorTool,
) {
    *config = previous_config;
    if let Some(next_tool) = next_tool
        && previous_tool != EditorTool::Place
    {
        next_tool.set(previous_tool);
    }
}
