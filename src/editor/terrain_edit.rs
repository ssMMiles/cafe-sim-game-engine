//! Terrain tool: sculpt the heightmap under the cursor.
//!
//! LMB raises, Ctrl+LMB lowers (RMB is the editor's look-drag). The brush
//! follows [`EditorRay`], so it lands wherever the cursor is on the viewport.

use std::collections::HashSet;

use bevy::{
    camera::primitives::Aabb,
    light::NotShadowCaster,
    picking::mesh_picking::ray_cast::{MeshRayCast, MeshRayCastSettings},
    prelude::*,
};

use crate::editor::picking::{EditorRay, ViewportInput};
use crate::game::world::terrain::{
    CHUNK_SIZE, HEIGHT_SCALE, HeightmapData, TILE_SIZE, TerrainChunk,
    mesh::{LOD_LEVELS, generate_lod_mesh},
    storage::TerrainChunkRegistry,
};
use crate::settings::UserSettings;

/// Brush parameters that don't live on `UserSettings`.
#[derive(Resource, Debug)]
pub struct BrushSettings {
    /// Height change per second at falloff=1, in pre-scaled world units.
    pub strength: f32,
}

impl Default for BrushSettings {
    fn default() -> Self {
        Self { strength: 50.0 }
    }
}

#[derive(Resource, Default)]
pub struct TerrainBrushHit {
    pub hit: Option<Vec3>,
    pub pending_full_lod: HashSet<(i32, i32)>,
    pub was_painting: bool,
}

pub fn adjust_brush_size(
    mut settings: ResMut<UserSettings>,
    input: Res<ButtonInput<KeyCode>>,
    time: Res<Time>,
) {
    let dt = time.delta_secs();
    if input.pressed(KeyCode::BracketLeft) {
        settings.brush_radius = (settings.brush_radius - 10.0 * dt).max(1.0);
    }
    if input.pressed(KeyCode::BracketRight) {
        settings.brush_radius = (settings.brush_radius + 10.0 * dt).min(50.0);
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
    settings: Res<UserSettings>,
    brush_hit: Res<TerrainBrushHit>,
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut preview_query: Query<(&mut Transform, &mut Visibility), With<BrushPreviewMarker>>,
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
            transform.scale = Vec3::splat(settings.brush_radius);
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

/// Which way the brush is pushing this frame, if the button is down over the
/// viewport. Ctrl lowers; plain LMB raises.
fn brush_sign(mouse: &ButtonInput<MouseButton>, keys: &ButtonInput<KeyCode>) -> Option<f32> {
    if !mouse.pressed(MouseButton::Left) {
        return None;
    }
    let ctrl = keys.pressed(KeyCode::ControlLeft) || keys.pressed(KeyCode::ControlRight);
    Some(if ctrl { -1.0 } else { 1.0 })
}

#[allow(clippy::too_many_arguments)]
pub fn apply_terrain_brush(
    mouse_input: Res<ButtonInput<MouseButton>>,
    keys: Res<ButtonInput<KeyCode>>,
    viewport: Res<ViewportInput>,
    settings: Res<UserSettings>,
    brush: Res<BrushSettings>,
    mut brush_hit: ResMut<TerrainBrushHit>,
    heightmap: Option<ResMut<HeightmapData>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut chunk_query: Query<(&TerrainChunk, &mut Aabb)>,
    mut registry: Option<ResMut<TerrainChunkRegistry>>,
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

    let Some(mut heightmap) = heightmap else {
        return;
    };

    let radius = settings.brush_radius;
    let dt = time.delta_secs();

    let center_tx = (hit_point.x / TILE_SIZE.0) as i32;
    let center_ty = (hit_point.z / TILE_SIZE.1) as i32;
    let extent = (radius / TILE_SIZE.0.min(TILE_SIZE.1)) as i32 + 1;

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

            let falloff = 1.0 - dist / radius;
            let delta = brush.strength * falloff * dt * sign;
            let current = heightmap.get_tile(tx, ty);
            let new_h = (current + delta).clamp(0.0, HEIGHT_SCALE);
            heightmap.set_tile(tx, ty, new_h);

            let cs = (CHUNK_SIZE - 1) as i32;
            let cx = tx.div_euclid(cs);
            let cy = ty.div_euclid(cs);
            affected.insert((cx, cy));

            if tx.rem_euclid(cs) == 0 {
                affected.insert((cx - 1, cy));
            }
            if ty.rem_euclid(cs) == 0 {
                affected.insert((cx, cy - 1));
            }
            if tx.rem_euclid(cs) == 0 && ty.rem_euclid(cs) == 0 {
                affected.insert((cx - 1, cy - 1));
            }
        }
    }

    let heightmap = heightmap.into_inner();
    let mut registry = registry.as_deref_mut();

    let cs = (CHUNK_SIZE - 1) as i32;
    for &(cx, cy) in &affected {
        let start_x = cx * cs;
        let start_y = cy * cs;

        let mut heights = Vec::with_capacity(CHUNK_SIZE * CHUNK_SIZE);
        for ly in 0..CHUNK_SIZE {
            let gy = start_y + ly as i32;
            for lx in 0..CHUNK_SIZE {
                heights.push(heightmap.get_tile(start_x + lx as i32, gy));
            }
        }

        if let Some(ref mut reg) = registry {
            if let Some(&entity) = reg.loaded_chunks.get(&(cx, cy))
                && let Ok((chunk, mut aabb)) = chunk_query.get_mut(entity)
            {
                let (new_mesh, new_aabb) = generate_lod_mesh(
                    |gx, gy| {
                        let lx = (gx - start_x) as usize;
                        let ly = (gy - start_y) as usize;
                        heights[ly * CHUNK_SIZE + lx]
                    },
                    start_x,
                    start_y,
                    CHUNK_SIZE,
                    CHUNK_SIZE,
                    TILE_SIZE,
                    0,
                );
                if let Some(m) = meshes.get_mut(&chunk.lod_meshes[0]) {
                    *m = new_mesh;
                }
                *aabb = new_aabb;
            }
            reg.dirty_chunks.insert((cx, cy));
        }

        brush_hit.pending_full_lod.insert((cx, cy));
    }
    brush_hit.was_painting = true;
}

