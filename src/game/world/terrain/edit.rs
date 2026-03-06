use std::collections::HashSet;

use bevy::{camera::primitives::Aabb, picking::mesh_picking::ray_cast::MeshRayCast, prelude::*};

use crate::game::{gfx::camera::PlayerCamera, world::terrain::{
    CHUNK_SIZE, HEIGHT_SCALE, HeightmapData, TILE_SIZE, TerrainChunk, mesh::{LOD_LEVELS, generate_lod_mesh}, storage::TerrainChunkRegistry
}};
use crate::settings::UserSettings;

/// Height change per second at falloff=1, in pre-scaled world units.
const BRUSH_STRENGTH: f32 = 50.0;

/// Stores the world-space terrain hit point found this frame by the raycast system.
/// `None` means no terrain was hit (or terrain edit mode is inactive).
#[derive(Resource, Default)]
pub struct TerrainBrushHit(pub Option<Vec3>);

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
        brush_hit.0 = None;
        return;
    }

    let Ok(camera_transform) = camera_query.single() else {
        brush_hit.0 = None;
        return;
    };

    let ray = Ray3d::new(
        camera_transform.translation,
        Dir3::new(camera_transform.forward().as_vec3()).unwrap(),
    );

    let hits = raycast.cast_ray(ray, &Default::default());
    brush_hit.0 = hits.iter().find_map(|(entity, hit_data)| {
        if terrain_chunk_filter.contains(*entity) {
            Some(hit_data.point)
        } else {
            None
        }
    });
}

/// Modifies heightmap heights and regenerates chunk meshes based on the hit
/// point written by `raycast_terrain_hit`. Must run after that system.
pub fn apply_terrain_brush(
    mouse_input: Res<ButtonInput<MouseButton>>,
    settings: Res<UserSettings>,
    brush_hit: Res<TerrainBrushHit>,
    mut heightmap: Option<ResMut<HeightmapData>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut chunk_query: Query<(&TerrainChunk, &mut Aabb)>,
    mut registry: Option<ResMut<TerrainChunkRegistry>>,
    time: Res<Time>,
) {
    let raise = mouse_input.pressed(MouseButton::Left);
    let lower = mouse_input.pressed(MouseButton::Right);
    let Some(hit_point) = brush_hit.0 else { return };
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

    let grid_w = heightmap.grid_w as i32;
    let grid_h = heightmap.grid_h as i32;

    let mut affected: HashSet<(usize, usize)> = HashSet::new();

    for dx in -extent..=extent {
        for dy in -extent..=extent {
            let tx = center_tx + dx;
            let ty = center_ty + dy;
            if tx < 0 || ty < 0 || tx >= grid_w || ty >= grid_h {
                continue;
            }
            let world_x = tx as f32 * TILE_SIZE.0;
            let world_z = ty as f32 * TILE_SIZE.1;
            let dist = Vec2::new(world_x - hit_point.x, world_z - hit_point.z).length();
            if dist > radius {
                continue;
            }

            let falloff = 1.0 - dist / radius;
            let delta = BRUSH_STRENGTH * falloff * dt * sign;
            let current = heightmap.get_tile(tx as usize, ty as usize).unwrap_or(0.0);
            let new_h = (current + delta).clamp(0.0, HEIGHT_SCALE);
            heightmap.set_tile(tx as usize, ty as usize, new_h);

            // A tile on a chunk boundary also belongs to the adjacent chunk — regenerate both.
            let cs = CHUNK_SIZE - 1;
            let cx = tx as usize / cs;
            let cy = ty as usize / cs;
            affected.insert((cx, cy));
            if tx as usize > 0 && tx as usize % cs == 0 {
                affected.insert((cx - 1, cy));
            }
            if ty as usize > 0 && ty as usize % cs == 0 {
                affected.insert((cx, cy - 1));
            }
            if tx as usize > 0 && tx as usize % cs == 0 && ty as usize > 0 && ty as usize % cs == 0
            {
                affected.insert((cx - 1, cy - 1));
            }
        }
    }

    // Borrow heights immutably for mesh regeneration; all set_tile calls are done.
    let heights: &[f32] = &heightmap.heights;
    let grid_w = heightmap.grid_w;
    let grid_h = heightmap.grid_h;

    for (cx, cy) in &affected {
        let (cx, cy) = (*cx, *cy);
        let start_x = cx * (CHUNK_SIZE - 1);
        let start_y = cy * (CHUNK_SIZE - 1);
        let end_x = (start_x + CHUNK_SIZE).min(grid_w);
        let end_y = (start_y + CHUNK_SIZE).min(grid_h);
        let chunk_w = end_x - start_x;
        let chunk_h = end_y - start_y;

        for (chunk, mut aabb) in chunk_query.iter_mut() {
            if chunk.chunk_x != cx || chunk.chunk_y != cy {
                continue;
            }

            let mut new_aabb = None;
            for lod in 0..LOD_LEVELS {
                let (new_mesh, lod_aabb) = generate_lod_mesh(
                    heights, grid_w, grid_h, start_x, start_y, chunk_w, chunk_h, TILE_SIZE, 1.0,
                    lod,
                );
                if lod == 0 {
                    new_aabb = Some(lod_aabb);
                }
                if let Some(m) = meshes.get_mut(&chunk.lod_meshes[lod]) {
                    *m = new_mesh;
                }
            }
            if let Some(a) = new_aabb {
                *aabb = a;
            }
            break;
        }

        if let Some(ref mut reg) = registry {
            reg.dirty_chunks.insert((cx as u32, cy as u32));
        }
    }
}
