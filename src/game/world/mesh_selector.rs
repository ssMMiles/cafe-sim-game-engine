use bevy::{
    asset::{AssetLoader, LoadContext, io::Reader},
    input::mouse::{MouseScrollUnit, MouseWheel},
    prelude::*,
    window::PrimaryWindow,
};

use crate::{
    game::{GameEntity, world::objects::PlacementConfig},
    settings::UserSettings,
    ui::{UiAnchors, UiTheme},
};

// ── Data types ────────────────────────────────────────────────────────────────

#[derive(Clone, Debug)]
pub struct MeshEntry {
    pub name: String,
    pub asset_path: String,
}

#[derive(Resource, Default)]
pub struct MeshCatalog {
    pub entries: Vec<MeshEntry>,
}

/// Whether the mesh-selector panel is currently shown.
#[derive(Resource, Default)]
pub struct MeshSelectorOpen(pub bool);

/// Index of the currently selected entry in `MeshCatalog`.
#[derive(Resource, Default)]
pub struct MeshSelectorIndex(pub usize);

// ── Manifest asset ────────────────────────────────────────────────────────────

/// Parsed contents of `meshes/manifest.manifest`.
#[derive(Asset, TypePath)]
pub struct MeshManifestAsset(pub Vec<MeshEntry>);

/// Handle kept alive until the manifest has been consumed.
#[derive(Resource)]
pub struct MeshManifestHandle(pub Handle<MeshManifestAsset>);

/// Asset loader for `.manifest` files — expects pipe-delimited `name|asset_path` lines.
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

/// Marker on the root UI node of the panel.
#[derive(Component)]
pub struct MeshSelectorPanel;

/// Marker on the inner scrollable row so the scroll system can find it.
#[derive(Component)]
pub struct MeshSelectorScrollable;

/// Attached to each card button; stores the asset path it represents.
#[derive(Component)]
pub struct MeshButton {
    pub asset_path: String,
}

// ── Run conditions ────────────────────────────────────────────────────────────

pub fn mesh_selector_open(selector: Res<MeshSelectorOpen>) -> bool {
    selector.0
}

// ── Systems ───────────────────────────────────────────────────────────────────

/// Kicks off loading of `meshes/manifest.manifest` via the asset server.
pub fn request_mesh_manifest(mut commands: Commands, asset_server: Res<AssetServer>) {
    let handle = asset_server.load::<MeshManifestAsset>("meshes/manifest.manifest");
    commands.insert_resource(MeshManifestHandle(handle));
}

/// Polls each frame until the manifest asset is loaded, then populates `MeshCatalog`.
pub fn populate_mesh_catalog(
    mut commands: Commands,
    handle: Option<Res<MeshManifestHandle>>,
    manifests: Res<Assets<MeshManifestAsset>>,
    asset_server: Res<AssetServer>,
    mut catalog: ResMut<MeshCatalog>,
) {
    let Some(handle) = handle else { return };

    if !catalog.entries.is_empty() {
        return; // already populated
    }

    match asset_server.load_state(&handle.0) {
        bevy::asset::LoadState::Loaded => {
            if let Some(manifest) = manifests.get(&handle.0) {
                catalog.entries = manifest.0.clone();
                info!("Loaded {} meshes from manifest", catalog.entries.len());
                commands.remove_resource::<MeshManifestHandle>();
            }
        }
        bevy::asset::LoadState::Failed(err) => {
            warn!("Failed to load mesh manifest: {err:?} — catalog will be empty");
            commands.remove_resource::<MeshManifestHandle>();
        }
        _ => {}
    }
}

/// Tab toggles the panel while in edit mode. Leaving edit mode force-closes it.
pub fn toggle_mesh_selector(
    mut selector: ResMut<MeshSelectorOpen>,
    settings: Res<UserSettings>,
    input: Res<ButtonInput<KeyCode>>,
) {
    // Force close whenever edit mode turns off
    if settings.is_changed() && !settings.edit_mode {
        selector.0 = false;
        return;
    }

    if !settings.edit_mode {
        return;
    }

    if input.just_pressed(KeyCode::Tab) {
        selector.0 = !selector.0;
    }
}

