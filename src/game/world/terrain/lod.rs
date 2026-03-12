use bevy::prelude::*;

use crate::game::{gfx::camera::PlayerCamera};

use super::{TerrainChunk, constants::LOD_DISTANCES_SQ, mesh::LOD_LEVELS};

/// Tracks camera position for LOD update throttling
#[derive(Resource)]
pub struct TerrainLodState {
    last_camera_pos: Vec3,
    update_threshold_sq: f32,
}

impl Default for TerrainLodState {
    fn default() -> Self {
        Self {
            last_camera_pos: Vec3::ZERO,
            update_threshold_sq: 15.0 * 15.0,
        }
    }
}

/// System to update terrain chunk LOD based on camera distance.
pub fn update_terrain_lod(
    camera_query: Query<&GlobalTransform, With<PlayerCamera>>,
    mut chunk_query: Query<(
        &mut TerrainChunk,
        &mut Mesh3d,
        &bevy::camera::primitives::Aabb,
        &GlobalTransform,
    )>,
    mut lod_state: ResMut<TerrainLodState>,
) {
    let Ok(camera_transform) = camera_query.single() else {
        return;
    };
    let camera_pos = camera_transform.translation();

    let moved_sq = camera_pos.distance_squared(lod_state.last_camera_pos);
    if moved_sq < lod_state.update_threshold_sq {
        return;
    }
    lod_state.last_camera_pos = camera_pos;


    for (mut chunk, mut mesh3d, aabb, global_transform) in chunk_query.iter_mut() {
        // AABB center is in local space; transform to world space
        let local_center: Vec3 = aabb.center.into();
        let chunk_center = global_transform.transform_point(local_center);
        let distance_sq = camera_pos.distance_squared(chunk_center);

        let current = chunk.current_lod;
        let new_lod = if current > 0 {
            let threshold_sq = LOD_DISTANCES_SQ[current - 1] * 0.81;
            if distance_sq < threshold_sq {
                current - 1
            } else if current < LOD_LEVELS - 1 && distance_sq >= LOD_DISTANCES_SQ[current] {
                current + 1
            } else {
                current
            }
        } else if distance_sq >= LOD_DISTANCES_SQ[0] {
            1
        } else {
            0
        };

        if new_lod != chunk.current_lod {
            mesh3d.0 = chunk.lod_meshes[new_lod].clone();
            chunk.current_lod = new_lod;
        }
    }
}
