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
use super::catalog::AssetCatalog;
use super::controls::ActiveControls;
use super::edits::{LightingState, PlacedEdited, RespawnPlaced, ScriptEdit};
use super::grab::GrabbedEntity;
use super::history::History;
use super::picking::ViewportInput;
use super::prefab_edit::{PrefabAction, PrefabEditState};
use super::previews::PreviewImages;
use super::selection::Selection;
use super::terrain_edit::BrushSettings;
use super::thumbnails::ThumbnailCache;
use super::tool::EditorTool;
use crate::AppState;
use crate::game::gfx::camera::{GameRenderTarget, PlayerCamera};
use crate::game::world::objects::{PlacedObject, PlacedObjectRegistry, PlacementConfig};
use crate::game::world::prefabs::PrefabLibrary;
use crate::game::world::terrain::storage::{TerrainWorld, WorldConfig};
use crate::projects::ActiveProject;
use crate::script_host::{LoadedPluginIds, NeedsHydration};
use crate::settings::UserSettings;

mod assets;
mod history_panel;
mod inspector;
mod menu;
mod outliner;
mod place;
mod terrain;
mod tool_panel;
mod viewport;

pub use assets::AssetsBrowserState;
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
        app.init_resource::<AssetsBrowserState>();
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
    /// Undo/redo stack of the active world.
    History,
    /// Browse and pick meshes, built-ins, prefabs, textures, audio.
    Assets,
}

