//! Terrain tool: sculpt the heightmap under the cursor.
//!
//! Three modes ([`BrushMode`]): sculpt (LMB raises, Ctrl+LMB lowers — RMB is
//! the editor's look-drag), flatten (pull heights toward the height sampled
//! under the cursor when the stroke began), and smooth (pull each tile toward
//! its neighbourhood average). The brush follows [`EditorRay`], so it lands
//! wherever the cursor is on the viewport.

use std::collections::{HashMap, HashSet};

use avian3d::prelude::Collider;
use bevy::{
    camera::primitives::Aabb,
    ecs::system::SystemParam,
    light::NotShadowCaster,
    picking::mesh_picking::ray_cast::{MeshRayCast, MeshRayCastSettings},
    prelude::*,
};

use crate::editor::history::{Change, History};
use crate::editor::picking::{EditorRay, ViewportInput};
use crate::game::world::terrain::{
    CHUNK_SIZE, HEIGHT_SCALE, HeightmapData, TILE_SIZE, TerrainChunk,
    bootstrap::heightfield_collider,
    mesh::{LOD_LEVELS, generate_lod_mesh},
    storage::TerrainChunkRegistry,
};

pub const BRUSH_RADIUS_RANGE: std::ops::RangeInclusive<f32> = 1.0..=50.0;
pub const BRUSH_STRENGTH_RANGE: std::ops::RangeInclusive<f32> = 5.0..=200.0;

/// How the brush modifies the heightmap.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum BrushMode {
    /// LMB raises, Ctrl+LMB lowers.
    #[default]
    Sculpt,
    /// Pull heights toward the height under the cursor at stroke start.
    Flatten,
    /// Pull each tile toward the average of its neighbours.
    Smooth,
}

impl BrushMode {
    pub fn label(self) -> &'static str {
        match self {
            BrushMode::Sculpt => "Sculpt",
            BrushMode::Flatten => "Flatten",
            BrushMode::Smooth => "Smooth",
        }
    }
}

#[derive(Resource, Debug)]
pub struct BrushSettings {
    pub mode: BrushMode,
    /// Brush radius in metres.
    pub radius: f32,
    /// Height change per second at full weight, in pre-scaled world units.
    /// For flatten/smooth this caps how fast heights approach their target.
    pub strength: f32,
    /// Fraction of the radius over which influence eases to zero at the rim
    /// (0 = hard-edged, 1 = fades from the centre).
    pub falloff: f32,
}

impl Default for BrushSettings {
    fn default() -> Self {
        Self {
            mode: BrushMode::default(),
            radius: 5.0,
            strength: 50.0,
            falloff: 1.0,
        }
    }
}

#[derive(Resource, Default)]
pub struct TerrainBrushHit {
    pub hit: Option<Vec3>,
    pub pending_full_lod: HashSet<(i32, i32)>,
    pub was_painting: bool,
    /// Flatten target, sampled under the cursor at stroke start.
    pub flatten_target: f32,
    /// Height of every tile the current stroke has touched, as it was before
    /// the stroke's first write to it. Becomes the stroke's undo record.
    pub stroke_before: HashMap<(i32, i32), f32>,
}

/// Influence of the brush at `dist` from its centre: 1 inside the flat core,
/// easing (smoothstep) to 0 at the rim. `falloff` is the faded fraction.
fn brush_weight(dist: f32, radius: f32, falloff: f32) -> f32 {
    let inner = radius * (1.0 - falloff.clamp(0.0, 1.0));
    if dist <= inner {
        return 1.0;
    }
    if dist >= radius {
        return 0.0;
    }
    let s = 1.0 - (dist - inner) / (radius - inner);
    s * s * (3.0 - 2.0 * s)
}