pub fn finalize_terrain_lod(
    mouse_input: Res<ButtonInput<MouseButton>>,
    mut brush_hit: ResMut<TerrainBrushHit>,
    heightmap: Option<Res<HeightmapData>>,
    mut meshes: ResMut<Assets<Mesh>>,
    chunk_query: Query<&TerrainChunk>,
    registry: Option<Res<TerrainChunkRegistry>>,
) {
    let painting = mouse_input.pressed(MouseButton::Left);

    if painting || !brush_hit.was_painting || brush_hit.pending_full_lod.is_empty() {
        if !painting {
            brush_hit.was_painting = false;
        }
        return;
    }
    brush_hit.was_painting = false;

    let Some(heightmap) = heightmap else { return };
    let Some(registry) = registry else { return };

    let cs = (CHUNK_SIZE - 1) as i32;
    let pending: Vec<(i32, i32)> = brush_hit.pending_full_lod.drain().collect();

    for (cx, cy) in pending {
        let start_x = cx * cs;
        let start_y = cy * cs;

        let Some(&entity) = registry.loaded_chunks.get(&(cx, cy)) else {
            continue;
        };
        let Ok(chunk) = chunk_query.get(entity) else {
            continue;
        };

        let mut heights = Vec::with_capacity(CHUNK_SIZE * CHUNK_SIZE);
        for ly in 0..CHUNK_SIZE {
            let gy = start_y + ly as i32;
            for lx in 0..CHUNK_SIZE {
                heights.push(heightmap.get_tile(start_x + lx as i32, gy));
            }
        }

        for lod in 1..LOD_LEVELS {
            let (new_mesh, _) = generate_lod_mesh(
                |gx, gy| {
                    let lx = (gx - start_x) as usize;
                    let ly = (gy - start_y) as usize;
                    heights[ly * CHUNK_SIZE + lx]
                },
                start_x,
                start_y,
                CHUNK_SIZE,
                CHUNK_SIZE,
                TILE_SIZE,
                lod,
            );
            if let Some(m) = meshes.get_mut(&chunk.lod_meshes[lod]) {
                *m = new_mesh;
            }
        }
    }
}
