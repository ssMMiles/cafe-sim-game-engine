//! Editor chrome: the egui dock around the 3D viewport.
//!
//! Everything the editor *draws* lives here (bevy_ui remains the player's UI:
//! main menu, plugin HUDs). The dock is immediate-mode and rebuilt every pass
//! in [`EguiPrimaryContextPass`], which bevy_egui runs during `PostUpdate`;
//! anything it publishes for `Update` systems (notably [`ViewportInput`] and
//! the edit/action messages) is therefore consumed next frame.
//!
//! The egui context is attached explicitly to the in-game `Camera2d`
//! (`PrimaryEguiContext` in `gfx::camera::setup_camera`) rather than
//! auto-created, because auto-creation grabs the *first* camera — the main
//! menu's — and the in-game 3D and 2D cameras spawn in the same frame.
//! bevy_ui is ordered above egui so gameplay UI is never hidden by the dock.
//!
//! Panels get their ECS access through one [`Panels`] system param so the
//! `TabViewer` holds no borrows of its own; each panel is a free function
//! `draw(ui, &mut Panels)` in its own file.

use bevy::diagnostic::{DiagnosticsStore, FrameTimeDiagnosticsPlugin};
use bevy::ecs::system::SystemParam;
use bevy::prelude::*;
use bevy_egui::{
    EguiContexts, EguiGlobalSettings, EguiPlugin, EguiPrimaryContextPass, EguiTextureHandle,
    UiRenderOrder, egui,
};
use egui_dock::{DockArea, DockState, NodeIndex};
use engine_api::{ScriptBindings, ScriptRegistry};

use super::EditorMode;
use super::actions::EditorAction;
use super::camera::{EditorCameraSettings, FlyLock};
use super::catalog::MeshCatalog;
use super::controls::ActiveControls;
use super::edits::{LightingState, PlacedEdited, RespawnPlaced, ScriptEdit};
use super::grab::GrabbedEntity;
use super::history::History;
use super::picking::ViewportInput;
use super::selection::Selection;
use super::terrain_edit::BrushSettings;
use super::tool::EditorTool;
use crate::AppState;
use crate::game::gfx::camera::{GameRenderTarget, PlayerCamera};
use crate::game::world::objects::{PlacedObject, PlacedObjectRegistry, PlacementConfig};
use crate::game::world::terrain::storage::{TerrainWorld, WorldConfig};
use crate::projects::ActiveProject;
use crate::script_host::{LoadedPluginIds, NeedsHydration};
use crate::settings::UserSettings;

mod inspector;
mod menu;
mod outliner;
mod place;
mod terrain;
mod tool_panel;
mod viewport;

pub use inspector::InspectorState;
pub use menu::MenuState;
pub use outliner::OutlinerState;
use viewport::EditorTabs;

pub struct EditorUiPlugin;

impl Plugin for EditorUiPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(EguiPlugin {
            ui_render_order: UiRenderOrder::BevyUiAboveEgui,
            ..default()
        });
        app.insert_resource(EguiGlobalSettings {
            auto_create_primary_context: false,
            ..default()
        });
        app.init_resource::<DockLayout>();
        app.init_resource::<InspectorState>();
        app.init_resource::<OutlinerState>();
        app.init_resource::<MenuState>();
        app.add_systems(
            EguiPrimaryContextPass,
            editor_ui.run_if(in_state(AppState::InGame).and_then(in_state(EditorMode::Editor))),
        );
    }
}

/// A dock tab. The layout is rebuilt from [`DockLayout::default`] every run
/// (no persistence yet — `docs/EDITOR_UI_DESIGN.md` §4.14).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EditorTab {
    Viewport,
    Outliner,
    Inspector,
    /// Settings for whatever [`EditorTool`] is active.
    Tool,
}

