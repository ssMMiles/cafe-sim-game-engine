//! Context-sensitive controls panel (top-left, under the FPS counter).
//!
//! Many editor controls only do something in a particular sub-mode — scroll
//! cycles the catalog only while the catalog is open, `[` / `]` resize the
//! brush only while sculpting — and nothing in-game said so. This panel lists
//! exactly the controls that are live right now and nothing else.
//!
//! The row table below is the single source of truth: add a control here and
//! it shows up in the right context automatically.

use bevy::{ecs::relationship::RelatedSpawnerCommands, prelude::*};

use crate::{
    AppState,
    editor::{
        grab::GrabbedEntity, mesh_selector::MeshSelectorOpen, script_binding::ScriptPanelOpen,
    },
    game::GameEntity,
    settings::UserSettings,
    ui::{UiAnchors, UiTheme},
};

#[derive(Component)]
pub struct ControlsDisplay;

/// Which sub-mode a control belongs to. `Base` rows are always shown.
#[derive(PartialEq, Eq, Clone, Copy)]
enum Ctx {
    Base,
    Edit,
    Carrying,
    Catalog,
    Scripts,
    Terrain,
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
    row(Ctx::Base, "F1", "Editor / Gameplay"),
    row(Ctx::Base, "Shift+E", "Edit mode"),
    row(Ctx::Base, "Shift+G", "Terrain edit"),
    row(Ctx::Base, "Shift+S", "Save world"),
    row(Ctx::Base, "Shift+B", "Bake lightmaps"),
    row(Ctx::Base, "Shift+R", "Reload plugins"),
    // Edit mode, nothing carried.
    row(Ctx::Edit, "Tab", "Placement catalog"),
    row(Ctx::Edit, "K", "Scripts panel"),
    row(Ctx::Edit, "LMB", "Place"),
    row(Ctx::Edit, "R", "Grab object"),
    row(Ctx::Edit, "Delete", "Delete object"),
    row(Ctx::Edit, "Shift+Scroll", "Scale"),
    row(Ctx::Edit, "Alt+Scroll", "Rotate"),
    // Carrying a grabbed object.
    row(Ctx::Carrying, "LMB", "Drop here"),
    row(Ctx::Carrying, "R", "Cancel (restore)"),
    row(Ctx::Carrying, "Delete", "Destroy"),
    row(Ctx::Carrying, "Shift+Scroll", "Scale"),
    row(Ctx::Carrying, "Alt+Scroll", "Rotate"),
    // Open panels.
    row(Ctx::Catalog, "Scroll", "Cycle meshes"),
    row(Ctx::Scripts, "Scroll", "Cycle scripts"),
    row(Ctx::Scripts, "Enter", "Attach script"),
    row(Ctx::Scripts, "Backspace", "Detach script"),
    // Terrain sculpting.
    row(Ctx::Terrain, "LMB / RMB", "Raise / lower"),
    row(Ctx::Terrain, "[ / ]", "Brush size"),
];

/// The sub-modes that are active this frame. Compared frame to frame so the
/// panel only rebuilds when something actually changed.
#[derive(PartialEq, Eq, Clone, Copy, Default)]
pub struct ControlContext {
    edit_mode: bool,
    terrain_edit: bool,
    catalog_open: bool,
    scripts_panel_open: bool,
    carrying: bool,
}

/// Last context the panel was built for.
///
/// A resource rather than a `Local` because the panel is despawned and
/// respawned every time editor mode is toggled: a stale cached context would
/// make `update_controls_panel` skip the rebuild and leave the new panel
/// permanently empty. `setup_controls_panel` clears this.
#[derive(Resource, Default)]
pub struct ControlsCache(Option<ControlContext>);