pub fn adjust_brush_size(
    mut brush: ResMut<BrushSettings>,
    input: Res<ButtonInput<KeyCode>>,
    time: Res<Time>,
) {
    let dt = time.delta_secs();
    if input.pressed(KeyCode::BracketLeft) {
        brush.radius = (brush.radius - 10.0 * dt).max(*BRUSH_RADIUS_RANGE.start());
    }
    if input.pressed(KeyCode::BracketRight) {
        brush.radius = (brush.radius + 10.0 * dt).min(*BRUSH_RADIUS_RANGE.end());
    }
}

pub fn raycast_terrain_hit(
    ray: Res<EditorRay>,
    terrain_chunk_filter: Query<(), With<TerrainChunk>>,
    mut raycast: MeshRayCast,
    mut brush_hit: ResMut<TerrainBrushHit>,
) {
    let Some(ray) = ray.0 else {
        brush_hit.hit = None;
        return;
    };

    let filter = |entity: Entity| terrain_chunk_filter.contains(entity);
    let hits = raycast.cast_ray(ray, &MeshRayCastSettings::default().with_filter(&filter));
    brush_hit.hit = hits.first().map(|(_, hit_data)| hit_data.point);
}

#[derive(Component)]
pub struct BrushPreviewMarker;

pub fn update_brush_preview(
    brush: Res<BrushSettings>,
    brush_hit: Res<TerrainBrushHit>,
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut preview_query: Query<(&mut Transform, &mut Visibility), With<BrushPreviewMarker>>,
    mut gizmos: Gizmos,
) {
    if preview_query.is_empty() {
        commands.spawn((
            Mesh3d(meshes.add(Circle::new(1.0))),
            MeshMaterial3d(materials.add(StandardMaterial {
                base_color: Color::srgba(0.4, 0.85, 1.0, 0.35),
                alpha_mode: AlphaMode::Blend,
                unlit: true,
                double_sided: true,
                cull_mode: None,
                ..default()
            })),
            Transform::from_rotation(Quat::from_rotation_x(-std::f32::consts::FRAC_PI_2)),
            Visibility::Hidden,
            NotShadowCaster,
            BrushPreviewMarker,
        ));
        return;
    }

    let Ok((mut transform, mut visibility)) = preview_query.single_mut() else {
        return;
    };

    match brush_hit.hit {
        Some(hit) => {
            *visibility = Visibility::Visible;
            transform.translation = hit + Vec3::Y * 0.05;
            transform.scale = Vec3::splat(brush.radius);

            // Ring where the falloff starts, so the flat core is visible.
            let inner = brush.radius * (1.0 - brush.falloff.clamp(0.0, 1.0));
            if inner > 0.05 && brush.falloff > 0.01 {
                gizmos.circle(
                    Isometry3d::new(
                        hit + Vec3::Y * 0.06,
                        Quat::from_rotation_x(-std::f32::consts::FRAC_PI_2),
                    ),
                    inner,
                    Color::srgba(0.4, 0.85, 1.0, 0.6),
                );
            }
        }
        None => {
            *visibility = Visibility::Hidden;
        }
    }
}

/// Despawn the brush preview disc — `OnExit(EditorTool::Terrain)` and
/// `OnExit(EditorMode::Editor)`.
pub fn teardown_terrain_brush_preview(
    mut commands: Commands,
    preview_query: Query<Entity, With<BrushPreviewMarker>>,
    mut brush_hit: ResMut<TerrainBrushHit>,
) {
    brush_hit.hit = None;
    for entity in preview_query.iter() {
        commands.entity(entity).despawn();
    }
}

/// Which way the sculpt brush is pushing this frame, if the button is down
/// over the viewport. Ctrl lowers; plain LMB raises. Flatten/smooth ignore
/// the sign but share the "is the brush painting" test.
fn brush_sign(mouse: &ButtonInput<MouseButton>, keys: &ButtonInput<KeyCode>) -> Option<f32> {
    if !mouse.pressed(MouseButton::Left) {
        return None;
    }
    let ctrl = keys.pressed(KeyCode::ControlLeft) || keys.pressed(KeyCode::ControlRight);
    Some(if ctrl { -1.0 } else { 1.0 })
}

