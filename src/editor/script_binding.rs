//! Scripts panel (K in edit mode): attach plugin scripts to placed entities.
//!
//! The panel lists every script the loaded plugins export as `plugin/script`
//! cards, plus a leading "No stamp" card. Scrolling selects a card and arms
//! **stamp mode** — every entity placed from then on gets that binding — until
//! "No stamp" is selected again. With the panel open:
//!
//! - **Enter** attaches the selected script to the placed entity under the
//!   crosshair, seeded with its schema defaults.
//! - **Backspace** detaches it (or, on "No stamp", detaches everything).
//!
//! A status line shows what the crosshair is on and which bindings it carries,
//! so the result of an attach is visible without any other tooling. Attaching
//! only edits `ScriptBindings` and re-arms `NeedsHydration`; the host's
//! hydration pass turns that into plugin components on the next frame.

use bevy::{
    input::mouse::{MouseScrollUnit, MouseWheel},
    prelude::*,
    window::PrimaryWindow,
};
use engine_api::{ScriptBinding, ScriptBindings, ScriptRegistry};

use super::mesh_selector::MeshSelectorOpen;
use super::object_placement::PlacedPicker;
use crate::{
    game::{
        GameEntity,
        world::objects::{PlacedObject, PlacedObjectRegistry, PlacementConfig},
    },
    projects::ActiveProject,
    script_host::{LoadedPluginIds, NeedsHydration},
    settings::UserSettings,
    ui::{UiAnchors, UiTheme},
};

// ── Resources ─────────────────────────────────────────────────────────────────

#[derive(Resource, Default)]
pub struct ScriptPanelOpen(pub bool);

/// Selected card. `0` is always "No stamp"; `i >= 1` indexes
/// `ScriptPanelEntries.scripts[i - 1]`.
#[derive(Resource, Default)]
pub struct ScriptPanelIndex(pub usize);

/// Snapshot of the registry taken when the panel is (re)built, so card
/// indices stay stable while it is open.
#[derive(Resource, Default)]
pub struct ScriptPanelEntries {
    pub scripts: Vec<(String, String)>,
}

impl ScriptPanelEntries {
    fn from_registry(registry: &ScriptRegistry) -> Self {
        let mut scripts: Vec<(String, String)> = registry
            .scripts()
            .map(|s| (s.plugin.clone(), s.schema.name.to_owned()))
            .collect();
        scripts.sort();
        Self { scripts }
    }

    fn card_count(&self) -> usize {
        self.scripts.len() + 1
    }

    /// The `(plugin, script)` behind a card index; `None` for "No stamp".
    fn at(&self, index: usize) -> Option<&(String, String)> {
        index.checked_sub(1).and_then(|i| self.scripts.get(i))
    }
}

// ── Components ────────────────────────────────────────────────────────────────

#[derive(Component)]
pub struct ScriptPanel;

#[derive(Component)]
pub struct ScriptPanelScrollable;

#[derive(Component)]
pub struct ScriptPanelStatus;

#[derive(Component)]
pub struct ScriptCard {
    pub index: usize,
}

// ── Run conditions ────────────────────────────────────────────────────────────

pub fn script_panel_open(panel: Res<ScriptPanelOpen>) -> bool {
    panel.0
}

// ── Systems ───────────────────────────────────────────────────────────────────

pub fn toggle_script_panel(
    mut panel: ResMut<ScriptPanelOpen>,
    mut mesh_selector: ResMut<MeshSelectorOpen>,
    settings: Res<UserSettings>,
    input: Res<ButtonInput<KeyCode>>,
) {
    if settings.is_changed() && !settings.edit_mode {
        panel.0 = false;
        return;
    }
    if !settings.edit_mode {
        return;
    }
    // Plain K: cafe-sim already uses B for its shop, and Shift+<letter> is the
    // engine's own toggle convention, so this stays out of both namespaces.
    if input.just_pressed(KeyCode::KeyK) {
        panel.0 = !panel.0;
        if panel.0 {
            mesh_selector.0 = false;
        }
    }
}

const CARD_W: f32 = 140.0;
const CARD_H_NORMAL: f32 = 60.0;
const CARD_H_SELECTED: f32 = 76.0;
const CARD_MARGIN: f32 = 8.0;
const STRIP_H: f32 = 96.0;
const STATUS_H: f32 = 40.0;