impl ControlContext {
    fn includes(&self, ctx: Ctx) -> bool {
        match ctx {
            Ctx::Base => true,
            // While carrying, the normal edit-mode click/grab bindings are
            // replaced by the carry ones, so the two are mutually exclusive.
            Ctx::Edit => self.edit_mode && !self.carrying,
            Ctx::Carrying => self.carrying,
            Ctx::Catalog => self.catalog_open,
            Ctx::Scripts => self.scripts_panel_open,
            Ctx::Terrain => self.terrain_edit,
        }
    }
}

/// Spawn the panel. Mirrors `setup_debug_hud`: `UiAnchors` only exists once
/// the game is in `AppState::InGame`, so this is a no-op outside it.
pub fn setup_controls_panel(
    mut commands: Commands,
    state: Res<State<AppState>>,
    anchors: Option<Res<UiAnchors>>,
    theme: Res<UiTheme>,
    mut cache: ResMut<ControlsCache>,
) {
    if *state.get() != AppState::InGame {
        return;
    }
    let Some(anchors) = anchors else {
        return;
    };

    // Fresh panel, so whatever context the previous one was built for no
    // longer applies.
    cache.0 = None;

    let panel = commands
        .spawn((
            GameEntity,
            ControlsDisplay,
            Node {
                flex_direction: FlexDirection::Column,
                padding: UiRect::all(Val::Px(theme.pad_md)),
                ..default()
            },
            BackgroundColor(theme.bg_panel),
        ))
        .id();
    commands.entity(anchors.top_left).add_child(panel);
}

pub fn cleanup_controls_panel(mut commands: Commands, query: Query<Entity, With<ControlsDisplay>>) {
    for entity in query.iter() {
        commands.entity(entity).despawn();
    }
}

/// Rebuild the row list whenever the active context changes.
pub fn update_controls_panel(
    mut commands: Commands,
    settings: Res<UserSettings>,
    catalog_open: Res<MeshSelectorOpen>,
    scripts_open: Res<ScriptPanelOpen>,
    grabbed: Res<GrabbedEntity>,
    theme: Res<UiTheme>,
    query: Query<Entity, With<ControlsDisplay>>,
    mut cache: ResMut<ControlsCache>,
) {
    let context = ControlContext {
        edit_mode: settings.edit_mode,
        terrain_edit: settings.terrain_edit_mode,
        catalog_open: catalog_open.0,
        scripts_panel_open: scripts_open.0,
        carrying: grabbed.0.is_some(),
    };

    if cache.0 == Some(context) {
        return;
    }
    cache.0 = Some(context);

    for entity in query.iter() {
        commands.entity(entity).despawn_related::<Children>();
        commands
            .entity(entity)
            .with_children(|parent| spawn_rows(parent, &context, &theme));
    }
}

fn spawn_rows(
    parent: &mut RelatedSpawnerCommands<'_, ChildOf>,
    context: &ControlContext,
    theme: &UiTheme,
) {
    parent.spawn((
        Text::new("Editor Controls"),
        TextFont {
            font_size: theme.size_body,
            ..default()
        },
        TextColor(theme.text_white),
        Node {
            margin: UiRect::bottom(Val::Px(theme.pad_sm)),
            ..default()
        },
    ));

    parent
        .spawn(Node {
            flex_direction: FlexDirection::Column,
            row_gap: Val::Px(theme.pad_sm),
            ..default()
        })
        .with_children(|col| {
            for control in CONTROLS.iter().filter(|c| context.includes(c.ctx)) {
                col.spawn(Node {
                    flex_direction: FlexDirection::Row,
                    justify_content: JustifyContent::SpaceBetween,
                    column_gap: Val::Px(theme.pad_md),
                    width: Val::Px(220.0),
                    ..default()
                })
                .with_children(|row_node| {
                    row_node.spawn((
                        Text::new(control.keys),
                        TextFont {
                            font_size: theme.size_label,
                            ..default()
                        },
                        TextColor(theme.text_accent),
                    ));
                    row_node.spawn((
                        Text::new(control.action),
                        TextFont {
                            font_size: theme.size_label,
                            ..default()
                        },
                        TextColor(theme.text_dim),
                    ));
                });
            }
        });
}
