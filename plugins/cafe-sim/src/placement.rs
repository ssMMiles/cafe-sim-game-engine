use bevy::prelude::*;
use engine_api::{AutoCollider, PlayerCamera};
use std::collections::HashMap;

use crate::catalog::{PieceDef, PieceKind};
use crate::grid::{FLOOR_Y, PIECE_SCALE, camera_floor_hit, cell_center, world_to_cell};
use crate::menus::ActiveMenu;
use crate::spawn::SpawnOwnedCommands;

/// Currently selected piece for placement.
///
/// `piece` is the `'static` catalog reference used by every readonly site
/// (preview rendering, grid hints, click-target gating, etc.).
/// `indices` is the parallel `(category, piece)` lookup key, captured at
/// shop-card click time and threaded into `spawn_piece` so placed entities
/// get a stable `CafePiece` tag for save serialization.
///
/// The indices live next to the reference (rather than being recovered
/// via pointer comparison) because `pub const CATALOG` does not guarantee
/// a stable address — the compiler can inline `const` accesses into
/// separate copies, making pointer-equality lookups unreliable.
#[derive(Resource, Default)]
pub struct PlacementSelection {
    pub piece: Option<&'static PieceDef>,
    pub indices: Option<(u8, u8)>,
}

impl PlacementSelection {
    pub fn set(&mut self, piece: &'static PieceDef, indices: (u8, u8)) {
        self.piece = Some(piece);
        self.indices = Some(indices);
    }

    pub fn clear(&mut self) {
        self.piece = None;
        self.indices = None;
    }
}

#[derive(Resource, Default)]
pub struct GridOccupancy(pub HashMap<(i32, i32), Entity>);

#[derive(Resource, Default)]
pub struct PreviewEntity(pub Option<Entity>);

#[derive(Component)]
pub struct PlacementPreview;

/// Queues a one-shot material override on every mesh descendant of this entity
/// once the scene has finished spawning. Used to tint pieces by their catalog
/// color for both the translucent placement preview and the placed instance.
#[derive(Component)]
pub struct PaintMaterial {
    pub color: Color,
    pub alpha: f32,
    pub unlit: bool,
}

#[derive(Component)]
pub struct PlacementHint;

#[derive(Component)]
pub struct Countertop;

#[derive(Component)]
pub struct CoffeeMachine;

#[derive(Component)]
pub struct GarbageCan;

/// Cell coordinates of a placed piece, for systems that need to walk to or
/// reason about a specific tile (e.g. customers heading to a counter).
#[derive(Component, Clone, Copy)]
pub struct PlacedCell(pub i32, pub i32);

/// Stable identifier of a placed piece's catalog entry, for save serialization.
/// Captured at spawn time from the active `PlacementSelection` (or from save
/// data on load) and never mutated afterwards.
#[derive(Component, Clone, Copy)]
pub struct CafePiece {
    pub category_idx: u8,
    pub piece_idx: u8,
}

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

    if let Some(piece) = selection.piece {
        let scene: Handle<Scene> = asset_server.load(piece.asset_path);
        let mut preview = commands.spawn_owned((
            PlacementPreview,
            SceneRoot(scene),
            Transform::from_xyz(0.0, FLOOR_Y, 0.0).with_scale(Vec3::splat(PIECE_SCALE)),
            Visibility::Hidden,
        ));
        if piece.tint {
            preview.insert(PaintMaterial {
                color: piece.color(),
                alpha: 0.55,
                unlit: false,
            });
        }
        preview_entity.0 = Some(preview.id());
    }
}

/// Polls each frame: as soon as a `PaintMaterial` root has spawned its mesh
/// descendants, replace their materials with the requested tint.
pub fn apply_paint_when_loaded(
    mut commands: Commands,
    mut materials: ResMut<Assets<StandardMaterial>>,
    needs_q: Query<(Entity, &PaintMaterial)>,
    children_q: Query<&Children>,
    mesh_q: Query<(), With<Mesh3d>>,
) {
    for (root, paint) in needs_q.iter() {
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

        let srgba = paint.color.to_srgba();
        let alpha_mode = if paint.alpha < 1.0 {
            AlphaMode::Blend
        } else {
            AlphaMode::Opaque
        };
        let tinted = materials.add(StandardMaterial {
            base_color: Color::srgba(srgba.red, srgba.green, srgba.blue, paint.alpha),
            alpha_mode,
            unlit: paint.unlit,
            ..default()
        });

        for &e in &descendants {
            if mesh_q.contains(e) {
                commands.entity(e).insert(MeshMaterial3d(tinted.clone()));
            }
        }

        commands.entity(root).remove::<PaintMaterial>();
    }
}

