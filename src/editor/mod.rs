//! `EditorPlugin` — owns every system that exists for authoring rather than
//! gameplay. Toggle between editor and gameplay with **F1**.
//!
//! The editor is a docked window (`ui`): a viewport tab showing the camera's
//! render texture, with an outliner, inspector, place and terrain panels
//! around it and a menu bar on top. The camera is flown with a right-button
//! drag (`camera`), tools pick with a ray through the cursor (`picking`), the
//! current [`tool::EditorTool`] decides what a click does, and every change to
//! level data funnels through `edits`. See `docs/EDITOR_UI_DESIGN.md`.
//!
//! Default mode is [`EditorMode::Editor`]. While in [`EditorMode::Gameplay`]
//! everything in this plugin is suspended, the render texture is presented
//! full-window again, and the active plugins own cursor and input.

use bevy::prelude::*;

use crate::{
    AppState,
    game::{SimSet, SimulationState},
};

mod actions;
mod baking_trigger;
mod camera;
mod catalog;
mod controls;
mod edits;
mod gizmo;
mod grab;
pub mod history;
mod input;
mod lightmap_toggle;
mod object_placement;
mod physics_debug;
mod picking;
mod prefab_edit;
mod previews;
mod selection;
mod terrain_edit;
mod terrain_save;
mod thumbnails;
mod tool;
mod ui;

use actions::{EditorAction, apply_level_actions, apply_selection_actions};
use baking_trigger::trigger_bake;
use camera::{
    EditorCameraSettings, FlyLock, editor_fly_camera, free_cursor_for_editor, init_editor_camera,
    release_fly_lock, restore_render_target, sync_look_cursor, sync_render_target,
};
use catalog::{
    AssetCatalog, AssetManifestAsset, AssetManifestLoader, populate_asset_catalog,
    request_asset_manifest,
};
use edits::{
    LightingState, PlacedEdited, RespawnPlaced, ScriptEdit, apply_script_edits,
    clear_lighting_stale_on_bake, commit_placed_edits, respawn_placed,
};
use gizmo::{
    EditorGizmoGroup, GizmoInteraction, configure_editor_gizmos, draw_transform_gizmo,
    gizmo_pointer_free, reset_gizmo, update_transform_gizmo,
};
use grab::{GrabbedEntity, handle_grab_keys, place_grabbed_object, release_grab};
use history::{History, apply_undo_redo, prepare_history, settle_history, undo_redo_hotkeys};
use input::keyboard_free;
use lightmap_toggle::{sync_lightmap_mode, toggle_lightmap_mode};
use object_placement::{
    PlacementPreviewEntity, adjust_placement_rotation, adjust_placement_scale, draw_preview_gizmo,
    handle_place_click, rebuild_preview_on_config_change, setup_placement_preview,
    teardown_placement_preview, update_placement_preview,
};
use physics_debug::{sync_physics_debug, toggle_physics_debug};
use picking::{EditorRay, ViewportInput, compute_editor_ray, viewport_pointer_free};
use prefab_edit::{PrefabAction, PrefabEditState, apply_prefab_actions};
use selection::{
    Selection, clear_hover, delete_selected, draw_selection_gizmos, focus_selection,
    select_on_click, update_hover, validate_selection,
};
use terrain_edit::{
    BrushSettings, TerrainBrushHit, adjust_brush_size, apply_terrain_brush,
    finalize_terrain_stroke, raycast_terrain_hit, teardown_terrain_brush_preview,
    update_brush_preview,
};
use terrain_save::trigger_save_terrain;
use tool::EditorTool;

#[derive(States, Default, Debug, Clone, PartialEq, Eq, Hash)]
pub enum EditorMode {
    #[default]
    Editor,
    Gameplay,
}

#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub enum EditorSet {
    /// `AppState::InGame` + `EditorMode::Editor`, before everything else:
    /// derives this frame's cursor ray and validates the selection.
    Prepare,
    /// `AppState::InGame` + `EditorMode::Editor`. Panel-driven edits, tool
    /// switches, toggles — runs while paused too.
    InGame,
    /// `AppState::InGame` + `SimulationState::Running` + `EditorMode::Editor`.
    /// Placement, sculpt, camera — input-driven editing only.
    Active,
}

pub struct EditorPlugin;

