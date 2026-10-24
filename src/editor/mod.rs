//! `EditorPlugin` — owns every system that exists for authoring rather than
//! gameplay (object placement, terrain sculpting, save/bake triggers, debug
//! HUDs, debug toggles). Toggle between editor and gameplay with **F1**.
//!
//! Default mode is [`EditorMode::Editor`] so existing workflows behave as
//! before. While in [`EditorMode::Gameplay`], everything in this plugin is
//! suspended; on transition out of editor the open editor UI/preview entities
//! are despawned so the screen is clean.

use bevy::prelude::*;

use crate::{
    AppState,
    game::{SimSet, SimulationState},
    settings::UserSettings,
};

mod baking_trigger;
mod controls;
mod debug_hud;
mod grab;
mod input;
mod lightmap_toggle;
mod mesh_selector;
mod object_placement;
mod physics_debug;
mod script_binding;
mod terrain_edit;
mod terrain_save;

pub use mesh_selector::MeshSelectorOpen;
pub use script_binding::ScriptPanelOpen;

use baking_trigger::trigger_bake;
use controls::{
    ControlsCache, cleanup_controls_panel, setup_controls_panel, update_controls_panel,
};
use debug_hud::{
    cleanup_debug_hud, setup_debug_hud, update_fps_display, update_interaction_info_display,
    update_keybinds_display, update_position_display,
};
use grab::{
    GrabbedEntity, handle_grab_keys, place_grabbed_object, release_grab,
    release_grab_on_edit_mode_exit,
};
use lightmap_toggle::{sync_lightmap_mode, toggle_lightmap_mode};
use mesh_selector::{
    MeshCatalog, MeshManifestAsset, MeshManifestLoader, MeshSelectorIndex,
    cleanup_mesh_selector_panel, mesh_selector_open, populate_mesh_catalog, request_mesh_manifest,
    scroll_mesh_selector, setup_mesh_selector_panel, teardown_mesh_selector, toggle_mesh_selector,
};
use object_placement::{
    PlacementPreviewEntity, adjust_placement_rotation, adjust_placement_scale,
    cleanup_placement_preview, delete_hovered_object, draw_preview_gizmo, handle_edit_mode_click,
    rebuild_preview_on_config_change, setup_placement_preview, teardown_placement_preview,
    toggle_edit_mode, update_placement_preview,
};
use physics_debug::{sync_physics_debug, toggle_physics_debug};
use script_binding::{
    ScriptPanelEntries, ScriptPanelIndex, handle_script_panel_keys, script_panel_open,
    scroll_script_panel, sync_script_panel, teardown_script_panel, toggle_script_panel,
    update_script_panel_status,
};
use terrain_edit::{
    TerrainBrushHit, adjust_brush_size, apply_terrain_brush, finalize_terrain_lod,
    raycast_terrain_hit, teardown_terrain_brush_preview, toggle_terrain_edit_mode,
    update_brush_preview,
};
use terrain_save::trigger_save_terrain;

#[derive(States, Default, Debug, Clone, PartialEq, Eq, Hash)]
pub enum EditorMode {
    #[default]
    Editor,
    Gameplay,
}

#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub enum EditorSet {
    /// `AppState::InGame` + `EditorMode::Editor`. HUD updates, panel toggles,
    /// edit-mode toggles — runs while paused too.
    InGame,
    /// `AppState::InGame` + `SimulationState::Running` + `EditorMode::Editor`.
    /// Placement, sculpt, scroll cycling — input-driven editing only.
    Active,
}

pub struct EditorPlugin;

