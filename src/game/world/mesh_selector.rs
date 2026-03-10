use bevy::{
    asset::{AssetLoader, LoadContext, io::Reader},
    input::mouse::{MouseScrollUnit, MouseWheel},
    prelude::*,
};

use crate::{
    game::{GameEntity, world::objects::PlacementConfig},
    settings::UserSettings,
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

/// Spawn the bottom strip when `MeshSelectorOpen` flips to `true`.
pub fn setup_mesh_selector_panel(
    mut commands: Commands,
    selector: Res<MeshSelectorOpen>,
    catalog: Res<MeshCatalog>,
    config: Res<PlacementConfig>,
    mut selector_index: ResMut<MeshSelectorIndex>,
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

    // Root strip (absolute, pinned to bottom)
    let root = commands
        .spawn((
            GameEntity,
            MeshSelectorPanel,
            Node {
                position_type: PositionType::Absolute,
                bottom: Val::Px(0.0),
                left: Val::Px(0.0),
                right: Val::Px(0.0),
                height: Val::Px(130.0),
                overflow: Overflow::clip(),
                ..default()
            },
            BackgroundColor(Color::srgba(0.0, 0.0, 0.0, 0.8)),
            GlobalZIndex(50),
        ))
        .id();

    // Inner scrollable row
    let row = commands
        .spawn((
            GameEntity,
            MeshSelectorScrollable,
            Node {
                flex_direction: FlexDirection::Row,
                align_items: AlignItems::Center,
                height: Val::Px(130.0),
                overflow: Overflow::clip(),
                padding: UiRect::axes(Val::Px(8.0), Val::Px(0.0)),
                ..default()
            },
            ScrollPosition::DEFAULT,
        ))
        .id();

    commands.entity(root).add_child(row);

    for entry in &catalog.entries {
        let is_selected = entry.asset_path == selected_path;
        let card_color = if is_selected {
            Color::srgb(0.3, 0.6, 1.0)
        } else {
            Color::srgb(0.15, 0.15, 0.15)
        };

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
                    width: Val::Px(100.0),
                    height: Val::Px(110.0),
                    margin: UiRect::all(Val::Px(8.0)),
                    ..default()
                },
                BackgroundColor(card_color),
            ))
            .with_child((
                GameEntity,
                Text::new(entry.name.clone()),
                TextFont {
                    font_size: 12.0,
                    ..default()
                },
                TextColor(Color::WHITE),
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
/// Updates `MeshSelectorIndex`, `PlacementConfig`, card highlights, and auto-scrolls the panel.
/// Shift+scroll is reserved for placement scale adjustment.
pub fn scroll_mesh_selector(
    mut scroll_events: MessageReader<MouseWheel>,
    input: Res<ButtonInput<KeyCode>>,
    mut scroll_query: Query<&mut ScrollPosition, With<MeshSelectorScrollable>>,
    mut card_query: Query<(&MeshButton, &mut BackgroundColor)>,
    catalog: Res<MeshCatalog>,
    mut selector_index: ResMut<MeshSelectorIndex>,
    mut config: ResMut<PlacementConfig>,
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
    let new_path = catalog.entries[new_index].asset_path.clone();
    config.asset_path = new_path.clone();
    config.scale = Vec3::splat(1.0);

    // Refresh card highlights
    for (button, mut bg) in card_query.iter_mut() {
        bg.0 = if button.asset_path == new_path {
            Color::srgb(0.3, 0.6, 1.0)
        } else {
            Color::srgb(0.15, 0.15, 0.15)
        };
    }

    // Auto-scroll the panel so the selected card is visible
    const CARD_STRIDE: f32 = 116.0;
    let target_scroll = new_index as f32 * CARD_STRIDE;
    for mut scroll_pos in scroll_query.iter_mut() {
        scroll_pos.0.x = target_scroll;
    }
}
