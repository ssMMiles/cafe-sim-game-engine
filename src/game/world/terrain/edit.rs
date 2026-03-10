use std::collections::HashSet;

use bevy::{camera::primitives::Aabb, picking::mesh_picking::ray_cast::MeshRayCast, prelude::*};

use crate::game::{
    gfx::camera::PlayerCamera,
    world::terrain::{
        CHUNK_SIZE, HEIGHT_SCALE, HeightmapData, TILE_SIZE, TerrainChunk,
        mesh::{LOD_LEVELS, generate_lod_mesh},
        storage::TerrainChunkRegistry,
    },
};
use crate::settings::UserSettings;

/// Height change per second at falloff=1, in pre-scaled world units.
const BRUSH_STRENGTH: f32 = 50.0;

/// Stores the world-space terrain hit point found this frame by the raycast system.
/// `None` means no terrain was hit (or terrain edit mode is inactive).
#[derive(Resource, Default)]
pub struct TerrainBrushHit {
    pub hit: Option<Vec3>,
    /// Chunks that need full LOD regen (LOD1–3) after the brush is released.
    /// LOD1–3 are only visible at 500m–4km distance, so deferring them until
    /// painting stops keeps frame time low while giving immediate LOD0 feedback.
    pub pending_full_lod: HashSet<(i32, i32)>,
    pub was_painting: bool,
}

pub fn toggle_terrain_edit_mode(
    mut settings: ResMut<UserSettings>,
    input: Res<ButtonInput<KeyCode>>,
) {
    if input.just_pressed(KeyCode::KeyG)
        && (input.pressed(KeyCode::ShiftLeft) || input.pressed(KeyCode::ShiftRight))
    {
        settings.toggle_terrain_edit_mode();
    }
}

pub fn adjust_brush_size(
    mut settings: ResMut<UserSettings>,
    input: Res<ButtonInput<KeyCode>>,
    time: Res<Time>,
) {
    if !settings.terrain_edit_mode {
        return;
    }
    let dt = time.delta_secs();
    if input.pressed(KeyCode::BracketLeft) {
        settings.brush_radius = (settings.brush_radius - 10.0 * dt).max(1.0);
    }
    if input.pressed(KeyCode::BracketRight) {
        settings.brush_radius = (settings.brush_radius + 10.0 * dt).min(50.0);
    }
}

/// Casts a ray and writes the first terrain hit into `TerrainBrushHit`.
/// Must run before `apply_terrain_brush` each frame.
pub fn raycast_terrain_hit(
    mouse_input: Res<ButtonInput<MouseButton>>,
    settings: Res<UserSettings>,
    camera_query: Query<&Transform, With<PlayerCamera>>,
    terrain_chunk_filter: Query<(), With<TerrainChunk>>,
    mut raycast: MeshRayCast,
    mut brush_hit: ResMut<TerrainBrushHit>,
) {
    let raise = mouse_input.pressed(MouseButton::Left);
    let lower = mouse_input.pressed(MouseButton::Right);

    if !settings.terrain_edit_mode || (!raise && !lower) {
        brush_hit.hit = None;
        return;
    }

    let Ok(camera_transform) = camera_query.single() else {
        brush_hit.hit = None;
        return;
    };

    let ray = Ray3d::new(
        camera_transform.translation,
        Dir3::new(camera_transform.forward().as_vec3()).unwrap(),
    );

    let hits = raycast.cast_ray(ray, &Default::default());
    brush_hit.hit = hits.iter().find_map(|(entity, hit_data)| {
        if terrain_chunk_filter.contains(*entity) {
            Some(hit_data.point)
        } else {
            None
        }
    });
}