/// Build or tear down the panel UI to match `ScriptPanelOpen`, and rebuild it
/// while open whenever the registry changes (plugin load/unload/reload).
pub fn sync_script_panel(
    mut commands: Commands,
    panel: Res<ScriptPanelOpen>,
    registry: Res<ScriptRegistry>,
    config: Res<PlacementConfig>,
    mut entries: ResMut<ScriptPanelEntries>,
    mut index: ResMut<ScriptPanelIndex>,
    existing: Query<Entity, With<ScriptPanel>>,
    anchors: Res<UiAnchors>,
    theme: Res<UiTheme>,
    windows: Query<&Window, With<PrimaryWindow>>,
) {
    let needs_rebuild = panel.0 && (panel.is_changed() || registry.is_changed());
    let needs_teardown = !panel.0 && panel.is_changed();
    if !needs_rebuild && !needs_teardown {
        return;
    }

    for e in existing.iter() {
        commands.entity(e).despawn();
    }
    if needs_teardown {
        return;
    }

    *entries = ScriptPanelEntries::from_registry(&registry);

    // Highlight whatever stamp is currently armed, or "No stamp".
    index.0 = config
        .stamp_script
        .as_ref()
        .and_then(|s| entries.scripts.iter().position(|e| e == s))
        .map(|i| i + 1)
        .unwrap_or(0);

    let window_width = windows.single().map(|w| w.width()).unwrap_or(1280.0);
    let stride = CARD_W + CARD_MARGIN * 2.0;
    let initial_scroll = (index.0 as f32 * stride - (window_width / 2.0 - stride / 2.0)).max(0.0);

    let root = commands
        .spawn((
            GameEntity,
            ScriptPanel,
            Node {
                width: Val::Percent(100.0),
                height: Val::Px(STRIP_H + STATUS_H),
                flex_direction: FlexDirection::Column,
                overflow: Overflow::clip(),
                ..default()
            },
            BackgroundColor(theme.bg_panel),
            GlobalZIndex(50),
        ))
        .id();
    commands.entity(anchors.bottom).add_child(root);

    let status = commands
        .spawn((
            GameEntity,
            ScriptPanelStatus,
            Text::new("Scripts — look at a placed entity"),
            TextFont {
                font_size: theme.size_label,
                ..default()
            },
            TextColor(theme.text_dim),
            Node {
                height: Val::Px(STATUS_H),
                padding: UiRect::axes(Val::Px(theme.pad_md), Val::Px(theme.pad_sm)),
                ..default()
            },
        ))
        .id();
    commands.entity(root).add_child(status);

    let row = commands
        .spawn((
            GameEntity,
            ScriptPanelScrollable,
            Node {
                flex_direction: FlexDirection::Row,
                align_items: AlignItems::Center,
                height: Val::Px(STRIP_H),
                overflow: Overflow::clip(),
                padding: UiRect::axes(Val::Px(CARD_MARGIN), Val::Px(0.0)),
                ..default()
            },
            ScrollPosition(Vec2::new(initial_scroll, 0.0)),
        ))
        .id();
    commands.entity(root).add_child(row);

    let labels = std::iter::once("No stamp".to_owned())
        .chain(entries.scripts.iter().map(|(p, s)| format!("{p}/{s}")));

    for (card_index, label) in labels.enumerate() {
        let selected = card_index == index.0;
        let card = commands
            .spawn((
                GameEntity,
                ScriptCard { index: card_index },
                Node {
                    justify_content: JustifyContent::Center,
                    align_items: AlignItems::Center,
                    width: Val::Px(CARD_W),
                    height: Val::Px(if selected {
                        CARD_H_SELECTED
                    } else {
                        CARD_H_NORMAL
                    }),
                    margin: UiRect::all(Val::Px(CARD_MARGIN)),
                    overflow: Overflow::clip(),
                    ..default()
                },
                BackgroundColor(if selected {
                    theme.highlight
                } else {
                    theme.bg_surface
                }),
            ))
            .with_child((
                GameEntity,
                Text::new(label),
                TextFont {
                    font_size: theme.size_caption,
                    ..default()
                },
                TextColor(theme.text_white),
                Node {
                    max_width: Val::Px(CARD_W - 8.0),
                    ..default()
                },
            ))
            .id();
        commands.entity(row).add_child(card);
    }
}

