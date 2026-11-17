use bevy::prelude::*;
use engine_api::{
    AutoCollider, ConvertToToon, DefaultToonLightmap, DynamicLit, GltfSurface, PlayerCamera,
    ProbeVolumeImages, ToonMaterial, UnlitMaterial,
};
use std::collections::HashMap;

use crate::catalog::{PieceDef, PieceKind};
use crate::grid::{CafeAnchor, Footprint, PIECE_SCALE, Turn};
use crate::menus::ActiveMenu;
use crate::shading::{ProbeLitColors, flat, probe_lit};
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
    /// How the piece is turned (R). Kept across pieces: having turned the
    /// counter to run along the wall, the next piece runs that way too.
    pub turn: Turn,
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

/// Which placed piece stands on each cell. A piece with a footprint wider
/// than one cell appears once per cell it covers.
#[derive(Resource, Default)]
pub struct GridOccupancy(pub HashMap<(i32, i32), Entity>);

impl GridOccupancy {
    /// Nothing stands on any of the footprint's cells.
    pub fn is_free(&self, footprint: &Footprint) -> bool {
        footprint.cells().all(|c| !self.0.contains_key(&c))
    }

    /// On the floor and unoccupied: the piece can go here.
    pub fn accepts(&self, footprint: &Footprint, anchor: &CafeAnchor) -> bool {
        footprint.inside(anchor) && self.is_free(footprint)
    }
}

/// Where the piece being placed would land this frame, for the floor
/// outline. `None` when nothing is being placed or the cursor is off the floor.
#[derive(Resource, Default)]
pub struct PlacementCursor {
    pub footprint: Option<Footprint>,
    pub accepted: bool,
}

#[derive(Resource, Default)]
pub struct PreviewEntity(pub Option<Entity>);

#[derive(Component)]
pub struct PlacementPreview;

/// Queues a one-shot material override on every mesh descendant of this entity
/// once the scene has finished spawning. Used to tint pieces by their catalog
/// color for the translucent placement preview, the shop thumbnails and the
/// placed instance.
#[derive(Component)]
pub struct PaintMaterial {
    pub color: Color,
    pub alpha: f32,
    pub shading: PaintShading,
}

/// How a tinted piece is shaded. See [`crate::shading`].
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum PaintShading {
    /// A translucent silhouette in the colour — the placement ghost
    /// (`UnlitMaterial`, the scene pass's transparent layer).
    Ghost,
    /// The colour, flat and opaque, unlit — the shop thumbnails, which have
    /// no lights and want to read as a swatch. `alpha` is ignored.
    Flat,
    /// The toon shading the level uses, lit from the probe volume where the
    /// piece stands. Opaque only; `alpha` is ignored.
    ProbeLit,
}

#[derive(Component)]
pub struct PlacementHint;

#[derive(Component)]
pub struct Countertop;

#[derive(Component)]
pub struct CoffeeMachine;

#[derive(Component)]
pub struct GarbageCan;

/// The origin cell of a placed piece — the minimum corner of its footprint,
/// which for a one-cell piece is simply its cell. Systems that walk to or
/// put things on a piece (customers heading to a counter, drinks set down on
/// it) use this cell; a wide counter's full extent is [`PlacedTurn`] applied
/// to its catalog footprint.
#[derive(Component, Clone, Copy)]
pub struct PlacedCell(pub i32, pub i32);

/// How a placed piece is turned. With [`PlacedCell`] and the catalog
/// footprint it gives the cells the piece covers.
#[derive(Component, Clone, Copy, Default)]
pub struct PlacedTurn(pub Turn);

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
    anchor: Res<CafeAnchor>,
    mut preview_entity: ResMut<PreviewEntity>,
) {
    if !selection.is_changed() {
        return;
    }

    if let Some(e) = preview_entity.0.take() {
        commands.entity(e).despawn();
    }

    if let Some(piece) = selection.piece {
        let scene: Handle<WorldAsset> = asset_server.load(piece.asset_path);
        // Always a ghost, whatever the piece's own materials: nothing draws
        // an unconverted glTF in this engine, so an untinted preview left
        // alone would be invisible.
        let preview = commands.spawn_owned((
            PlacementPreview,
            WorldAssetRoot(scene),
            anchor.place(piece_transform(Vec3::ZERO, standing(piece, selection.turn))),
            Visibility::Hidden,
            PaintMaterial {
                color: ghost_color(piece),
                alpha: 0.55,
                shading: PaintShading::Ghost,
            },
        ));
        preview_entity.0 = Some(preview.id());
    }
}