impl EditorTab {
    fn title(self) -> &'static str {
        match self {
            EditorTab::Viewport => "Viewport",
            EditorTab::Outliner => "Outliner",
            EditorTab::Inspector => "Inspector",
            EditorTab::Tool => "Tool",
        }
    }
}

#[derive(Resource)]
pub struct DockLayout(pub DockState<EditorTab>);

impl Default for DockLayout {
    fn default() -> Self {
        let mut state = DockState::new(vec![EditorTab::Viewport]);
        let surface = state.main_surface_mut();
        // `fraction` is the share the *old* node keeps.
        let [viewport, _outliner] =
            surface.split_left(NodeIndex::root(), 0.82, vec![EditorTab::Outliner]);
        let [viewport, _inspector] =
            surface.split_right(viewport, 0.75, vec![EditorTab::Inspector]);
        surface.split_below(viewport, 0.75, vec![EditorTab::Tool]);
        Self(state)
    }
}

/// Every placed entity, as the panels see it.
pub type PlacedQuery<'w, 's> = Query<
    'w,
    's,
    (
        Entity,
        &'static mut PlacedObject,
        &'static mut Transform,
        Option<&'static ScriptBindings>,
        Has<NeedsHydration>,
    ),
>;

/// ECS access for the panels, gathered once per pass.
#[derive(SystemParam)]
pub struct Panels<'w, 's> {
    pub selection: ResMut<'w, Selection>,
    pub placed: PlacedQuery<'w, 's>,
    pub script_registry: Res<'w, ScriptRegistry>,
    pub loaded_plugins: Res<'w, LoadedPluginIds>,
    pub active: Option<Res<'w, ActiveProject>>,
    pub world_config: Option<Res<'w, WorldConfig>>,
    pub terrain_world: Option<ResMut<'w, TerrainWorld>>,
    pub catalog: Res<'w, MeshCatalog>,
    pub config: ResMut<'w, PlacementConfig>,
    pub tool: Res<'w, State<EditorTool>>,
    pub settings: ResMut<'w, UserSettings>,
    pub brush: ResMut<'w, BrushSettings>,
    pub lighting: ResMut<'w, LightingState>,
    pub history: ResMut<'w, History>,
    pub registry: Res<'w, PlacedObjectRegistry>,
    pub grabbed: Res<'w, GrabbedEntity>,
    pub cam_settings: Res<'w, EditorCameraSettings>,
    pub fly_lock: ResMut<'w, FlyLock>,
    pub edits: MessageWriter<'w, PlacedEdited>,
    pub script_edits: MessageWriter<'w, ScriptEdit>,
    pub respawns: MessageWriter<'w, RespawnPlaced>,
    pub actions: MessageWriter<'w, EditorAction>,
    pub inspector: ResMut<'w, InspectorState>,
    pub outliner: ResMut<'w, OutlinerState>,
    pub menu: ResMut<'w, MenuState>,
}

impl Panels<'_, '_> {
    /// Display name for a placed entity: its label, or its kind.
    pub fn display_name(obj: &PlacedObject) -> String {
        obj.display_name()
    }
}