/// Why the script list is empty, phrased for the person looking at it.
///
/// The common cause is a plugin the project declares but whose dylib was never
/// built — `cargo run` builds only the host, not the plugin crates — and the
/// loader reports that as a single `warn!` that is easy to miss. The pause
/// menu's plugin picker lists plugin *source* directories, so adding a plugin
/// that has never been compiled looks like it worked.
fn empty_reason(active: Option<&ActiveProject>, loaded: &LoadedPluginIds) -> String {
    let Some(active) = active else {
        return "No scripts — no active project".to_owned();
    };
    let missing: Vec<&str> = active
        .0
        .plugins
        .iter()
        .filter(|p| !loaded.0.contains(p))
        .map(|p| p.as_str())
        .collect();

    if !missing.is_empty() {
        return format!(
            "No scripts — declared but not loaded: {}. Build them: cargo build --workspace",
            missing.join(", ")
        );
    }
    if active.0.plugins.is_empty() {
        return "No scripts — this project declares no plugins (add one in the pause menu)"
            .to_owned();
    }
    format!(
        "No scripts — {} loaded but none export any scripts",
        active.0.plugins.join(", ")
    )
}

/// Hard teardown for `OnExit(EditorMode::Editor)`.
pub fn teardown_script_panel(
    mut commands: Commands,
    mut panel: ResMut<ScriptPanelOpen>,
    existing: Query<Entity, With<ScriptPanel>>,
) {
    panel.0 = false;
    for e in existing.iter() {
        commands.entity(e).despawn();
    }
}

/// Scroll selects a card and arms (or clears) stamp mode.
pub fn scroll_script_panel(
    mut scroll_events: MessageReader<MouseWheel>,
    input: Res<ButtonInput<KeyCode>>,
    entries: Res<ScriptPanelEntries>,
    mut index: ResMut<ScriptPanelIndex>,
    mut config: ResMut<PlacementConfig>,
    mut scroll_query: Query<&mut ScrollPosition, With<ScriptPanelScrollable>>,
    mut card_query: Query<(&ScriptCard, &mut BackgroundColor, &mut Node)>,
    theme: Res<UiTheme>,
    windows: Query<&Window, With<PrimaryWindow>>,
) {
    let shift = input.pressed(KeyCode::ShiftLeft) || input.pressed(KeyCode::ShiftRight);
    let mut delta = 0.0_f32;
    for event in scroll_events.read() {
        if shift {
            continue;
        }
        delta += match event.unit {
            MouseScrollUnit::Line => event.y,
            MouseScrollUnit::Pixel => event.y / 40.0,
        };
    }
    if delta.abs() < 0.5 {
        return;
    }

    let step: i32 = if delta > 0.0 { -1 } else { 1 };
    let new_index = (index.0 as i32 + step).clamp(0, entries.card_count() as i32 - 1) as usize;
    if new_index == index.0 {
        return;
    }
    index.0 = new_index;

    // Bypass change detection: arming a stamp must not rebuild the placement
    // preview (that's keyed on `PlacementConfig` changing).
    config.bypass_change_detection().stamp_script = entries.at(new_index).cloned();

    for (card, mut bg, mut node) in card_query.iter_mut() {
        let selected = card.index == new_index;
        bg.0 = if selected {
            theme.highlight
        } else {
            theme.bg_surface
        };
        node.height = Val::Px(if selected {
            CARD_H_SELECTED
        } else {
            CARD_H_NORMAL
        });
    }

    let window_width = windows.single().map(|w| w.width()).unwrap_or(1280.0);
    let stride = CARD_W + CARD_MARGIN * 2.0;
    let target = (new_index as f32 * stride - (window_width / 2.0 - stride / 2.0)).max(0.0);
    for mut scroll in scroll_query.iter_mut() {
        scroll.0.x = target;
    }
}