/// Polls each frame: as soon as a `PaintMaterial` root has spawned its mesh
/// descendants, replace their materials with the requested tint.
pub fn apply_paint_when_loaded(
    mut commands: Commands,
    mut unlit_materials: ResMut<Assets<UnlitMaterial>>,
    mut toon_materials: ResMut<Assets<ToonMaterial>>,
    mut colors: ResMut<ProbeLitColors>,
    lightmap: Res<DefaultToonLightmap>,
    probes: Res<ProbeVolumeImages>,
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

        let meshes = descendants.iter().copied().filter(|&e| mesh_q.contains(e));
        match paint.shading {
            PaintShading::Ghost => {
                let srgba = paint.color.to_srgba();
                let tinted = unlit_materials.add(UnlitMaterial::new(Color::srgba(
                    srgba.red,
                    srgba.green,
                    srgba.blue,
                    paint.alpha,
                )));
                for e in meshes {
                    commands
                        .entity(e)
                        .remove::<GltfSurface>()
                        .insert(MeshMaterial3d(tinted.clone()));
                }
            }
            PaintShading::Flat => {
                let tinted = flat(
                    paint.color,
                    &mut colors,
                    &mut toon_materials,
                    &lightmap,
                    &probes,
                );
                for e in meshes {
                    commands
                        .entity(e)
                        .remove::<GltfSurface>()
                        .insert(MeshMaterial3d(tinted.clone()));
                }
            }
            PaintShading::ProbeLit => {
                let tinted = probe_lit(
                    paint.color,
                    &mut colors,
                    &mut toon_materials,
                    &lightmap,
                    &probes,
                );
                for e in meshes {
                    commands
                        .entity(e)
                        .remove::<GltfSurface>()
                        .insert(MeshMaterial3d(tinted.clone()));
                }
            }
        }

        commands.entity(root).remove::<PaintMaterial>();
    }
}

/// How a piece actually stands: its authored facing, then the player's turn.
/// Footprint and mesh rotation both follow this.
fn standing(piece: &PieceDef, turn: Turn) -> Turn {
    piece.facing.then(turn)
}

/// Where the piece under the cursor would go: the footprint of `piece`,
/// turned as selected, around the cell the camera is looking at.
fn footprint_under_cursor(
    piece: &PieceDef,
    turn: Turn,
    cam: &Transform,
    anchor: &CafeAnchor,
) -> Option<Footprint> {
    let hit = anchor.camera_floor_hit(cam)?;
    let hovered = anchor.world_to_cell(hit)?;
    Some(Footprint::around(
        hovered,
        piece.footprint(),
        standing(piece, turn),
    ))
}

/// The placement ghost's colour: the catalog tint where the piece is
/// tinted, a neutral cream where it keeps its own materials.
fn ghost_color(piece: &PieceDef) -> Color {
    if piece.tint {
        piece.color()
    } else {
        Color::srgb(0.92, 0.88, 0.78)
    }
}

/// A piece's shell-local transform: standing at `centre`, turned, at
/// furniture scale. Composed with the shell by [`CafeAnchor::place`].
fn piece_transform(centre: Vec3, turn: Turn) -> Transform {
    Transform::from_translation(centre)
        .with_rotation(turn.rotation())
        .with_scale(Vec3::splat(PIECE_SCALE))
}

