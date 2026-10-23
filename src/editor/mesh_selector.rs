//! Bottom-of-screen placement catalog (Tab in edit mode).
//!
//! Entries come from three sources, merged into one scrollable strip:
//! 1. meshes listed in the build-time `meshes/manifest.manifest`,
//! 2. two built-in light kinds,
//! 3. one preset per plugin script that declares a `default_mesh` — picking it
//!    selects that mesh *and* arms stamp mode with the script, so a plugin's
//!    catalog entry is a one-click "mesh + binding" placement.
//!
//! The strip is rebuilt whenever the script registry changes (plugin load,
//! unload, hot-reload) so presets track the loaded plugins.

use bevy::{
    asset::{AssetLoader, LoadContext, io::Reader},
    input::mouse::{MouseScrollUnit, MouseWheel},
    prelude::*,
    window::PrimaryWindow,
};
use engine_api::ScriptRegistry;

use crate::{
    game::{
        GameEntity,
        world::objects::{PlacedKind, PlacementConfig},
    },
    settings::UserSettings,
    ui::{UiAnchors, UiTheme},
};

// ── Data types ────────────────────────────────────────────────────────────────

/// A raw line from the mesh manifest.
#[derive(Clone, Debug)]
pub struct MeshEntry {
    pub name: String,
    pub asset_path: String,
}

/// One selectable card in the strip.
#[derive(Clone, Debug)]
pub struct CatalogEntry {
    pub name: String,
    pub kind: PlacedKind,
    /// Set for plugin presets: selecting the card also arms stamp mode with
    /// this `(plugin, script)`.
    pub preset_script: Option<(String, String)>,
}

#[derive(Resource, Default)]
pub struct MeshCatalog {
    /// Manifest meshes, kept so the strip can be rebuilt when presets change.
    manifest: Vec<MeshEntry>,
    manifest_loaded: bool,
    pub entries: Vec<CatalogEntry>,
}

impl MeshCatalog {
    /// Recompose `entries` from the manifest, the built-in lights, and the
    /// registry's presets.
    fn rebuild(&mut self, registry: &ScriptRegistry) {
        let mut entries: Vec<CatalogEntry> = self
            .manifest
            .iter()
            .map(|m| CatalogEntry {
                name: m.name.clone(),
                kind: PlacedKind::mesh(m.asset_path.clone()),
                preset_script: None,
            })
            .collect();

        entries.push(CatalogEntry {
            name: "Point Light".into(),
            kind: PlacedKind::point_light(),
            preset_script: None,
        });
        entries.push(CatalogEntry {
            name: "Spot Light".into(),
            kind: PlacedKind::spot_light(),
            preset_script: None,
        });

        let mut presets: Vec<CatalogEntry> = registry
            .scripts()
            .filter_map(|s| {
                let mesh = s.schema.default_mesh?;
                Some(CatalogEntry {
                    name: format!("{}/{}", s.plugin, s.schema.name),
                    kind: PlacedKind::mesh(mesh),
                    preset_script: Some((s.plugin.clone(), s.schema.name.to_owned())),
                })
            })
            .collect();
        // HashMap iteration order is arbitrary; keep the strip stable.
        presets.sort_by(|a, b| a.name.cmp(&b.name));
        entries.extend(presets);

        self.entries = entries;
    }
}

/// Whether the catalog strip is currently shown.
#[derive(Resource, Default)]
pub struct MeshSelectorOpen(pub bool);

#[derive(Resource, Default)]
pub struct MeshSelectorIndex(pub usize);

// ── Manifest asset ────────────────────────────────────────────────────────────

#[derive(Asset, TypePath)]
pub struct MeshManifestAsset(pub Vec<MeshEntry>);

#[derive(Resource)]
pub struct MeshManifestHandle(pub Handle<MeshManifestAsset>);

#[derive(Default, TypePath)]
pub struct MeshManifestLoader;

impl AssetLoader for MeshManifestLoader {
    type Asset = MeshManifestAsset;
    type Settings = ();
    type Error = std::io::Error;

    async fn load(
        &self,
        reader: &mut dyn Reader,
        _settings: &Self::Settings,
        _load_context: &mut LoadContext<'_>,
    ) -> Result<Self::Asset, Self::Error> {
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes).await?;
        let text = String::from_utf8_lossy(&bytes);

        let entries = text
            .lines()
            .filter(|l| !l.trim().is_empty())
            .filter_map(|line| {
                let (name, path) = line.split_once('|')?;
                Some(MeshEntry {
                    name: name.to_string(),
                    asset_path: path.to_string(),
                })
            })
            .collect();

        Ok(MeshManifestAsset(entries))
    }

    fn extensions(&self) -> &[&str] {
        &["manifest"]
    }
}

// ── Components ────────────────────────────────────────────────────────────────

#[derive(Component)]
pub struct MeshSelectorPanel;

#[derive(Component)]
pub struct MeshSelectorScrollable;

#[derive(Component)]
pub struct MeshButton {
    pub index: usize,
}

// ── Run conditions ────────────────────────────────────────────────────────────

pub fn mesh_selector_open(selector: Res<MeshSelectorOpen>) -> bool {
    selector.0
}