const CARD_W: f32 = 100.0;
const CARD_H_NORMAL: f32 = 104.0;
const CARD_H_SELECTED: f32 = 128.0;
const CARD_MARGIN: f32 = 8.0;
/// Horizontal distance from one card's left edge to the next.
pub const CARD_STRIDE: f32 = CARD_W + CARD_MARGIN * 2.0;
const PANEL_H: f32 = 152.0; // enough to contain the tallest card + margins

/// Spawn the bottom strip when `MeshSelectorOpen` flips to `true`.
pub fn setup_mesh_selector_panel(
    mut commands: Commands,
    selector: Res<MeshSelectorOpen>,
    catalog: Res<MeshCatalog>,
    config: Res<PlacementConfig>,
    mut selector_index: ResMut<MeshSelectorIndex>,
    anchors: Res<UiAnchors>,
    theme: Res<UiTheme>,
    windows: Query<&Window, With<PrimaryWindow>>,
) {
    if !selector.is_changed() || !selector.0 {
        return;
    }

    let selected_path = config.asset_path.clone();

    let selected_index = catalog
        .entries
        .iter()
        .position(|e| e.asset_path == selected_path)
        .unwrap_or(0);
    selector_index.0 = selected_index;

    // Initial scroll offset so selected card is centred in the viewport.
    let window_width = windows.single().map(|w| w.width()).unwrap_or(1280.0);
    let initial_scroll =
        (selected_index as f32 * CARD_STRIDE - (window_width / 2.0 - CARD_STRIDE / 2.0)).max(0.0);

    // Root strip — child of bottom anchor, spans full width.
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

    // Inner scrollable row — cards are vertically centred inside the strip.
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

    for entry in &catalog.entries {
        let is_selected = entry.asset_path == selected_path;
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

        // The label sits inside a fixed-size clipping container so long names
        // never spill outside the card boundary.
        let card = commands
            .spawn((
                GameEntity,
                MeshButton {
                    asset_path: entry.asset_path.clone(),
                },
                Node {
                    flex_direction: FlexDirection::Column,
                    justify_content: JustifyContent::Center,
                    align_items: AlignItems::Center,
                    width: Val::Px(CARD_W),
                    height: Val::Px(card_height),
                    margin: UiRect::all(Val::Px(CARD_MARGIN)),
                    overflow: Overflow::clip(),
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

        commands.entity(row).add_child(card);
    }
}

/// Despawn the panel when `MeshSelectorOpen` flips to `false`.
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

/// Step through the catalog with the mouse wheel (no Shift held).
/// Updates `MeshSelectorIndex`, `PlacementConfig`, card highlights/sizes, and centres the panel.
/// Shift+scroll is reserved for placement scale adjustment.
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

    // Threshold: at least half a line notch
    if delta.abs() < 0.5 {
        return;
    }

    // Scroll up (positive y) = previous entry; scroll down = next entry
    let step: i32 = if delta > 0.0 { -1 } else { 1 };
    let current = selector_index.0 as i32;
    let new_index = (current + step).clamp(0, catalog.entries.len() as i32 - 1) as usize;

    if new_index == selector_index.0 {
        return; // already at boundary
    }

    selector_index.0 = new_index;
    config.asset_path = catalog.entries[new_index].asset_path.clone();
    config.scale = Vec3::splat(1.0);

    // Refresh card highlights and heights
    for (button, mut bg, mut node) in card_query.iter_mut() {
        let selected = button.asset_path == config.asset_path;
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

    // Centre the selected card in the viewport
    let window_width = windows.single().map(|w| w.width()).unwrap_or(1280.0);
    let target_scroll =
        (new_index as f32 * CARD_STRIDE - (window_width / 2.0 - CARD_STRIDE / 2.0)).max(0.0);
    for mut scroll_pos in scroll_query.iter_mut() {
        scroll_pos.0.x = target_scroll;
    }
}