pub fn update_preview_transform(
    preview_entity: Res<PreviewEntity>,
    mut preview_q: Query<(&mut Transform, &mut Visibility), With<PlacementPreview>>,
    cam_q: Query<&Transform, (With<PlayerCamera>, Without<PlacementPreview>)>,
    occupancy: Res<GridOccupancy>,
    active: Res<ActiveMenu>,
) {
    let Some(e) = preview_entity.0 else {
        return;
    };
    let Ok((mut t, mut vis)) = preview_q.get_mut(e) else {
        return;
    };

    if active.is_open() {
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
    active: Res<ActiveMenu>,
    selection: Res<PlacementSelection>,
    mouse: Res<ButtonInput<MouseButton>>,
    cam_q: Query<&Transform, With<PlayerCamera>>,
    mut occupancy: ResMut<GridOccupancy>,
) {
    if active.is_open() || !mouse.just_pressed(MouseButton::Left) {
        return;
    }
    let (Some(piece), Some(indices)) = (selection.piece, selection.indices) else {
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

    spawn_piece(
        &mut commands,
        &asset_server,
        piece,
        indices,
        (i, j),
        &mut occupancy,
    );
}

/// Spawn a catalog piece at the given grid cell, register it in the
/// occupancy map, and tag it with [`CafePiece`] so the save layer can
/// re-serialize it later. Shared between the live "click to place" path
/// and the save-load hydration path; the caller is responsible for
/// passing the correct catalog indices for `piece`.
pub fn spawn_piece(
    commands: &mut Commands,
    asset_server: &AssetServer,
    piece: &'static PieceDef,
    indices: (u8, u8),
    (i, j): (i32, i32),
    occupancy: &mut GridOccupancy,
) -> Entity {
    let scene: Handle<Scene> = asset_server.load(piece.asset_path);
    let mut placed = commands.spawn_owned((
        SceneRoot(scene),
        Transform::from_translation(cell_center(i, j)).with_scale(Vec3::splat(PIECE_SCALE)),
        AutoCollider,
        PlacedCell(i, j),
        CafePiece {
            category_idx: indices.0,
            piece_idx: indices.1,
        },
    ));
    if piece.tint {
        placed.insert(PaintMaterial {
            color: piece.color(),
            alpha: 1.0,
            unlit: false,
        });
    }
    match piece.kind {
        PieceKind::Countertop => {
            placed.insert(Countertop);
        }
        PieceKind::CoffeeMachine => {
            placed.insert(CoffeeMachine);
        }
        PieceKind::GarbageCan => {
            placed.insert(GarbageCan);
        }
        PieceKind::Decoration => {}
    }
    let entity = placed.id();
    occupancy.0.insert((i, j), entity);
    entity
}

pub fn cancel_placement(
    input: Res<ButtonInput<KeyCode>>,
    active: Res<ActiveMenu>,
    mut selection: ResMut<PlacementSelection>,
) {
    if active.is_open() || selection.piece.is_none() {
        return;
    }
    if input.just_pressed(KeyCode::KeyC) {
        selection.clear();
    }
}

pub fn manage_placement_hint(
    mut commands: Commands,
    selection: Res<PlacementSelection>,
    active: Res<ActiveMenu>,
    existing: Query<Entity, With<PlacementHint>>,
) {
    let should_show = selection.piece.is_some() && !active.is_open();

    if should_show && existing.is_empty() {
        let bg = Color::srgba(0.0, 0.0, 0.0, 0.55);
        let text_color = Color::srgb(0.95, 0.95, 0.95);

        let root = commands
            .spawn_owned((
                PlacementHint,
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
            .spawn_owned((
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
