use bevy::prelude::*;
use engine_api::{AutoCollider, PlayerCamera, ScriptOwned};
use std::collections::HashMap;

use crate::PLUGIN_NAME;
use crate::catalog::PieceDef;
use crate::grid::{FLOOR_Y, PIECE_SCALE, camera_floor_hit, cell_center, world_to_cell};
use crate::shop::ShopOpen;

#[derive(Resource, Default)]
pub struct PlacementSelection(pub Option<&'static PieceDef>);

#[derive(Resource, Default)]
pub struct GridOccupancy(pub HashMap<(i32, i32), Entity>);

#[derive(Resource, Default)]
pub struct PreviewEntity(pub Option<Entity>);

#[derive(Component)]
pub struct PlacementPreview;

#[derive(Component)]
pub struct NeedsTranslucent;

#[derive(Component)]
pub struct PlacementHint;

pub fn sync_preview(
    mut commands: Commands,
    asset_server: Res<AssetServer>,
    selection: Res<PlacementSelection>,
    mut preview_entity: ResMut<PreviewEntity>,
) {
    if !selection.is_changed() {
        return;
    }

    if let Some(e) = preview_entity.0.take() {
        commands.entity(e).despawn();
    }

    if let Some(piece) = selection.0 {
        let scene: Handle<Scene> = asset_server.load(piece.asset_path);
        let entity = commands
            .spawn((
                ScriptOwned::new(PLUGIN_NAME),
                PlacementPreview,
                NeedsTranslucent,
                SceneRoot(scene),
                Transform::from_xyz(0.0, FLOOR_Y, 0.0).with_scale(Vec3::splat(PIECE_SCALE)),
                Visibility::Hidden,
            ))
            .id();
        preview_entity.0 = Some(entity);
    }
}

/// Polls each frame: as soon as a NeedsTranslucent root has spawned its mesh
/// descendants, replace their materials with a translucent override.
pub fn apply_translucent_when_loaded(
    mut commands: Commands,
    mut materials: ResMut<Assets<StandardMaterial>>,
    needs_q: Query<Entity, With<NeedsTranslucent>>,
    children_q: Query<&Children>,
    mesh_q: Query<(), With<Mesh3d>>,
) {
    for root in needs_q.iter() {
        let mut stack = vec![root];
        let mut descendants: Vec<Entity> = Vec::new();
        while let Some(e) = stack.pop() {
            if let Ok(children) = children_q.get(e) {
                for c in children.iter() {
                    descendants.push(c);
                    stack.push(c);
                }
            }
        }

        let has_mesh = descendants.iter().any(|&e| mesh_q.contains(e));
        if !has_mesh {
            continue;
        }

        let translucent = materials.add(StandardMaterial {
            base_color: Color::srgba(0.7, 0.85, 1.0, 0.15),
            alpha_mode: AlphaMode::Blend,
            unlit: true,
            ..default()
        });

        for &e in &descendants {
            if mesh_q.contains(e) {
                commands
                    .entity(e)
                    .insert(MeshMaterial3d(translucent.clone()));
            }
        }

        commands.entity(root).remove::<NeedsTranslucent>();
    }
}

pub fn update_preview_transform(
    preview_entity: Res<PreviewEntity>,
    mut preview_q: Query<(&mut Transform, &mut Visibility), With<PlacementPreview>>,
    cam_q: Query<&Transform, (With<PlayerCamera>, Without<PlacementPreview>)>,
    occupancy: Res<GridOccupancy>,
    shop: Res<ShopOpen>,
) {
    let Some(e) = preview_entity.0 else {
        return;
    };
    let Ok((mut t, mut vis)) = preview_q.get_mut(e) else {
        return;
    };

    if shop.0 {
        *vis = Visibility::Hidden;
        return;
    }

    let Ok(cam) = cam_q.single() else {
        *vis = Visibility::Hidden;
        return;
    };

    let Some(hit) = camera_floor_hit(cam) else {
        *vis = Visibility::Hidden;
        return;
    };
    let Some((i, j)) = world_to_cell(hit) else {
        *vis = Visibility::Hidden;
        return;
    };
    if occupancy.0.contains_key(&(i, j)) {
        *vis = Visibility::Hidden;
        return;
    }

    t.translation = cell_center(i, j);
    *vis = Visibility::Visible;
}

pub fn handle_place_click(
    mut commands: Commands,
    asset_server: Res<AssetServer>,
    shop: Res<ShopOpen>,
    selection: Res<PlacementSelection>,
    mouse: Res<ButtonInput<MouseButton>>,
    cam_q: Query<&Transform, With<PlayerCamera>>,
    mut occupancy: ResMut<GridOccupancy>,
) {
    if shop.0 || !mouse.just_pressed(MouseButton::Left) {
        return;
    }
    let Some(piece) = selection.0 else {
        return;
    };
    let Ok(cam) = cam_q.single() else {
        return;
    };
    let Some(hit) = camera_floor_hit(cam) else {
        return;
    };
    let Some((i, j)) = world_to_cell(hit) else {
        return;
    };
    if occupancy.0.contains_key(&(i, j)) {
        return;
    }

    let scene: Handle<Scene> = asset_server.load(piece.asset_path);
    let entity = commands
        .spawn((
            ScriptOwned::new(PLUGIN_NAME),
            SceneRoot(scene),
            Transform::from_translation(cell_center(i, j)).with_scale(Vec3::splat(PIECE_SCALE)),
            AutoCollider,
        ))
        .id();
    occupancy.0.insert((i, j), entity);
}

pub fn cancel_placement(
    input: Res<ButtonInput<KeyCode>>,
    shop: Res<ShopOpen>,
    mut selection: ResMut<PlacementSelection>,
) {
    if shop.0 || selection.0.is_none() {
        return;
    }
    if input.just_pressed(KeyCode::KeyC) {
        selection.0 = None;
    }
}

pub fn manage_placement_hint(
    mut commands: Commands,
    selection: Res<PlacementSelection>,
    shop: Res<ShopOpen>,
    existing: Query<Entity, With<PlacementHint>>,
) {
    let should_show = selection.0.is_some() && !shop.0;

    if should_show && existing.is_empty() {
        let bg = Color::srgba(0.0, 0.0, 0.0, 0.55);
        let text_color = Color::srgb(0.95, 0.95, 0.95);

        let root = commands
            .spawn((
                PlacementHint,
                ScriptOwned::new(PLUGIN_NAME),
                Node {
                    position_type: PositionType::Absolute,
                    bottom: Val::Px(48.0),
                    left: Val::Percent(50.0),
                    margin: UiRect::left(Val::Px(-110.0)),
                    width: Val::Px(220.0),
                    padding: UiRect::axes(Val::Px(16.0), Val::Px(8.0)),
                    justify_content: JustifyContent::Center,
                    align_items: AlignItems::Center,
                    ..default()
                },
                BackgroundColor(bg),
                GlobalZIndex(90),
            ))
            .id();

        let label = commands
            .spawn((
                ScriptOwned::new(PLUGIN_NAME),
                Text::new("Press C to cancel"),
                TextFont {
                    font_size: 16.0,
                    ..default()
                },
                TextColor(text_color),
            ))
            .id();
        commands.entity(root).add_child(label);
    } else if !should_show && !existing.is_empty() {
        for e in existing.iter() {
            commands.entity(e).despawn();
        }
    }
}
