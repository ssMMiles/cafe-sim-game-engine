use std::fs;

use bevy::{
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

/// Walks `assets/meshes/` on startup and builds the `MeshCatalog`.
pub fn discover_meshes(mut catalog: ResMut<MeshCatalog>) {
    let meshes_dir = std::path::Path::new("assets/meshes");
    let Ok(top_entries) = fs::read_dir(meshes_dir) else {
        warn!("Could not read assets/meshes/ — mesh catalog will be empty");
        return;
    };

    let mut entries: Vec<MeshEntry> = Vec::new();

    for dir_entry in top_entries.flatten() {
        let dir_path = dir_entry.path();
        if !dir_path.is_dir() {
            continue;
        }

        let dir_name = dir_path
            .file_name()
            .unwrap()
            .to_string_lossy()
            .to_string();

        // Skip the player-only model directory
        if dir_name == "character" {
            continue;
        }

        let Ok(files) = fs::read_dir(&dir_path) else {
            continue;
        };

        for file_entry in files.flatten() {
            let file_path = file_entry.path();
            let ext = file_path
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("");
            if ext != "gltf" && ext != "glb" {
                continue;
            }

            let file_name = file_path
                .file_name()
                .unwrap()
                .to_string_lossy()
                .to_string();
            let asset_path = format!("meshes/{dir_name}/{file_name}#Scene0");

            entries.push(MeshEntry {
                name: dir_name.clone(),
                asset_path,
            });

            // Only use the first GLTF/GLB found per directory
            break;
        }
    }

    entries.sort_by(|a, b| a.name.cmp(&b.name));
    info!("Discovered {} meshes for placement", entries.len());
    catalog.entries = entries;
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