// ── Systems ───────────────────────────────────────────────────────────────────

pub fn request_mesh_manifest(mut commands: Commands, asset_server: Res<AssetServer>) {
    let handle = asset_server.load::<MeshManifestAsset>("meshes/manifest.manifest");
    commands.insert_resource(MeshManifestHandle(handle));
}

/// Ingest the manifest once it loads, then keep `entries` in step with the
/// script registry so plugin presets appear and disappear with their plugins.
pub fn populate_mesh_catalog(
    mut commands: Commands,
    handle: Option<Res<MeshManifestHandle>>,
    manifests: Res<Assets<MeshManifestAsset>>,
    asset_server: Res<AssetServer>,
    registry: Res<ScriptRegistry>,
    mut catalog: ResMut<MeshCatalog>,
    mut selector_index: ResMut<MeshSelectorIndex>,
    mut config: ResMut<PlacementConfig>,
) {
    let mut manifest_changed = false;

    if let Some(handle) = handle {
        match asset_server.load_state(&handle.0) {
            bevy::asset::LoadState::Loaded => {
                if let Some(manifest) = manifests.get(&handle.0) {
                    catalog.manifest = manifest.0.clone();
                    catalog.manifest_loaded = true;
                    manifest_changed = true;
                    info!("Loaded {} meshes from manifest", catalog.manifest.len());
                    commands.remove_resource::<MeshManifestHandle>();
                }
            }
            bevy::asset::LoadState::Failed(err) => {
                warn!("Failed to load mesh manifest: {err:?} — catalog will have no meshes");
                catalog.manifest_loaded = true;
                manifest_changed = true;
                commands.remove_resource::<MeshManifestHandle>();
            }
            _ => {}
        }
    }

    if !manifest_changed && !registry.is_changed() {
        return;
    }

    catalog.rebuild(&registry);

    // The selected card may have moved or vanished (a preset whose plugin
    // unloaded). Re-find it by identity; fall back to the first entry.
    let current = catalog
        .entries
        .iter()
        .position(|e| e.kind == config.kind && e.preset_script == config.stamp_script)
        .or_else(|| catalog.entries.iter().position(|e| e.kind == config.kind))
        .unwrap_or(0);
    selector_index.0 = current;

    // First rebuild: make sure the config's kind is something the strip
    // actually offers, so the preview and the highlight agree.
    if manifest_changed
        && let Some(entry) = catalog.entries.get(current)
        && entry.kind != config.kind
    {
        config.kind = entry.kind.clone();
    }
}

pub fn toggle_mesh_selector(
    mut selector: ResMut<MeshSelectorOpen>,
    mut script_panel: ResMut<super::script_binding::ScriptPanelOpen>,
    settings: Res<UserSettings>,
    input: Res<ButtonInput<KeyCode>>,
) {
    if settings.is_changed() && !settings.edit_mode {
        selector.0 = false;
        return;
    }

    if !settings.edit_mode {
        return;
    }

    if input.just_pressed(KeyCode::Tab) {
        selector.0 = !selector.0;
        // Both panels own the scroll wheel, so only one is open at a time.
        if selector.0 {
            script_panel.0 = false;
        }
    }
}

const CARD_W: f32 = 100.0;
const CARD_H_NORMAL: f32 = 104.0;
const CARD_H_SELECTED: f32 = 128.0;
const CARD_MARGIN: f32 = 8.0;
pub const CARD_STRIDE: f32 = CARD_W + CARD_MARGIN * 2.0;
const PANEL_H: f32 = 152.0;