pub fn update_preview_transform(
    preview_entity: Res<PreviewEntity>,
    mut preview_q: Query<(&mut Transform, &mut Visibility), With<PlacementPreview>>,
    cam_q: Query<&Transform, (With<PlayerCamera>, Without<PlacementPreview>)>,
    occupancy: Res<GridOccupancy>,
    selection: Res<PlacementSelection>,
    active: Res<ActiveMenu>,
    anchor: Res<CafeAnchor>,
    mut cursor: ResMut<PlacementCursor>,
) {
    cursor.footprint = None;
    cursor.accepted = false;
    let Some(e) = preview_entity.0 else {
        return;
    };
    let Ok((mut t, mut vis)) = preview_q.get_mut(e) else {
        return;
    };
    let Some(piece) = selection.piece else {
        *vis = Visibility::Hidden;
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

    let Some(footprint) = footprint_under_cursor(piece, selection.turn, cam, &anchor) else {
        *vis = Visibility::Hidden;
        return;
    };
    let accepted = occupancy.accepts(&footprint, &anchor);
    cursor.footprint = Some(footprint);
    cursor.accepted = accepted;
    if !accepted {
        *vis = Visibility::Hidden;
        return;
    }

    // Rewritten in full rather than translated: the shell can be turned or
    // scaled while the preview is up.
    *t = anchor.place(piece_transform(
        footprint.centre_local(&anchor),
        standing(piece, selection.turn),
    ));
    *vis = Visibility::Visible;
}

/// R turns the piece being placed a quarter turn.
pub fn rotate_placement(
    input: Res<ButtonInput<KeyCode>>,
    active: Res<ActiveMenu>,
    mut selection: ResMut<PlacementSelection>,
) {
    if active.is_open() || selection.piece.is_none() {
        return;
    }
    if input.just_pressed(KeyCode::KeyR) {
        selection.turn = selection.turn.next();
    }
}

pub fn handle_place_click(
    mut commands: Commands,
    asset_server: Res<AssetServer>,
    active: Res<ActiveMenu>,
    selection: Res<PlacementSelection>,
    mouse: Res<ButtonInput<MouseButton>>,
    cam_q: Query<&Transform, With<PlayerCamera>>,
    anchor: Res<CafeAnchor>,
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
    let Some(footprint) = footprint_under_cursor(piece, selection.turn, cam, &anchor) else {
        return;
    };
    if !occupancy.accepts(&footprint, &anchor) {
        return;
    }

    spawn_piece(
        &mut commands,
        &asset_server,
        piece,
        indices,
        footprint.origin,
        selection.turn,
        &mut occupancy,
        &anchor,
    );
}

/// Spawn a catalog piece with its footprint's minimum corner at `origin`,
/// turned by `turn`; register every cell it covers in the occupancy map and
/// tag it with [`CafePiece`] so the save layer can re-serialize it later.
/// Shared between the live "click to place" path and the save-load hydration
/// path; the caller checks the footprint fits ([`GridOccupancy::accepts`])
/// and passes the correct catalog indices for `piece`.
pub fn spawn_piece(
    commands: &mut Commands,
    asset_server: &AssetServer,
    piece: &'static PieceDef,
    indices: (u8, u8),
    origin: (i32, i32),
    turn: Turn,
    occupancy: &mut GridOccupancy,
    anchor: &CafeAnchor,
) -> Entity {
    let stands = standing(piece, turn);
    let footprint = Footprint::at(origin, piece.footprint(), stands);
    let scene: Handle<WorldAsset> = asset_server.load(piece.asset_path);
    let mut placed = commands.spawn_owned((
        WorldAssetRoot(scene),
        anchor.place(piece_transform(footprint.centre_local(anchor), stands)),
        AutoCollider,
        PlacedCell(origin.0, origin.1),
        PlacedTurn(turn),
        CafePiece {
            category_idx: indices.0,
            piece_idx: indices.1,
        },
    ));
    // Lit like the room it stands in, from the probe volume (see
    // `shading`). A tinted piece gets its colour's shared toon material
    // once its meshes exist; an untinted one keeps its glTF materials and
    // asks the host to convert them.
    if piece.tint {
        placed.insert(PaintMaterial {
            color: piece.color(),
            alpha: 1.0,
            shading: PaintShading::ProbeLit,
        });
    } else {
        placed.insert((ConvertToToon, DynamicLit));
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
    for cell in footprint.cells() {
        occupancy.0.insert(cell, entity);
    }
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
                    margin: UiRect::left(Val::Px(-120.0)),
                    width: Val::Px(240.0),
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
                Text::new("R to turn  -  C to cancel"),
                TextFont {
                    font_size: FontSize::Px(16.0),
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
