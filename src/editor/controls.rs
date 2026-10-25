//! Context-sensitive control hints, shown in the editor's status bar.
//!
//! Many editor controls only do something in a particular tool or panel —
//! scroll cycles the catalog only while the catalog is open, `[` / `]` resize
//! the brush only while sculpting. [`ActiveControls::rows`] yields exactly the
//! controls that are live right now.
//!
//! The row table below is the single source of truth: add a control here and
//! it shows up in the right context automatically.

use bevy::ecs::system::SystemParam;
use bevy::prelude::*;

use super::{grab::GrabbedEntity, terrain_edit::BrushMode, tool::EditorTool};

/// Which context a control belongs to. `Base` rows are always shown.
#[derive(PartialEq, Eq, Clone, Copy)]
enum Ctx {
    Base,
    Select,
    Place,
    Carrying,
    /// Terrain tool, any brush mode.
    Terrain,
    TerrainSculpt,
    TerrainFlatten,
    TerrainSmooth,
    /// Fly lock engaged: the cursor is captured, clicks are suppressed.
    Flying,
}

struct ControlRow {
    ctx: Ctx,
    keys: &'static str,
    action: &'static str,
}

const fn row(ctx: Ctx, keys: &'static str, action: &'static str) -> ControlRow {
    ControlRow { ctx, keys, action }
}

/// Every editor control, tagged with the context that makes it live.
static CONTROLS: &[ControlRow] = &[
    row(Ctx::Base, "F1", "Gameplay"),
    row(Ctx::Base, "1 / 2 / 3", "Select / Place / Terrain"),
    row(Ctx::Base, "Esc", "Cancel / deselect"),
    row(Ctx::Base, "Shift+S", "Save"),
    row(Ctx::Base, "Shift+B", "Bake"),
    row(Ctx::Base, "Shift+L", "Lighting view"),
    row(Ctx::Base, "Shift+R", "Reload plugins"),
    // Select tool.
    row(Ctx::Select, "LMB", "Select"),
    row(Ctx::Select, "Alt+LMB", "Select group"),
    row(Ctx::Select, "Drag gizmo", "Move"),
    row(Ctx::Select, "Ctrl+drag gizmo", "Rotate"),
    row(Ctx::Select, "F", "Focus"),
    row(Ctx::Select, "R", "Grab"),
    row(Ctx::Select, "Delete", "Delete"),
    row(Ctx::Select, "Shift+Delete", "Delete group + members"),
    // Place tool, nothing carried.
    row(Ctx::Place, "LMB", "Place"),
    row(Ctx::Place, "R", "Grab"),
    row(Ctx::Place, "Delete", "Delete selection"),
    row(Ctx::Place, "Shift+Scroll", "Scale"),
    row(Ctx::Place, "Alt+Scroll", "Rotate"),
    // Carrying a grabbed object.
    row(Ctx::Carrying, "LMB", "Drop here"),
    row(Ctx::Carrying, "R", "Cancel (restore)"),
    row(Ctx::Carrying, "Delete", "Destroy"),
    row(Ctx::Carrying, "Shift+Scroll", "Scale"),
    row(Ctx::Carrying, "Alt+Scroll", "Rotate"),
    // Terrain tool, per brush mode.
    row(Ctx::TerrainSculpt, "LMB", "Raise"),
    row(Ctx::TerrainSculpt, "Ctrl+LMB", "Lower"),
    row(Ctx::TerrainFlatten, "LMB", "Flatten"),
    row(Ctx::TerrainSmooth, "LMB", "Smooth"),
    row(Ctx::Terrain, "[ / ]", "Brush size"),
    // Fly lock (cursor captured, no clicks).
    row(Ctx::Flying, "Esc", "Exit fly mode"),
    row(Ctx::Flying, "WASD / QE", "Fly (Shift ×4)"),
    row(Ctx::Flying, "Scroll", "Fly speed"),
];

/// Reads the state that decides which rows are live.
#[derive(SystemParam)]
pub struct ActiveControls<'w> {
    tool: Res<'w, State<EditorTool>>,
    grabbed: Res<'w, GrabbedEntity>,
}

impl ActiveControls<'_> {
    /// `flying` and `brush` are passed in rather than read from their
    /// resources ([`super::camera::FlyLock`], `BrushSettings`): `editor_ui`
    /// already holds both mutably through `Panels`, and a second `Res` access
    /// would be a B0002 parameter conflict.
    fn includes(&self, ctx: Ctx, flying: bool, brush: BrushMode) -> bool {
        let tool = *self.tool.get();
        let carrying = self.grabbed.0.is_some();
        let terrain = tool == EditorTool::Terrain && !flying;
        match ctx {
            Ctx::Base => true,
            // While carrying, the normal click/grab bindings are replaced by
            // the carry ones, so the two are mutually exclusive. While
            // flying, pointer-driven rows would be lies: clicks are
            // suppressed until Esc gives the cursor back.
            Ctx::Select => tool == EditorTool::Select && !carrying && !flying,
            Ctx::Place => tool == EditorTool::Place && !carrying && !flying,
            Ctx::Carrying => carrying && !flying,
            Ctx::Terrain => terrain,
            Ctx::TerrainSculpt => terrain && brush == BrushMode::Sculpt,
            Ctx::TerrainFlatten => terrain && brush == BrushMode::Flatten,
            Ctx::TerrainSmooth => terrain && brush == BrushMode::Smooth,
            Ctx::Flying => flying,
        }
    }

    /// `(keys, action)` for every control that is live right now.
    pub fn rows(
        &self,
        flying: bool,
        brush: BrushMode,
    ) -> impl Iterator<Item = (&'static str, &'static str)> + '_ {
        CONTROLS
            .iter()
            .filter(move |c| self.includes(c.ctx, flying, brush))
            .map(|c| (c.keys, c.action))
    }
}