pub(super) fn editor_ui(
    mut contexts: EguiContexts,
    mut dock: ResMut<DockLayout>,
    mut viewport: ResMut<ViewportInput>,
    target: Option<Res<GameRenderTarget>>,
    mut panels: Panels,
    controls: ActiveControls,
    diagnostics: Res<DiagnosticsStore>,
    camera: Query<&Transform, (With<PlayerCamera>, Without<PlacedObject>)>,
) {
    // Register (idempotently) the render texture with egui before borrowing
    // the context, since both go through `EguiContexts`.
    let texture = target.as_ref().map(|t| {
        contexts
            .image_id(t.0.id())
            .unwrap_or_else(|| contexts.add_image(EguiTextureHandle::Weak(t.0.id())))
    });
    let Ok(ctx) = contexts.ctx_mut() else {
        return;
    };
    let ctx = ctx.clone();

    // Start from a blank slate every pass; the viewport tab fills in what it
    // saw. `size_px` carries over so the render target isn't shrunk to 1×1
    // on a pass where the tab wasn't drawn.
    let previous = std::mem::take(&mut *viewport);
    let mut current = ViewportInput {
        size_px: previous.size_px,
        ..default()
    };

    if std::mem::take(&mut panels.menu.reset_layout) {
        *dock = DockLayout::default();
    }

    // egui 0.34 unified panels onto `Ui`: `Panel` (which replaced
    // `SidePanel`/`TopBottomPanel`) and `DockArea` take a `&mut Ui`, not a
    // `&Context`. bevy_egui drives `begin_pass`/`end_pass` itself so we can't
    // use `Context::run_ui`; build the root `Ui` directly instead. It defaults
    // to the background layer and `Context::content_rect`, which is what the
    // old context-level panels used. Each panel shrinks `root`'s available
    // space in turn and the dock fills whatever is left.
    let mut root = egui::Ui::new(
        ctx.clone(),
        egui::Id::new("editor_root"),
        egui::UiBuilder::new(),
    );

    egui::Panel::top("editor_menu").show(&mut root, |ui| {
        menu::menu_bar(ui, &mut panels);
    });
    egui::Panel::top("editor_toolbar").show(&mut root, |ui| {
        menu::toolbar(ui, &mut panels);
    });
    // `Window` is a free-floating area, not a panel — it still takes a `&Context`.
    menu::dialogs(&ctx, &mut panels);

    egui::Panel::bottom("editor_status").show(&mut root, |ui| {
        ui.horizontal_wrapped(|ui| {
            for (keys, action) in controls.rows(panels.fly_lock.0, panels.brush.mode) {
                ui.strong(keys);
                ui.label(action);
                ui.add_space(8.0);
            }
        });
        ui.horizontal_wrapped(|ui| {
            let fps = diagnostics
                .get(&FrameTimeDiagnosticsPlugin::FPS)
                .and_then(|d| d.smoothed())
                .unwrap_or(0.0);
            ui.monospace(format!("{fps:>4.0} fps"));
            if let Ok(t) = camera.single() {
                let p = t.translation;
                ui.monospace(format!("cam {:.1} {:.1} {:.1}", p.x, p.y, p.z));
            }
            ui.separator();
            let dirty = panels.registry.dirty_chunks.len();
            if dirty > 0 {
                ui.colored_label(
                    egui::Color32::from_rgb(255, 200, 80),
                    format!("{dirty} unsaved chunk(s) · Shift+S"),
                );
            } else {
                ui.weak("all saved");
            }
            ui.separator();
            if panels.lighting.stale {
                ui.colored_label(
                    egui::Color32::from_rgb(255, 200, 80),
                    "lighting out of date · Shift+B",
                );
            } else {
                ui.weak("lighting baked");
            }
            if let Some(notice) = panels.history.notice() {
                ui.separator();
                ui.colored_label(egui::Color32::from_rgb(255, 120, 120), notice);
            }
        });
    });

    let mut tabs = EditorTabs::new(
        &mut current,
        previous.look_active,
        texture,
        ctx.pixels_per_point(),
        &mut panels,
    );

    let mut style = egui_dock::Style::from_egui(root.style().as_ref());
    style.tab.tab_body.inner_margin = egui::Margin::ZERO;
    DockArea::new(&mut dock.0)
        .style(style)
        .show_close_buttons(false)
        .show_leaf_close_all_buttons(false)
        .show_leaf_collapse_buttons(false)
        .show_inside(&mut root, &mut tabs);

    // Whether a widget interaction is still in flight: a drag, an open popup
    // (colour picker) or a focused field. The undo stack keeps a continuous
    // edit's transaction open while this holds.
    current.egui_busy = ctx.egui_is_using_pointer()
        || egui::Popup::is_any_open(&ctx)
        || ctx.egui_wants_keyboard_input();

    *viewport = current;
}