/// Enter attaches the selected script to the entity under the crosshair;
/// Backspace detaches it ("No stamp" detaches all).
pub fn handle_script_panel_keys(
    mut commands: Commands,
    input: Res<ButtonInput<KeyCode>>,
    entries: Res<ScriptPanelEntries>,
    index: Res<ScriptPanelIndex>,
    script_registry: Res<ScriptRegistry>,
    mut placed_registry: ResMut<PlacedObjectRegistry>,
    mut picker: PlacedPicker,
    placed_query: Query<&PlacedObject>,
    mut bindings_query: Query<&mut ScriptBindings>,
) {
    let attach = input.just_pressed(KeyCode::Enter);
    let detach = input.just_pressed(KeyCode::Backspace);
    if !attach && !detach {
        return;
    }

    let Some(target) = picker.pick() else {
        info!("Scripts panel: nothing under the crosshair");
        return;
    };
    let Ok(obj) = placed_query.get(target) else {
        return;
    };
    let selected = entries.at(index.0);

    if attach {
        let Some((plugin, script)) = selected else {
            info!("Scripts panel: select a script card first (not 'No stamp')");
            return;
        };
        if let Ok(existing) = bindings_query.get(target)
            && existing
                .iter()
                .any(|b| &b.plugin == plugin && &b.script == script)
        {
            info!(
                "Scripts panel: {plugin}/{script} already attached to id={}",
                obj.id
            );
            return;
        }

        let props = script_registry
            .get(plugin, script)
            .map(|r| r.schema.default_props())
            .unwrap_or_default();
        let binding = ScriptBinding::new(plugin.clone(), script.clone()).with_props(props);

        match bindings_query.get_mut(target) {
            Ok(mut existing) => existing.0.push(binding),
            Err(_) => {
                commands
                    .entity(target)
                    .insert(ScriptBindings(vec![binding]));
            }
        }
        commands.entity(target).insert(NeedsHydration);
        placed_registry.dirty_chunks.insert(obj.chunk);
        info!("Attached {plugin}/{script} to placed id={}", obj.id);
        return;
    }

    // Detach.
    let Ok(mut existing) = bindings_query.get_mut(target) else {
        info!("Scripts panel: id={} has no scripts to detach", obj.id);
        return;
    };

    let removed: Vec<ScriptBinding> = match selected {
        Some((plugin, script)) => {
            let (gone, kept): (Vec<_>, Vec<_>) = existing
                .0
                .drain(..)
                .partition(|b| &b.plugin == plugin && &b.script == script);
            existing.0 = kept;
            gone
        }
        None => existing.0.drain(..).collect(),
    };

    if removed.is_empty() {
        info!("Scripts panel: that script isn't attached to id={}", obj.id);
        return;
    }
    if existing.0.is_empty() {
        commands
            .entity(target)
            .remove::<(ScriptBindings, NeedsHydration)>();
    }
    for b in &removed {
        let (plugin, script) = (b.plugin.clone(), b.script.clone());
        commands.queue(move |world: &mut World| {
            crate::script_host::detach_binding(world, target, &plugin, &script);
        });
        info!(
            "Detached {}/{} from placed id={}",
            b.plugin, b.script, obj.id
        );
    }
    placed_registry.dirty_chunks.insert(obj.chunk);
}

/// Per-frame status line: what the crosshair is on, its bindings, and the
/// armed stamp. This is the feedback loop that makes attach/detach testable
/// without reading logs.
pub fn update_script_panel_status(
    mut status_query: Query<(&mut Text, &mut TextColor), With<ScriptPanelStatus>>,
    config: Res<PlacementConfig>,
    entries: Res<ScriptPanelEntries>,
    active: Option<Res<ActiveProject>>,
    loaded: Res<LoadedPluginIds>,
    mut picker: PlacedPicker,
    placed_query: Query<(&PlacedObject, Option<&ScriptBindings>, Has<NeedsHydration>)>,
    theme: Res<UiTheme>,
) {
    let Ok((mut text, mut color)) = status_query.single_mut() else {
        return;
    };

    // Nothing to attach: explain why rather than showing an empty strip.
    if entries.scripts.is_empty() {
        **text = empty_reason(active.as_deref(), &loaded);
        color.0 = theme.danger;
        return;
    }

    let stamp = match &config.stamp_script {
        Some((p, s)) => format!("stamp: {p}/{s}"),
        None => "stamp: off".to_owned(),
    };

    match picker.pick().and_then(|e| placed_query.get(e).ok()) {
        Some((obj, bindings, pending)) => {
            let list = bindings
                .map(|b| {
                    b.iter()
                        .map(|x| format!("{}/{}", x.plugin, x.script))
                        .collect::<Vec<_>>()
                        .join(", ")
                })
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "none".to_owned());
            let state = if pending { " (awaiting plugin)" } else { "" };
            let name = if obj.name.is_empty() {
                obj.kind.label().to_owned()
            } else {
                obj.name.clone()
            };
            **text = format!(
                "{name} id={}  scripts: {list}{state}   |   {stamp}   |   Enter attach · Backspace detach",
                obj.id
            );
            color.0 = theme.text_white;
        }
        None => {
            **text = format!("Look at a placed entity   |   {stamp}   |   scroll to pick a script");
            color.0 = theme.text_dim;
        }
    }
}