pub fn apply_terrain_brush(
    mouse_input: Res<ButtonInput<MouseButton>>,
    keys: Res<ButtonInput<KeyCode>>,
    viewport: Res<ViewportInput>,
    brush: Res<BrushSettings>,
    mut brush_hit: ResMut<TerrainBrushHit>,
    mut terrain: TerrainChunks,
    time: Res<Time>,
) {
    let Some(hit_point) = brush_hit.hit else {
        return;
    };
    if !viewport.pointer_free() {
        return;
    }
    let Some(sign) = brush_sign(&mouse_input, &keys) else {
        return;
    };

    let Some(heightmap) = terrain.heightmap.as_deref_mut() else {
        return;
    };

    let radius = brush.radius;
    let dt = time.delta_secs();

    let center_tx = (hit_point.x / TILE_SIZE.0) as i32;
    let center_ty = (hit_point.z / TILE_SIZE.1) as i32;
    let extent = (radius / TILE_SIZE.0.min(TILE_SIZE.1)) as i32 + 1;

    // Flatten pulls toward where the terrain was when the stroke began, so a
    // moving brush spreads one plateau instead of chasing its own output.
    if !brush_hit.was_painting {
        brush_hit.flatten_target = heightmap.get_tile(center_tx, center_ty);
        brush_hit.stroke_before.clear();
    }

    // Smooth reads neighbour averages from a snapshot taken before any write
    // this frame — a half-updated map would skew the average directionally.
    // One tile of margin so the rim tiles have a full 3×3 neighbourhood.
    let snap_extent = extent + 1;
    let snap_w = (2 * snap_extent + 1) as usize;
    let snapshot: Option<Vec<f32>> = (brush.mode == BrushMode::Smooth).then(|| {
        let mut v = Vec::with_capacity(snap_w * snap_w);
        for dy in -snap_extent..=snap_extent {
            for dx in -snap_extent..=snap_extent {
                v.push(heightmap.get_tile(center_tx + dx, center_ty + dy));
            }
        }
        v
    });
    let snap = |v: &[f32], tx: i32, ty: i32| -> f32 {
        let lx = (tx - (center_tx - snap_extent)) as usize;
        let ly = (ty - (center_ty - snap_extent)) as usize;
        v[ly * snap_w + lx]
    };

    let mut affected: HashSet<(i32, i32)> = HashSet::new();

    for dx in -extent..=extent {
        for dy in -extent..=extent {
            let tx = center_tx + dx;
            let ty = center_ty + dy;

            let world_x = tx as f32 * TILE_SIZE.0;
            let world_z = ty as f32 * TILE_SIZE.1;
            let dist = Vec2::new(world_x - hit_point.x, world_z - hit_point.z).length();
            if dist > radius {
                continue;
            }

            // `budget` is how far this tile may move this frame: full
            // strength in the core, fading over the falloff band.
            let weight = brush_weight(dist, radius, brush.falloff);
            let budget = brush.strength * weight * dt;
            let current = heightmap.get_tile(tx, ty);
            let new_h = match brush.mode {
                BrushMode::Sculpt => current + budget * sign,
                BrushMode::Flatten => {
                    current + (brush_hit.flatten_target - current).clamp(-budget, budget)
                }
                BrushMode::Smooth => {
                    let v = snapshot.as_deref().expect("snapshot built for Smooth");
                    let mut sum = 0.0;
                    for ny in -1..=1 {
                        for nx in -1..=1 {
                            sum += snap(v, tx + nx, ty + ny);
                        }
                    }
                    let avg = sum / 9.0;
                    current + (avg - current).clamp(-budget, budget)
                }
            }
            .clamp(0.0, HEIGHT_SCALE);
            if new_h == current {
                continue;
            }
            brush_hit.stroke_before.entry((tx, ty)).or_insert(current);
            heightmap.set_tile(tx, ty, new_h);
            affected.extend(chunks_touching_tile(tx, ty));
        }
    }

    // Only the mesh you can see moves per frame; the coarser LODs and the
    // collider wait for the stroke to end (`finalize_terrain_stroke`).
    for &chunk in &affected {
        terrain.rebuild_meshes(chunk, 0..1);
        terrain.mark_dirty(chunk);
        brush_hit.pending_full_lod.insert(chunk);
    }
    brush_hit.was_painting = true;
}

