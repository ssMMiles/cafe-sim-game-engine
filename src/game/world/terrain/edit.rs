use std::collections::HashSet;

use bevy::{
    camera::primitives::Aabb,
    light::NotShadowCaster,
    picking::mesh_picking::ray_cast::{MeshRayCast, MeshRayCastSettings},
    prelude::*,
};

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
/// Always runs when terrain edit mode is active so the preview circle is visible
/// even before a mouse button is pressed. Must run before `apply_terrain_brush`.
pub fn raycast_terrain_hit(
    settings: Res<UserSettings>,
    camera_query: Query<&Transform, With<PlayerCamera>>,
    terrain_chunk_filter: Query<(), With<TerrainChunk>>,
    mut raycast: MeshRayCast,
    mut brush_hit: ResMut<TerrainBrushHit>,
) {
    if !settings.terrain_edit_mode {
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

    let filter = |entity: Entity| terrain_chunk_filter.contains(entity);
    let hits = raycast.cast_ray(ray, &MeshRayCastSettings::default().with_filter(&filter));
    brush_hit.hit = hits.first().map(|(_, hit_data)| hit_data.point);
}

/// Marker for the brush preview disc entity.
#[derive(Component)]
pub struct BrushPreviewMarker;

/// Lazily spawns a semi-transparent disc entity and keeps it positioned at the
/// terrain hit point each frame. Hides it when terrain edit mode is off.
pub fn update_brush_preview(
    settings: Res<UserSettings>,
    brush_hit: Res<TerrainBrushHit>,
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut preview_query: Query<(&mut Transform, &mut Visibility), With<BrushPreviewMarker>>,
) {
    // Lazy spawn on first call.
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
            // Circle lies in XY; rotate to lie flat on XZ (horizontal).
            Transform::from_rotation(Quat::from_rotation_x(-std::f32::consts::FRAC_PI_2)),
            Visibility::Hidden,
            NotShadowCaster,
            BrushPreviewMarker,
        ));
        return;
    }

    let Ok((mut transform, mut visibility)) = preview_query.single_mut() else { return };

    match brush_hit.hit.filter(|_| settings.terrain_edit_mode) {
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

/// Modifies heightmap heights and regenerates LOD0 chunk meshes based on the hit
/// point written by `raycast_terrain_hit`. LOD1–3 are deferred to `finalize_terrain_lod`
/// which runs them in one batch when the brush is released.
pub fn apply_terrain_brush(
    mouse_input: Res<ButtonInput<MouseButton>>,
    settings: Res<UserSettings>,
    mut brush_hit: ResMut<TerrainBrushHit>,
    heightmap: Option<ResMut<HeightmapData>>,
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
    let mut registry = registry.as_deref_mut();

    let cs = (CHUNK_SIZE - 1) as i32;
    for &(cx, cy) in &affected {
        let start_x = cx * cs;
        let start_y = cy * cs;

        // Pre-extract this chunk's heights into a flat array for cache-friendly access.
        // generate_lod_mesh's normal sampling always clamps to [0, CHUNK_SIZE-1] local
        // coords (via saturating_sub / .min(chunk_width-1)), so no out-of-bounds reads.
        let mut heights = Vec::with_capacity(CHUNK_SIZE * CHUNK_SIZE);
        for ly in 0..CHUNK_SIZE {
            let gy = start_y + ly as i32;
            for lx in 0..CHUNK_SIZE {
                heights.push(heightmap.get_tile(start_x + lx as i32, gy));
            }
        }

        // O(1) registry lookup instead of O(n) linear chunk scan.
        if let Some(ref mut reg) = registry {
            if let Some(&entity) = reg.loaded_chunks.get(&(cx, cy)) {
                if let Ok((chunk, mut aabb)) = chunk_query.get_mut(entity) {
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
                        0, // LOD0 only
                    );
                    if let Some(m) = meshes.get_mut(&chunk.lod_meshes[0]) {
                        *m = new_mesh;
                    }
                    *aabb = new_aabb;
                }
            }
            reg.dirty_chunks.insert((cx, cy));
        }

        brush_hit.pending_full_lod.insert((cx, cy));
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
    chunk_query: Query<&TerrainChunk>,
    registry: Option<Res<TerrainChunkRegistry>>,
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
    let Some(registry) = registry else { return };

    let cs = (CHUNK_SIZE - 1) as i32;
    let pending: Vec<(i32, i32)> = brush_hit.pending_full_lod.drain().collect();

    for (cx, cy) in pending {
        let start_x = cx * cs;
        let start_y = cy * cs;

        let Some(&entity) = registry.loaded_chunks.get(&(cx, cy)) else { continue };
        let Ok(chunk) = chunk_query.get(entity) else { continue };

        // Pre-extract heights to flat array for cache-friendly sequential access.
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