impl Plugin for EditorPlugin {
    fn build(&self, app: &mut App) {
        app.init_state::<EditorMode>();
        app.add_plugins((tool::ToolPlugin, ui::EditorUiPlugin));

        let in_editor = in_state(AppState::InGame).and_then(in_state(EditorMode::Editor));
        app.configure_sets(Update, EditorSet::Prepare.run_if(in_editor.clone()));
        app.configure_sets(
            Update,
            EditorSet::InGame
                .run_if(in_editor.clone())
                .after(EditorSet::Prepare),
        );
        app.configure_sets(
            Update,
            EditorSet::Active
                .run_if(in_editor.and_then(in_state(SimulationState::Running)))
                .after(EditorSet::Prepare),
        );

        let place = in_state(EditorTool::Place);
        let terrain = in_state(EditorTool::Terrain);
        let select = in_state(EditorTool::Select);

        // ── Per-frame preparation ────────────────────────────────────────────
        app.init_resource::<ViewportInput>();
        app.init_resource::<EditorRay>();
        app.init_resource::<Selection>();
        app.add_systems(
            Update,
            (compute_editor_ray, validate_selection, prepare_history)
                .chain()
                .in_set(EditorSet::Prepare),
        );

        // ── Edits and actions (panels → world) ───────────────────────────────
        app.add_message::<PlacedEdited>();
        app.add_message::<ScriptEdit>();
        app.add_message::<RespawnPlaced>();
        app.add_message::<EditorAction>();
        app.add_message::<PrefabAction>();
        app.init_resource::<PrefabEditState>();
        app.init_resource::<LightingState>();
        app.init_resource::<History>();
        // Undo/redo first: the messages it emits are committed below in the
        // same frame. `settle_history` last: it sees this frame's recordings.
        app.add_systems(
            Update,
            (
                undo_redo_hotkeys.run_if(keyboard_free),
                apply_undo_redo,
                apply_level_actions,
                apply_selection_actions,
                apply_prefab_actions,
                commit_placed_edits,
                apply_script_edits,
                respawn_placed,
                clear_lighting_stale_on_bake,
                settle_history,
            )
                .chain()
                .in_set(EditorSet::InGame),
        );

        // ── Camera, cursor, render target ────────────────────────────────────
        app.init_resource::<EditorCameraSettings>();
        app.init_resource::<FlyLock>();
        app.add_systems(
            Update,
            (init_editor_camera, sync_look_cursor, sync_render_target).in_set(EditorSet::InGame),
        );
        app.add_systems(Update, editor_fly_camera.in_set(EditorSet::Active));
        app.add_systems(
            OnEnter(EditorMode::Editor),
            free_cursor_for_editor.run_if(in_state(AppState::InGame)),
        );
        app.add_systems(
            OnEnter(AppState::InGame),
            free_cursor_for_editor.run_if(in_state(EditorMode::Editor)),
        );

        // ── Selection + transform gizmo ──────────────────────────────────────
        // Hover and the selection/hover boxes are Select-tool-only: a gizmo
        // box under the placement preview or the terrain brush is just noise.
        // The transform gizmo runs first (chained) so a click on a handle is
        // a drag, never a (de)select — `gizmo_pointer_free` reads the state
        // it just computed.
        app.init_resource::<GizmoInteraction>();
        app.init_gizmo_group::<EditorGizmoGroup>();
        app.add_systems(Startup, configure_editor_gizmos);
        app.add_systems(
            Update,
            (
                update_transform_gizmo.run_if(select.clone()),
                select_on_click.run_if(
                    select
                        .clone()
                        .and_then(viewport_pointer_free)
                        .and_then(gizmo_pointer_free),
                ),
                update_hover.run_if(select.clone()),
                focus_selection.run_if(keyboard_free),
            )
                .chain()
                .in_set(EditorSet::Active),
        );
        app.add_systems(
            Update,
            (draw_selection_gizmos, draw_transform_gizmo)
                .in_set(EditorSet::InGame)
                .run_if(select.clone()),
        );
        app.add_systems(OnExit(EditorTool::Select), (clear_hover, reset_gizmo));

        // ── Placement (Place tool) ───────────────────────────────────────────
        app.init_resource::<PlacementPreviewEntity>();
        app.init_resource::<GrabbedEntity>();
        // `delete_selected` first: it bails while something is carried, so
        // one Delete press can't both destroy the carried entity and delete
        // the selection. Dropping a carried entity clears the click so the
        // same click can't also place a fresh one.
        app.add_systems(
            Update,
            (
                (delete_selected, handle_grab_keys)
                    .chain()
                    .run_if(keyboard_free.and_then(select.clone().or_else(place.clone()))),
                (place_grabbed_object, handle_place_click)
                    .chain()
                    .run_if(place.clone().and_then(viewport_pointer_free)),
                (update_placement_preview, draw_preview_gizmo)
                    .chain()
                    .run_if(place.clone()),
                (adjust_placement_scale, adjust_placement_rotation)
                    .run_if(place.clone().and_then(viewport_pointer_free)),
            )
                .in_set(EditorSet::Active),
        );
        app.add_systems(
            Update,
            (
                setup_placement_preview,
                rebuild_preview_on_config_change
                    .run_if(resource_changed::<crate::game::world::objects::PlacementConfig>),
            )
                .chain()
                .in_set(EditorSet::InGame)
                .run_if(place.clone()),
        );
        app.add_systems(
            OnExit(EditorTool::Place),
            (release_grab, teardown_placement_preview).chain(),
        );

        // ── Catalog ──────────────────────────────────────────────────────────
        app.init_asset::<AssetManifestAsset>();
        app.init_asset_loader::<AssetManifestLoader>();
        app.init_resource::<AssetCatalog>();
        app.init_resource::<previews::PreviewImages>();
        app.init_resource::<thumbnails::ThumbnailCache>();
        app.add_systems(Startup, request_asset_manifest);
        app.add_systems(Update, populate_asset_catalog.in_set(EditorSet::InGame));

        // ── Terrain tool ─────────────────────────────────────────────────────
        app.init_resource::<TerrainBrushHit>();
        app.init_resource::<BrushSettings>();
        app.add_systems(
            Update,
            (
                adjust_brush_size.run_if(keyboard_free),
                raycast_terrain_hit,
                apply_terrain_brush,
                finalize_terrain_stroke,
                update_brush_preview,
            )
                .chain()
                .in_set(EditorSet::Active)
                .run_if(terrain.clone()),
        );
        app.add_systems(OnExit(EditorTool::Terrain), teardown_terrain_brush_preview);

        // ── Save / bake / debug toggles ──────────────────────────────────────
        app.add_systems(
            Update,
            (
                trigger_save_terrain,
                trigger_bake,
                toggle_physics_debug,
                toggle_lightmap_mode,
                crate::game::gfx::crosshair::toggle_crosshair,
                crate::game::gfx::outline::toggle_outline,
                crate::game::gfx::post_process::toggle_edge_detection,
            )
                .in_set(EditorSet::InGame)
                .run_if(keyboard_free),
        );
        app.add_systems(
            Update,
            (sync_physics_debug, sync_lightmap_mode).in_set(EditorSet::InGame),
        );

        // ── Mode toggle (must work in both modes) ────────────────────────────
        app.add_systems(Update, toggle_editor_mode.in_set(SimSet::InGame));

        // ── OnExit cleanup so gameplay sees a clean screen ───────────────────
        app.add_systems(
            OnExit(EditorMode::Editor),
            (
                // Before the preview teardown: a carried entity must be put
                // back into the world, not dropped on the floor.
                release_grab,
                teardown_placement_preview,
                teardown_terrain_brush_preview,
                restore_render_target,
                release_fly_lock,
            )
                .chain(),
        );
    }
}