/// Stroke end (button released): regenerate the remaining LODs and the
/// physics heightfield of every chunk the stroke touched, and record the
/// stroke as one undo transaction.
pub fn finalize_terrain_stroke(
    mouse_input: Res<ButtonInput<MouseButton>>,
    brush: Res<BrushSettings>,
    mut brush_hit: ResMut<TerrainBrushHit>,
    mut terrain: TerrainChunks,
    mut history: ResMut<History>,
) {
    let painting = mouse_input.pressed(MouseButton::Left);

    if painting || !brush_hit.was_painting || brush_hit.pending_full_lod.is_empty() {
        if !painting {
            brush_hit.was_painting = false;
        }
        return;
    }
    brush_hit.was_painting = false;

    let pending: Vec<(i32, i32)> = brush_hit.pending_full_lod.drain().collect();
    for chunk in pending {
        terrain.rebuild_meshes(chunk, 1..LOD_LEVELS);
        terrain.rebuild_collider(chunk);
    }

    let tiles: Vec<(i32, i32, f32, f32)> = match terrain.heightmap.as_deref() {
        Some(heightmap) => brush_hit
            .stroke_before
            .drain()
            .map(|((tx, ty), before)| (tx, ty, before, heightmap.get_tile(tx, ty)))
            .collect(),
        None => Vec::new(),
    };
    history.push(
        format!("{} terrain", brush.mode.label()),
        vec![Change::Terrain { tiles }],
    );
}

/// Chunks whose meshes include tile `(tx, ty)`: its own, plus the neighbours
/// that share it when it sits on a chunk edge.
pub fn chunks_touching_tile(tx: i32, ty: i32) -> impl Iterator<Item = (i32, i32)> {
    let cs = (CHUNK_SIZE - 1) as i32;
    let (cx, cy) = (tx.div_euclid(cs), ty.div_euclid(cs));
    let on_x = tx.rem_euclid(cs) == 0;
    let on_y = ty.rem_euclid(cs) == 0;
    [
        Some((cx, cy)),
        on_x.then_some((cx - 1, cy)),
        on_y.then_some((cx, cy - 1)),
        (on_x && on_y).then_some((cx - 1, cy - 1)),
    ]
    .into_iter()
    .flatten()
}

/// The loaded terrain, as the brush and the undo stack mutate it: heights in,
/// meshes and colliders rebuilt from them.
#[derive(SystemParam)]
pub struct TerrainChunks<'w, 's> {
    pub heightmap: Option<ResMut<'w, HeightmapData>>,
    registry: Option<ResMut<'w, TerrainChunkRegistry>>,
    meshes: ResMut<'w, Assets<Mesh>>,
    chunks: Query<'w, 's, (&'static TerrainChunk, &'static mut Aabb)>,
    children: Query<'w, 's, &'static Children>,
    colliders: Query<'w, 's, (), With<Collider>>,
    commands: Commands<'w, 's>,
}

impl TerrainChunks<'_, '_> {
    pub fn is_loaded(&self, chunk: (i32, i32)) -> bool {
        self.registry
            .as_ref()
            .is_some_and(|r| r.loaded_chunks.contains_key(&chunk))
    }

    pub fn mark_dirty(&mut self, chunk: (i32, i32)) {
        if let Some(registry) = self.registry.as_deref_mut() {
            registry.dirty_chunks.insert(chunk);
        }
    }