/// Build the strip when it opens, and rebuild it while open whenever the
/// catalog changes (plugin load/unload shifts the preset cards, and stale
/// cards would point `MeshButton::index` at the wrong entries).
pub fn setup_mesh_selector_panel(
    mut commands: Commands,
    selector: Res<MeshSelectorOpen>,
    catalog: Res<MeshCatalog>,
    mut selector_index: ResMut<MeshSelectorIndex>,
    existing: Query<Entity, With<MeshSelectorPanel>>,
    anchors: Res<UiAnchors>,
    theme: Res<UiTheme>,
    windows: Query<&Window, With<PrimaryWindow>>,
) {
    if !selector.0 || !(selector.is_changed() || catalog.is_changed()) {
        return;
    }
    for e in existing.iter() {
        commands.entity(e).despawn();
    }

    let selected_index = selector_index
        .0
        .min(catalog.entries.len().saturating_sub(1));
    selector_index.0 = selected_index;

    let window_width = windows.single().map(|w| w.width()).unwrap_or(1280.0);
    let initial_scroll =
        (selected_index as f32 * CARD_STRIDE - (window_width / 2.0 - CARD_STRIDE / 2.0)).max(0.0);

    let root = commands
        .spawn((
            GameEntity,
            MeshSelectorPanel,
            Node {
                width: Val::Percent(100.0),
                height: Val::Px(PANEL_H),
                overflow: Overflow::clip(),
                ..default()
            },
            BackgroundColor(theme.bg_panel),
            GlobalZIndex(50),
        ))
        .id();

    commands.entity(anchors.bottom).add_child(root);

    let row = commands
        .spawn((
            GameEntity,
            MeshSelectorScrollable,
            Node {
                flex_direction: FlexDirection::Row,
                align_items: AlignItems::Center,
                height: Val::Px(PANEL_H),
                overflow: Overflow::clip(),
                padding: UiRect::axes(Val::Px(CARD_MARGIN), Val::Px(0.0)),
                ..default()
            },
            ScrollPosition(Vec2::new(initial_scroll, 0.0)),
        ))
        .id();

    commands.entity(root).add_child(row);

    for (index, entry) in catalog.entries.iter().enumerate() {
        let is_selected = index == selected_index;
        let card_color = if is_selected {
            theme.highlight
        } else {
            theme.bg_surface
        };
        let card_height = if is_selected {
            CARD_H_SELECTED
        } else {
            CARD_H_NORMAL
        };

        // Lights and presets get a small kind caption so they stand out from
        // plain meshes in a long strip.
        let caption = if entry.preset_script.is_some() {
            Some("preset")
        } else if !entry.kind.is_mesh() {
            Some(entry.kind.label())
        } else {
            None
        };

        let card = commands
            .spawn((
                GameEntity,
                MeshButton { index },
                Node {
                    flex_direction: FlexDirection::Column,
                    justify_content: JustifyContent::Center,
                    align_items: AlignItems::Center,
                    width: Val::Px(CARD_W),
                    height: Val::Px(card_height),
                    margin: UiRect::all(Val::Px(CARD_MARGIN)),
                    overflow: Overflow::clip(),
                    row_gap: Val::Px(2.0),
                    ..default()
                },
                BackgroundColor(card_color),
            ))
            .with_child((
                GameEntity,
                Text::new(entry.name.clone()),
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

        if let Some(caption) = caption {
            commands.entity(card).with_child((
                GameEntity,
                Text::new(caption),
                TextFont {
                    font_size: theme.size_caption - 2.0,
                    ..default()
                },
                TextColor(theme.text_accent),
            ));
        }

        commands.entity(row).add_child(card);
    }
}

pub fn cleanup_mesh_selector_panel(
    mut commands: Commands,
    selector: Res<MeshSelectorOpen>,
    panel_query: Query<Entity, With<MeshSelectorPanel>>,
) {
    if !selector.is_changed() || selector.0 {
        return;
    }

    for entity in panel_query.iter() {
        commands.entity(entity).despawn();
    }
}

/// Hard teardown for `OnExit(EditorMode::Editor)` — closes the panel and
/// despawns its UI regardless of `is_changed` state.
pub fn teardown_mesh_selector(
    mut commands: Commands,
    mut selector: ResMut<MeshSelectorOpen>,
    panel_query: Query<Entity, With<MeshSelectorPanel>>,
) {
    selector.0 = false;
    for entity in panel_query.iter() {
        commands.entity(entity).despawn();
    }
}

pub fn scroll_mesh_selector(
    mut scroll_events: MessageReader<MouseWheel>,
    input: Res<ButtonInput<KeyCode>>,
    mut scroll_query: Query<&mut ScrollPosition, With<MeshSelectorScrollable>>,
    mut card_query: Query<(&MeshButton, &mut BackgroundColor, &mut Node)>,
    catalog: Res<MeshCatalog>,
    mut selector_index: ResMut<MeshSelectorIndex>,
    mut config: ResMut<PlacementConfig>,
    theme: Res<UiTheme>,
    windows: Query<&Window, With<PrimaryWindow>>,
) {
    if catalog.entries.is_empty() {
        return;
    }

    let shift = input.pressed(KeyCode::ShiftLeft) || input.pressed(KeyCode::ShiftRight);

    let mut delta = 0.0_f32;
    for event in scroll_events.read() {
        if shift {
            continue;
        }
        let y = match event.unit {
            MouseScrollUnit::Line => event.y,
            MouseScrollUnit::Pixel => event.y / 40.0,
        };
        delta += y;
    }

    if delta.abs() < 0.5 {
        return;
    }

    let step: i32 = if delta > 0.0 { -1 } else { 1 };
    let current = selector_index.0 as i32;
    let new_index = (current + step).clamp(0, catalog.entries.len() as i32 - 1) as usize;

    if new_index == selector_index.0 {
        return;
    }

    selector_index.0 = new_index;
    let entry = &catalog.entries[new_index];
    config.kind = entry.kind.clone();
    config.scale = Vec3::splat(1.0);
    // A preset arms stamp mode; a plain entry leaves whatever the scripts
    // panel last set, so stamping is sticky across mesh changes.
    if let Some(preset) = &entry.preset_script {
        config.stamp_script = Some(preset.clone());
    }

    for (button, mut bg, mut node) in card_query.iter_mut() {
        let selected = button.index == new_index;
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
    let target_scroll =
        (new_index as f32 * CARD_STRIDE - (window_width / 2.0 - CARD_STRIDE / 2.0)).max(0.0);
    for mut scroll_pos in scroll_query.iter_mut() {
        scroll_pos.0.x = target_scroll;
    }
}