impl EditorTab {
    fn title(self) -> &'static str {
        match self {
            EditorTab::Viewport => "Viewport",
            EditorTab::Outliner => "Outliner",
            EditorTab::Inspector => "Inspector",
            EditorTab::Tool => "Tool",
            EditorTab::History => "History",
            EditorTab::Assets => "Assets",
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
        let [viewport, _outliner] = surface.split_left(
            NodeIndex::root(),
            0.82,
            vec![EditorTab::Outliner, EditorTab::History],
        );
        let [viewport, inspector] = surface.split_right(viewport, 0.75, vec![EditorTab::Inspector]);
        // Tool settings under the inspector; the asset browser gets the wide
        // strip under the viewport, where a thumbnail grid has room.
        surface.split_below(inspector, 0.6, vec![EditorTab::Tool]);
        surface.split_below(viewport, 0.72, vec![EditorTab::Assets]);
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
    pub catalog: Res<'w, AssetCatalog>,
    pub config: ResMut<'w, PlacementConfig>,
    pub tool: Res<'w, State<EditorTool>>,
    pub settings: ResMut<'w, UserSettings>,
    pub brush: ResMut<'w, BrushSettings>,
    pub lighting: ResMut<'w, LightingState>,
    pub history: ResMut<'w, History>,
    pub registry: Res<'w, PlacedObjectRegistry>,
    pub prefabs: Res<'w, PrefabLibrary>,
    pub prefab_edit: ResMut<'w, PrefabEditState>,
    pub prefab_actions: MessageWriter<'w, PrefabAction>,
    pub assets: ResMut<'w, AssetsBrowserState>,
    pub previews: ResMut<'w, PreviewImages>,
    pub thumbnails: ResMut<'w, ThumbnailCache>,
    pub asset_server: Res<'w, AssetServer>,
    pub images: Res<'w, Assets<Image>>,
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

/// Put Hack right behind the proportional face in the fallback chain.
///
/// egui's proportional family falls back Ubuntu-Light → NotoEmoji →
/// emoji-icon-font, and none of those has the geometric-shapes block the
/// panels use for markers (▸ ▾ ◆ ▤ ○ …): they rendered as the replacement
/// box on every platform, since egui rasterises its own embedded fonts.
/// Hack — already bundled, for the monospace family — has the whole block,
/// so it goes in ahead of the emoji fonts and every such marker comes from
/// one face. Emoji still fall through to NotoEmoji. See `glyph_is_in_hack`.
fn install_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    if let Some(family) = fonts.families.get_mut(&egui::FontFamily::Proportional) {
        family.insert(1.min(family.len()), "Hack".to_owned());
    }
    ctx.set_fonts(fonts);
}

/// The viewport tab's title: frame rate, camera, save and bake state. This
/// is the "status bar" — the viewport has no other heading to spend on the
/// word "Viewport".
fn viewport_title(
    panels: &Panels,
    diagnostics: &DiagnosticsStore,
    camera: Option<&Transform>,
    style: &egui::Style,
) -> egui::WidgetText {
    let font = egui::TextStyle::Button.resolve(style);
    let dim = style.visuals.weak_text_color();
    let warn = egui::Color32::from_rgb(255, 200, 80);
    let mut job = egui::text::LayoutJob::default();
    let mut append = |text: String, color: egui::Color32| {
        job.append(&text, 0.0, egui::TextFormat::simple(font.clone(), color));
    };
    let fps = diagnostics
        .get(&FrameTimeDiagnosticsPlugin::FPS)
        .and_then(|d| d.smoothed())
        .unwrap_or(0.0);
    append(format!("{fps:>4.0} fps"), dim);
    if let Some(t) = camera {
        let p = t.translation;
        append(format!("   cam {:.1} {:.1} {:.1}", p.x, p.y, p.z), dim);
    }
    let dirty = panels.registry.dirty_chunks.len();
    if dirty > 0 {
        append(format!("   {dirty} unsaved chunk(s) · Shift+S"), warn);
    } else {
        append("   all saved".to_owned(), dim);
    }
    if panels.lighting.stale {
        append("   lighting out of date · Shift+B".to_owned(), warn);
    } else {
        append("   lighting baked".to_owned(), dim);
    }
    job.into()
}

pub(super) fn editor_ui(
    mut contexts: EguiContexts,
    mut fonts_installed: Local<bool>,
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
    // Same for every preview / thumbnail image the panels want to draw.
    for handle in panels.previews.unbound(panels.thumbnails.images()) {
        let id = contexts
            .image_id(handle.id())
            .unwrap_or_else(|| contexts.add_image(EguiTextureHandle::Weak(handle.id())));
        panels.previews.bound(&handle, id);
    }
    let Ok(ctx) = contexts.ctx_mut() else {
        return;
    };
    let ctx = ctx.clone();
    if !std::mem::replace(&mut *fonts_installed, true) {
        install_fonts(&ctx);
    }

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
    if std::mem::take(&mut panels.assets.focus_requested)
        && let Some(path) = dock.0.find_tab(&EditorTab::Assets)
    {
        let _ = dock.0.set_active_tab(path);
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

    // Key hints, plus warnings when there are any. Frame rate, camera and
    // save/bake state live in the viewport tab's title instead.
    egui::Panel::bottom("editor_status").show(&mut root, |ui| {
        ui.horizontal_wrapped(|ui| {
            for (keys, action) in controls.rows(panels.fly_lock.0, panels.brush.mode) {
                ui.strong(keys);
                ui.label(action);
                ui.add_space(8.0);
            }
        });
        // Instances whose prefab the library doesn't have: they spawned
        // bare and heal when the prefab file reappears.
        let mut missing: Vec<&str> = panels
            .placed
            .iter()
            .filter_map(|(_, obj, _, _, _)| obj.prefab.as_ref())
            .filter(|inst| panels.prefabs.get(&inst.name).is_none())
            .map(|inst| inst.name.as_str())
            .collect();
        missing.sort_unstable();
        missing.dedup();
        let notice = panels.history.notice();
        if !missing.is_empty() || notice.is_some() {
            ui.horizontal_wrapped(|ui| {
                let red = egui::Color32::from_rgb(255, 120, 120);
                if !missing.is_empty() {
                    ui.colored_label(red, format!("missing prefab(s): {}", missing.join(", ")));
                }
                if let Some(notice) = notice {
                    if !missing.is_empty() {
                        ui.separator();
                    }
                    ui.colored_label(red, notice);
                }
            });
        }
    });

    let title = viewport_title(
        &panels,
        &diagnostics,
        camera.single().ok(),
        root.style().as_ref(),
    );
    let mut tabs = EditorTabs::new(
        &mut current,
        previous.look_active,
        texture,
        ctx.pixels_per_point(),
        title,
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Every marker glyph the panels draw must be owned by a face in the
    /// proportional fallback chain. This is the regression that had kind
    /// icons and the outliner's collapse arrows rendering as squares on every
    /// platform — the square *is* egui's replacement glyph `◻` — and
    /// `install_fonts` is the fix.
    ///
    /// Checked through the family's character map rather than
    /// `Font::has_glyph`: that compares against the face that owns `◻`, which
    /// with Hack in the chain is Hack itself, so it misreports every glyph
    /// Hack supplies.
    #[test]
    fn editor_glyphs_are_in_the_proportional_font_chain() {
        use egui::epaint::text::{Fonts, TextOptions};

        let mut fonts = egui::FontDefinitions::default();
        if let Some(family) = fonts.families.get_mut(&egui::FontFamily::Proportional) {
            family.insert(1.min(family.len()), "Hack".to_owned());
        }
        let mut fonts = Fonts::new(TextOptions::default(), fonts);
        let mut font = fonts.fonts.font(&egui::FontFamily::Proportional);
        let owned = font.characters();

        // Markers used across the panels (outliner, assets, inspector,
        // history, status), plus the punctuation the labels lean on.
        let used = "▸▾◆▤▦○▣◉♪▶⤴🔗·×—…°";
        let missing: String = used.chars().filter(|c| !owned.contains_key(c)).collect();
        assert!(missing.is_empty(), "no proportional face has: {missing}");

        // The one that started this: ✦ is in none of the bundled fonts,
        // which is why the light marker became ◉.
        assert!(!owned.contains_key(&'✦'));
    }
}