    /// Write tiles straight into the heightmap (no brush shaping).
    pub fn set_tiles(&mut self, tiles: impl IntoIterator<Item = ((i32, i32), f32)>) {
        if let Some(heightmap) = self.heightmap.as_deref_mut() {
            for ((tx, ty), h) in tiles {
                heightmap.set_tile(tx, ty, h);
            }
        }
    }

    fn chunk_heights(&self, chunk: (i32, i32)) -> Option<Vec<f32>> {
        let heightmap = self.heightmap.as_deref()?;
        let cs = (CHUNK_SIZE - 1) as i32;
        let (start_x, start_y) = (chunk.0 * cs, chunk.1 * cs);
        let mut heights = Vec::with_capacity(CHUNK_SIZE * CHUNK_SIZE);
        for ly in 0..CHUNK_SIZE {
            let gy = start_y + ly as i32;
            for lx in 0..CHUNK_SIZE {
                heights.push(heightmap.get_tile(start_x + lx as i32, gy));
            }
        }
        Some(heights)
    }

    /// Regenerate `lods` of a loaded chunk from the heightmap. LOD 0 also
    /// refreshes the chunk's `Aabb`.
    pub fn rebuild_meshes(&mut self, chunk: (i32, i32), lods: std::ops::Range<usize>) {
        let Some(&entity) = self
            .registry
            .as_ref()
            .and_then(|r| r.loaded_chunks.get(&chunk))
        else {
            return;
        };
        let Some(heights) = self.chunk_heights(chunk) else {
            return;
        };
        let Ok((terrain_chunk, mut aabb)) = self.chunks.get_mut(entity) else {
            return;
        };
        let cs = (CHUNK_SIZE - 1) as i32;
        let (start_x, start_y) = (chunk.0 * cs, chunk.1 * cs);
        let height_at = |gx: i32, gy: i32| {
            let lx = (gx - start_x) as usize;
            let ly = (gy - start_y) as usize;
            heights[ly * CHUNK_SIZE + lx]
        };
        for lod in lods.clone().take(LOD_LEVELS) {
            let (new_mesh, new_aabb) = generate_lod_mesh(
                height_at, start_x, start_y, CHUNK_SIZE, CHUNK_SIZE, TILE_SIZE, lod,
            );
            if let Some(mut m) = self.meshes.get_mut(&terrain_chunk.lod_meshes[lod]) {
                *m = new_mesh;
            }
            if lod == 0 {
                *aabb = new_aabb;
            }
        }
    }

    /// Replace the chunk's physics heightfield with one built from the
    /// current heights. Without this the character keeps walking on the
    /// pre-sculpt ground until the chunk is reloaded.
    pub fn rebuild_collider(&mut self, chunk: (i32, i32)) {
        let Some(&entity) = self
            .registry
            .as_ref()
            .and_then(|r| r.loaded_chunks.get(&chunk))
        else {
            return;
        };
        let Some(heights) = self.chunk_heights(chunk) else {
            return;
        };
        let Ok(children) = self.children.get(entity) else {
            return;
        };
        let Some(collider_entity) = children.iter().find(|c| self.colliders.contains(*c)) else {
            return;
        };
        let (collider, _) = heightfield_collider(|lx, ly| heights[ly * CHUNK_SIZE + lx], TILE_SIZE);
        self.commands.entity(collider_entity).insert(collider);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edge_tiles_belong_to_their_neighbours_too() {
        let cs = (CHUNK_SIZE - 1) as i32;
        let interior: Vec<_> = chunks_touching_tile(5, 5).collect();
        assert_eq!(interior, vec![(0, 0)]);
        let mut corner: Vec<_> = chunks_touching_tile(cs, cs).collect();
        corner.sort();
        assert_eq!(corner, vec![(0, 0), (0, 1), (1, 0), (1, 1)]);
        let mut edge: Vec<_> = chunks_touching_tile(cs, 3).collect();
        edge.sort();
        assert_eq!(edge, vec![(0, 0), (1, 0)]);
    }
}
