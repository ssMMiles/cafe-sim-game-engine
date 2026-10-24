//! Grab (`R`) — move, rotate, and rescale an entity that is already placed.
//!
//! Placement could only ever create new entities; the only edit available to
//! an existing one was Delete. Grabbing lifts a placed entity back into the
//! placement preview, so the normal placement controls (crosshair position,
//! Shift+Scroll scale, Alt+Scroll rotate) apply to it, and drops it again
//! under its **original id** — which matters, because the id keys the entity's
//! baked lightmap files and any `PropValue::EntityRef` pointing at it.
//!
//! The carried entity does not exist in the world while held: it lives in
//! [`GrabbedEntity`] as a [`PlacedEntityRecord`], so every exit path has to
//! put it back or it is silently lost. See [`release_grab`].
//!
//! Known limitation: position follows the crosshair while carrying, so there
//! is no way to rotate an object strictly in place. A transform gizmo is the
//! eventual answer.

use bevy::prelude::*;
use engine_api::ScriptBindings;

use crate::editor::object_placement::{PlacedPicker, PlacementPreview};
use crate::game::world::objects::{
    PlacedEntityRecord, PlacedObject, PlacedObjectRegistry, PlacementConfig, spawn_entity_record,
    world_pos_to_chunk,
};
use crate::settings::UserSettings;

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
}

/// `R` grabs or releases; `Delete` destroys what is being carried.
///
/// One system rather than three, because grab/cancel share the `R` key and
/// destroy/delete share `Delete`. As separate systems their relative order
/// would be unconstrained, so a grab could be cancelled by the very press
/// that created it.
pub fn handle_grab_keys(
    mut commands: Commands,
    settings: Res<UserSettings>,
    input: Res<ButtonInput<KeyCode>>,
    asset_server: Res<AssetServer>,
    mut grabbed: ResMut<GrabbedEntity>,
    mut registry: ResMut<PlacedObjectRegistry>,
    mut config: ResMut<PlacementConfig>,
    mut picker: PlacedPicker,
    placed_query: Query<(&PlacedObject, &Transform, Option<&ScriptBindings>)>,
) {
    if !settings.edit_mode {
        return;
    }
    let shift = input.pressed(KeyCode::ShiftLeft) || input.pressed(KeyCode::ShiftRight);

    // Shift+R is the plugin hot-reload; don't eat it.
    if input.just_pressed(KeyCode::KeyR) && !shift {
        if grabbed.0.is_some() {
            restore_grabbed(
                &mut commands,
                &asset_server,
                &mut registry,
                &mut config,
                &mut grabbed,
                "Grab cancelled",
            );
        } else {
            grab_hovered(
                &mut commands,
                &mut grabbed,
                &mut registry,
                &mut config,
                &mut picker,
                &placed_query,
            );
        }
        return;
    }

    // Destroying the carried entity. `delete_hovered_object` is ordered
    // before this and bails while anything is carried, so Delete can't both
    // destroy the carried entity and delete whatever is behind it.
    if input.just_pressed(KeyCode::Delete)
        && let Some(payload) = grabbed.0.take()
    {
        info!(
            "Destroyed grabbed {} id={}",
            payload.record.kind.label(),
            payload.record.id
        );
        restore_config(&mut config, payload.previous_config);
    }
}

/// Lift the placed entity under the crosshair into the preview.
fn grab_hovered(
    commands: &mut Commands,
    grabbed: &mut GrabbedEntity,
    registry: &mut PlacedObjectRegistry,
    config: &mut PlacementConfig,
    picker: &mut PlacedPicker,
    placed_query: &Query<(&PlacedObject, &Transform, Option<&ScriptBindings>)>,
) {
    let Some(root) = picker.pick() else {
        return;
    };
    let Ok((obj, transform, bindings)) = placed_query.get(root) else {
        return;
    };

    let record = obj.to_record(transform, bindings);
    let origin_chunk = obj.chunk;

    registry.by_id.remove(&record.id);
    registry.dirty_chunks.insert(origin_chunk);
    commands.entity(root).despawn();

    // Point the preview at what we just picked up. This is a real change, so
    // `rebuild_preview_on_config_change` respawns the preview with the
    // grabbed mesh — which is exactly what we want under the crosshair.
    let previous_config = config.clone();
    config.kind = record.kind.clone();
    config.scale = Vec3::from(record.scale);
    config.rotation = Quat::from_array(record.rotation);
    // Stamp mode must not fire on drop: the entity already carries its own
    // bindings, and stamping would duplicate them.
    config.stamp_script = None;

    info!(
        "Grabbed placed {} id={} from chunk {origin_chunk:?}",
        record.kind.label(),
        record.id
    );

    grabbed.0 = Some(GrabPayload {
        record,
        origin_chunk,
        previous_config,
    });
}