fn toggle_editor_mode(
    keys: Res<ButtonInput<KeyCode>>,
    current: Res<State<EditorMode>>,
    mut next: ResMut<NextState<EditorMode>>,
) {
    if keys.just_pressed(KeyCode::F1) {
        let target = match current.get() {
            EditorMode::Editor => EditorMode::Gameplay,
            EditorMode::Gameplay => EditorMode::Editor,
        };
        info!("EditorMode -> {target:?}");
        next.set(target);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::ecs::system::IntoSystem;

    fn initializes<M>(name: &str, system: impl IntoSystem<(), (), M>) {
        let mut world = World::new();
        let mut system = IntoSystem::into_system(system);
        // Panics with B0001 if two parameters conflict — which is exactly
        // what would otherwise happen at startup.
        system.initialize(&mut world);
        let _ = name;
    }

    /// Every editor system that takes more than one query. Conflicting
    /// access (e.g. `&mut PlacedObject` alongside `&PlacedObject`) is a
    /// startup panic, not a compile error, so it is checked here.
    #[test]
    fn editor_systems_have_disjoint_parameters() {
        initializes("apply_undo_redo", apply_undo_redo);
        initializes("settle_history", settle_history);
        initializes("prepare_history", prepare_history);
        initializes("apply_level_actions", apply_level_actions);
        initializes("apply_prefab_actions", apply_prefab_actions);
        initializes("apply_selection_actions", apply_selection_actions);
        initializes("commit_placed_edits", commit_placed_edits);
        initializes("apply_script_edits", apply_script_edits);
        initializes("respawn_placed", respawn_placed);
        initializes("select_on_click", select_on_click);
        initializes("update_hover", update_hover);
        initializes("draw_selection_gizmos", draw_selection_gizmos);
        initializes("focus_selection", focus_selection);
        initializes("delete_selected", delete_selected);
        initializes("handle_grab_keys", handle_grab_keys);
        initializes("place_grabbed_object", place_grabbed_object);
        initializes("handle_place_click", handle_place_click);
        initializes("update_placement_preview", update_placement_preview);
        initializes("editor_fly_camera", editor_fly_camera);
        initializes("init_editor_camera", init_editor_camera);
        initializes("sync_render_target", sync_render_target);
        initializes("apply_terrain_brush", apply_terrain_brush);
        initializes("finalize_terrain_stroke", finalize_terrain_stroke);
        initializes("raycast_terrain_hit", raycast_terrain_hit);
        initializes("update_brush_preview", terrain_edit::update_brush_preview);
        initializes("compute_editor_ray", compute_editor_ray);
        initializes("update_transform_gizmo", update_transform_gizmo);
        initializes("draw_transform_gizmo", draw_transform_gizmo);
        initializes("editor_ui", ui::editor_ui);
    }
}