impl Plugin for EditorPlugin {
    fn build(&self, app: &mut App) {
        app.init_state::<EditorMode>();

        app.configure_sets(
            Update,
            EditorSet::InGame.run_if(in_state(AppState::InGame).and(in_state(EditorMode::Editor))),
        );
        app.configure_sets(
            Update,
            EditorSet::Active.run_if(
                in_state(AppState::InGame)
                    .and(in_state(SimulationState::Running))
                    .and(in_state(EditorMode::Editor)),
            ),
        );

        // ── Object placement ────────────────────────────────────────────────
        app.init_resource::<PlacementPreviewEntity>();
        app.add_systems(Update, toggle_edit_mode.in_set(EditorSet::InGame));
        app.add_systems(
            Update,
            handle_edit_mode_click
                .in_set(EditorSet::Active)
                .run_if(edit_mode_active),
        );
        // ── Grab (move/rotate/rescale an already-placed entity) ─────────────
        app.init_resource::<GrabbedEntity>();
        // `delete_hovered_object` first: it bails while something is carried,
        // so one Delete press can't both destroy the carried entity and
        // delete whatever happens to be behind it.
        app.add_systems(
            Update,
            (delete_hovered_object, handle_grab_keys)
                .chain()
                .in_set(EditorSet::Active)
                .run_if(edit_mode_active),
        );
        // Before `handle_edit_mode_click` (and clears the press) so dropping a
        // carried entity can't also place a fresh one on the same click.
        app.add_systems(
            Update,
            place_grabbed_object
                .before(handle_edit_mode_click)
                .in_set(EditorSet::Active)
                .run_if(edit_mode_active),
        );
        // Not gated on `edit_mode_active` — this is what catches edit mode
        // being switched off while an entity is still held.
        app.add_systems(
            Update,
            release_grab_on_edit_mode_exit.in_set(EditorSet::InGame),
        );
        app.add_systems(Update, setup_placement_preview.in_set(EditorSet::InGame));
        app.add_systems(Update, cleanup_placement_preview.in_set(EditorSet::InGame));
        app.add_systems(
            Update,
            (update_placement_preview, draw_preview_gizmo)
                .chain()
                .in_set(EditorSet::Active),
        );
        app.add_systems(
            Update,
            (adjust_placement_scale, adjust_placement_rotation).in_set(EditorSet::Active),
        );
        app.add_systems(
            Update,
            rebuild_preview_on_config_change
                .in_set(EditorSet::InGame)
                .run_if(edit_mode_active)
                .run_if(resource_changed::<crate::game::world::objects::PlacementConfig>),
        );

        // ── Mesh selector ───────────────────────────────────────────────────
        app.init_asset::<MeshManifestAsset>();
        app.init_asset_loader::<MeshManifestLoader>();
        app.init_resource::<MeshCatalog>();
        app.init_resource::<MeshSelectorOpen>();
        app.init_resource::<MeshSelectorIndex>();
        app.add_systems(Startup, request_mesh_manifest);
        app.add_systems(Update, populate_mesh_catalog.in_set(EditorSet::InGame));
        app.add_systems(Update, toggle_mesh_selector.in_set(EditorSet::InGame));
        app.add_systems(Update, setup_mesh_selector_panel.in_set(EditorSet::InGame));
        app.add_systems(
            Update,
            cleanup_mesh_selector_panel.in_set(EditorSet::InGame),
        );
        app.add_systems(
            Update,
            scroll_mesh_selector
                .in_set(EditorSet::Active)
                .run_if(mesh_selector_open),
        );

        // ── Scripts panel ───────────────────────────────────────────────────
        app.init_resource::<ScriptPanelOpen>();
        app.init_resource::<ScriptPanelIndex>();
        app.init_resource::<ScriptPanelEntries>();
        app.add_systems(Update, toggle_script_panel.in_set(EditorSet::InGame));
        app.add_systems(Update, sync_script_panel.in_set(EditorSet::InGame));
        app.add_systems(
            Update,
            (
                scroll_script_panel,
                handle_script_panel_keys,
                update_script_panel_status,
            )
                .in_set(EditorSet::Active)
                .run_if(script_panel_open),
        );

        // ── Terrain edit ────────────────────────────────────────────────────
        app.init_resource::<TerrainBrushHit>();
        app.add_systems(
            Update,
            (
                toggle_terrain_edit_mode,
                adjust_brush_size,
                raycast_terrain_hit,
                apply_terrain_brush,
                finalize_terrain_lod,
                update_brush_preview,
            )
                .chain()
                .in_set(EditorSet::Active),
        );

        // ── Terrain save / bake triggers ────────────────────────────────────
        app.add_systems(Update, trigger_save_terrain.in_set(EditorSet::InGame));
        app.add_systems(Update, trigger_bake.in_set(EditorSet::InGame));

        // ── Debug toggles & HUD ─────────────────────────────────────────────
        app.add_systems(Update, toggle_physics_debug.in_set(EditorSet::InGame));
        app.add_systems(Update, sync_physics_debug.in_set(EditorSet::InGame));
        app.add_systems(Update, toggle_lightmap_mode.in_set(EditorSet::InGame));
        app.add_systems(Update, sync_lightmap_mode.in_set(EditorSet::InGame));
        app.add_systems(
            Update,
            crate::game::gfx::crosshair::toggle_crosshair.in_set(EditorSet::InGame),
        );
        app.add_systems(
            Update,
            crate::game::gfx::outline::toggle_outline.in_set(EditorSet::InGame),
        );
        app.add_systems(
            Update,
            crate::game::gfx::post_process::toggle_edge_detection.in_set(EditorSet::InGame),
        );

        // HUD spawns either when the editor turns on while in-game, or when
        // entering the game while already in editor mode (the default).
        app.init_resource::<ControlsCache>();
        app.add_systems(
            OnEnter(AppState::InGame),
            (setup_debug_hud, setup_controls_panel).run_if(in_state(EditorMode::Editor)),
        );
        app.add_systems(
            OnEnter(EditorMode::Editor),
            (setup_debug_hud, setup_controls_panel),
        );
        app.add_systems(Update, update_controls_panel.in_set(EditorSet::InGame));
        app.add_systems(Update, update_fps_display.in_set(EditorSet::InGame));
        app.add_systems(Update, update_keybinds_display.in_set(EditorSet::InGame));
        app.add_systems(Update, update_position_display.in_set(EditorSet::InGame));
        app.add_systems(
            Update,
            update_interaction_info_display.in_set(EditorSet::InGame),
        );

        // ── Mode toggle (must work in both modes) ───────────────────────────
        app.add_systems(Update, toggle_editor_mode.in_set(SimSet::InGame));

        // A plugin that was deactivated mid-menu may have left the cursor
        // free; the editor is FPS-style, so take it back. Gated to in-game so
        // the `OnEnter` that fires at startup can't grab the cursor on the
        // main menu.
        app.add_systems(
            OnEnter(EditorMode::Editor),
            lock_cursor_for_editor.run_if(in_state(AppState::InGame)),
        );

        // ── OnExit cleanup so gameplay sees a clean screen ──────────────────
        app.add_systems(
            OnExit(EditorMode::Editor),
            (
                cleanup_debug_hud,
                cleanup_controls_panel,
                teardown_mesh_selector,
                teardown_script_panel,
                // Before the preview teardown: a carried entity must be put
                // back into the world, not dropped on the floor.
                release_grab,
                teardown_placement_preview,
                teardown_terrain_brush_preview,
                clear_editor_sub_modes,
            )
                .chain(),
        );
    }
}

fn edit_mode_active(settings: Res<UserSettings>) -> bool {
    settings.edit_mode
}

fn lock_cursor_for_editor(cursor_options: Option<Single<&mut bevy::window::CursorOptions>>) {
    let Some(mut cursor) = cursor_options else {
        return;
    };
    cursor.grab_mode = bevy::window::CursorGrabMode::Locked;
    cursor.visible = false;
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

/// Force the editor's sub-mode flags off when leaving editor mode so a re-entry
/// starts in a known state. Without this, `settings.edit_mode == true` would
/// leave systems trying to spawn previews the moment editor returns.
fn clear_editor_sub_modes(mut settings: ResMut<UserSettings>) {
    if settings.edit_mode {
        settings.edit_mode = false;
    }
    if settings.terrain_edit_mode {
        settings.terrain_edit_mode = false;
    }
}
