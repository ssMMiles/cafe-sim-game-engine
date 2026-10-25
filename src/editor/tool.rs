//! The editor's active tool.
//!
//! Replaces the two booleans that used to live on `UserSettings`
//! (`edit_mode`, `terrain_edit_mode`). As a Bevy `State`, every tool gets
//! `OnEnter`/`OnExit` hooks, which is where previews are torn down and a
//! carried entity is put back — no more "did a flag flip under me" checks
//! spread across systems.

use bevy::prelude::*;

use super::EditorSet;
use super::camera::FlyLock;
use super::gizmo::GizmoInteraction;
use super::grab::GrabbedEntity;
use super::input::keyboard_free;
use super::picking::ViewportInput;
use super::selection::Selection;

#[derive(States, Default, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EditorTool {
    /// Click to select; `F` focuses; `Delete` removes the selection.
    #[default]
    Select,
    /// Place the catalog's current entry under the cursor; `R` grabs.
    Place,
    /// Sculpt the heightmap (LMB raise, Ctrl+LMB lower).
    Terrain,
}

impl EditorTool {
    pub fn label(self) -> &'static str {
        match self {
            EditorTool::Select => "Select",
            EditorTool::Place => "Place",
            EditorTool::Terrain => "Terrain",
        }
    }
}

pub struct ToolPlugin;

impl Plugin for ToolPlugin {
    fn build(&self, app: &mut App) {
        app.init_state::<EditorTool>();
        app.add_systems(
            Update,
            (tool_hotkeys, escape_cancels)
                .in_set(EditorSet::InGame)
                .run_if(keyboard_free),
        );
        // Leaving the editor always lands back on Select so re-entry is
        // predictable.
        app.add_systems(OnExit(super::EditorMode::Editor), reset_tool);
    }
}

/// `1` / `2` / `3` pick a tool directly while the viewport is hovered.
/// `Shift+E` and `Shift+G` keep their old meaning as toggles (Place and
/// Terrain respectively, back to Select when pressed again).
fn tool_hotkeys(
    input: Res<ButtonInput<KeyCode>>,
    viewport: Res<ViewportInput>,
    current: Res<State<EditorTool>>,
    mut next: ResMut<NextState<EditorTool>>,
) {
    let shift = input.pressed(KeyCode::ShiftLeft) || input.pressed(KeyCode::ShiftRight);
    let toggle = |tool: EditorTool| {
        if *current.get() == tool {
            EditorTool::Select
        } else {
            tool
        }
    };

    let target = if shift && input.just_pressed(KeyCode::KeyE) {
        Some(toggle(EditorTool::Place))
    } else if shift && input.just_pressed(KeyCode::KeyG) {
        Some(toggle(EditorTool::Terrain))
    } else if viewport.hovered && !shift {
        if input.just_pressed(KeyCode::Digit1) {
            Some(EditorTool::Select)
        } else if input.just_pressed(KeyCode::Digit2) {
            Some(EditorTool::Place)
        } else if input.just_pressed(KeyCode::Digit3) {
            Some(EditorTool::Terrain)
        } else {
            None
        }
    } else {
        None
    };

    if let Some(target) = target
        && target != *current.get()
    {
        info!("EditorTool -> {target:?}");
        next.set(target);
    }
}

/// Escape backs out one level: fly lock releases first (the cursor must come
/// back before anything else makes sense), then an in-flight gizmo drag is
/// cancelled (the gizmo system restores the start transform), then a carried
/// entity goes back (by leaving the Place tool, whose `OnExit` restores it),
/// then a non-Select tool returns to Select, then the selection clears.
fn escape_cancels(
    input: Res<ButtonInput<KeyCode>>,
    grabbed: Res<GrabbedEntity>,
    current: Res<State<EditorTool>>,
    mut next: ResMut<NextState<EditorTool>>,
    mut selection: ResMut<Selection>,
    mut fly_lock: ResMut<FlyLock>,
    mut gizmo: ResMut<GizmoInteraction>,
) {
    if !input.just_pressed(KeyCode::Escape) {
        return;
    }
    if fly_lock.0 {
        fly_lock.0 = false;
    } else if gizmo.drag.is_some() {
        gizmo.cancel_requested = true;
    } else if grabbed.0.is_some() || *current.get() != EditorTool::Select {
        next.set(EditorTool::Select);
    } else {
        selection.primary = None;
    }
}

fn reset_tool(current: Res<State<EditorTool>>, mut next: ResMut<NextState<EditorTool>>) {
    if *current.get() != EditorTool::Select {
        next.set(EditorTool::Select);
    }
}