/// Modifies heightmap heights and regenerates LOD0 chunk meshes based on the hit
/// point written by `raycast_terrain_hit`. LOD1–3 are deferred to `finalize_terrain_lod`
/// which runs them in one batch when the brush is released.
pub fn apply_terrain_brush(
    mouse_input: Res<ButtonInput<MouseButton>>,
    settings: Res<UserSettings>,
    mut brush_hit: ResMut<TerrainBrushHit>,
    mut heightmap: Option<ResMut<HeightmapData>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut chunk_query: Query<(&TerrainChunk, &mut Aabb)>,
    mut registry: Option<ResMut<TerrainChunkRegistry>>,
    time: Res<Time>,
) {
    let raise = mouse_input.pressed(MouseButton::Left);
    let lower = mouse_input.pressed(MouseButton::Right);
    let Some(hit_point) = brush_hit.hit else { return };
    if !settings.terrain_edit_mode || (!raise && !lower) {
        return;
    }

    let Some(mut heightmap) = heightmap else {
        return;
    };

    let radius = settings.brush_radius;
    let dt = time.delta_secs();
    let sign = if raise { 1.0_f32 } else { -1.0 };

    let center_tx = (hit_point.x / TILE_SIZE.0) as i32;
    let center_ty = (hit_point.z / TILE_SIZE.1) as i32;
    let extent = (radius / TILE_SIZE.0.min(TILE_SIZE.1)) as i32 + 1;

    // Collect which chunks need mesh regeneration (use signed i32 throughout).
    let mut affected: HashSet<(i32, i32)> = HashSet::new();

    // Phase 1: apply height deltas.
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
            let delta = BRUSH_STRENGTH * falloff * dt * sign;
            let current = heightmap.get_tile(tx, ty);
            let new_h = (current + delta).clamp(0.0, HEIGHT_SCALE);
            heightmap.set_tile(tx, ty, new_h);

            // A tile on a chunk boundary also belongs to the adjacent chunk.
            let cs = (CHUNK_SIZE - 1) as i32;
            let cx = tx.div_euclid(cs);
            let cy = ty.div_euclid(cs);
            affected.insert((cx, cy));

            // Shared left boundary
            if tx.rem_euclid(cs) == 0 {
                affected.insert((cx - 1, cy));
            }
            // Shared top boundary
            if ty.rem_euclid(cs) == 0 {
                affected.insert((cx, cy - 1));
            }
            // Shared corner (both boundaries)
            if tx.rem_euclid(cs) == 0 && ty.rem_euclid(cs) == 0 {
                affected.insert((cx - 1, cy - 1));
            }
        }
    }

    // Phase 2: regenerate LOD0 only for immediate visual feedback.
    // LOD1–3 are deferred to `finalize_terrain_lod` (runs on brush release) since
    // those levels are only visible at 500m–4km and not worth the per-frame cost.
    let heightmap = heightmap.into_inner();

    let cs = (CHUNK_SIZE - 1) as i32;
    for &(cx, cy) in &affected {
        let start_x = cx * cs;
        let start_y = cy * cs;

        for (chunk, mut aabb) in chunk_query.iter_mut() {
            if chunk.chunk_x != cx || chunk.chunk_y != cy {
                continue;
            }

            let (new_mesh, new_aabb) = generate_lod_mesh(
                |tx, ty| heightmap.get_tile(tx, ty),
                start_x,
                start_y,
                CHUNK_SIZE,
                CHUNK_SIZE,
                TILE_SIZE,
                0, // LOD0 only
            );
            if let Some(m) = meshes.get_mut(&chunk.lod_meshes[0]) {
                *m = new_mesh;
            }
            *aabb = new_aabb;
            break;
        }

        brush_hit.pending_full_lod.insert((cx, cy));
        if let Some(ref mut reg) = registry {
            reg.dirty_chunks.insert((cx, cy));
        }
    }
    brush_hit.was_painting = true;
}

/// Regenerates LOD1–3 meshes for all chunks touched since the last brush stroke,
/// once the mouse buttons are released. Runs after `apply_terrain_brush`.
pub fn finalize_terrain_lod(
    mouse_input: Res<ButtonInput<MouseButton>>,
    mut brush_hit: ResMut<TerrainBrushHit>,
    heightmap: Option<Res<HeightmapData>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut chunk_query: Query<&TerrainChunk>,
) {
    let painting = mouse_input.pressed(MouseButton::Left) || mouse_input.pressed(MouseButton::Right);

    // Only run the first frame after brush release.
    if painting || !brush_hit.was_painting || brush_hit.pending_full_lod.is_empty() {
        if !painting {
            brush_hit.was_painting = false;
        }
        return;
    }
    brush_hit.was_painting = false;

    let Some(heightmap) = heightmap else { return };

    let cs = (CHUNK_SIZE - 1) as i32;
    let pending: Vec<(i32, i32)> = brush_hit.pending_full_lod.drain().collect();

    for (cx, cy) in pending {
        let start_x = cx * cs;
        let start_y = cy * cs;

        for chunk in chunk_query.iter_mut() {
            if chunk.chunk_x != cx || chunk.chunk_y != cy {
                continue;
            }

            for lod in 1..LOD_LEVELS {
                let (new_mesh, _) = generate_lod_mesh(
                    |tx, ty| heightmap.get_tile(tx, ty),
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
            break;
        }
    }
}