/// LMB drops the carried entity at the preview's transform.
///
/// Ordered before `handle_edit_mode_click` and clears the press, so the same
/// click cannot also place a fresh entity on top of the one just dropped.
pub fn place_grabbed_object(
    mut commands: Commands,
    mut mouse_input: ResMut<ButtonInput<MouseButton>>,
    mut grabbed: ResMut<GrabbedEntity>,
    mut registry: ResMut<PlacedObjectRegistry>,
    mut config: ResMut<PlacementConfig>,
    asset_server: Res<AssetServer>,
    preview_query: Query<(&Transform, &Visibility), With<PlacementPreview>>,
) {
    if grabbed.0.is_none() || !mouse_input.just_pressed(MouseButton::Left) {
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

    mouse_input.clear_just_pressed(MouseButton::Left);

    let Some(payload) = grabbed.0.take() else {
        return;
    };
    let mut record = payload.record;

    record.position = preview_transform.translation.into();
    record.rotation = preview_transform.rotation.to_array();
    record.scale = preview_transform.scale.into();

    let chunk = world_pos_to_chunk(
        preview_transform.translation.x,
        preview_transform.translation.z,
    );
    spawn_entity_record(&mut commands, &asset_server, &mut registry, chunk, &record);
    registry.dirty_chunks.insert(chunk);
    registry.dirty_chunks.insert(payload.origin_chunk);

    info!(
        "Dropped placed {} id={} into chunk {chunk:?}",
        record.kind.label(),
        record.id
    );

    restore_config(&mut config, payload.previous_config);
}

/// Put a carried entity back when edit mode is switched off.
///
/// Without this, toggling Shift+E (or anything that clears `edit_mode`) while
/// holding an object would destroy it with no undo — the entity exists only
/// inside [`GrabbedEntity`] while carried.
pub fn release_grab_on_edit_mode_exit(
    mut commands: Commands,
    settings: Res<UserSettings>,
    mut grabbed: ResMut<GrabbedEntity>,
    mut registry: ResMut<PlacedObjectRegistry>,
    mut config: ResMut<PlacementConfig>,
    asset_server: Res<AssetServer>,
) {
    if settings.edit_mode || grabbed.0.is_none() {
        return;
    }
    restore_grabbed(
        &mut commands,
        &asset_server,
        &mut registry,
        &mut config,
        &mut grabbed,
        "Edit mode left while carrying an object",
    );
}

/// `OnExit(EditorMode::Editor)` counterpart of the above.
pub fn release_grab(
    mut commands: Commands,
    mut grabbed: ResMut<GrabbedEntity>,
    mut registry: ResMut<PlacedObjectRegistry>,
    mut config: ResMut<PlacementConfig>,
    asset_server: Res<AssetServer>,
) {
    if grabbed.0.is_none() {
        return;
    }
    restore_grabbed(
        &mut commands,
        &asset_server,
        &mut registry,
        &mut config,
        &mut grabbed,
        "Editor mode left while carrying an object",
    );
}

/// Respawn the carried entity at its original transform and chunk.
fn restore_grabbed(
    commands: &mut Commands,
    asset_server: &AssetServer,
    registry: &mut PlacedObjectRegistry,
    config: &mut PlacementConfig,
    grabbed: &mut GrabbedEntity,
    reason: &str,
) {
    let Some(payload) = grabbed.0.take() else {
        return;
    };
    spawn_entity_record(
        commands,
        asset_server,
        registry,
        payload.origin_chunk,
        &payload.record,
    );
    info!(
        "{reason}: restored {} id={} to chunk {:?}",
        payload.record.kind.label(),
        payload.record.id,
        payload.origin_chunk
    );
    restore_config(config, payload.previous_config);
}

fn restore_config(config: &mut PlacementConfig, previous: PlacementConfig) {
    *config = previous;
}
