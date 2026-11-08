use bevy::prelude::*;
use serde::{Deserialize, Serialize};

use crate::game::gfx::camera::PlayerCamera;

use super::{TerrainChunk, mesh::LOD_LEVELS};

/// A chunk steps back to the finer mesh at 90% of the distance it coarsened
/// at (compared squared: 0.9² = 0.81), so a camera sitting on a boundary
/// doesn't flip meshes every update.
const HYSTERESIS_SQ: f32 = 0.81;

/// Where each coarser terrain mesh takes over, metres from the camera:
/// `distances[k]` is where LOD `k+1` (a `2^(k+1)`-stride mesh) replaces LOD
/// `k`. A user display setting (persisted in `editor_settings.json`), not
/// world data — it trades draw cost against pop-in and touches nothing baked.
#[derive(Resource, Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TerrainLodSettings {
    pub distances: [f32; LOD_LEVELS - 1],
}

impl Default for TerrainLodSettings {
    /// The former `LOD_DISTANCES_SQ` constants, so nothing changes until the
    /// user tunes them. Note the streamed ring only reaches ~290 m along an
    /// axis (`ChunkStreamingConfig::load_radius`), so at these defaults only
    /// the corner chunks ever leave LOD 0.
    fn default() -> Self {
        Self {
            distances: [500.0, 1000.0, 2500.0],
        }
    }
}

impl TerrainLodSettings {
    /// Non-decreasing and positive, whatever a hand-edited settings file
    /// holds; the UI and the system both go through this.
    pub fn sanitized(mut self) -> Self {
        let mut floor = 1.0_f32;
        for d in &mut self.distances {
            *d = d.max(floor);
            floor = *d;
        }
        self
    }
}

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
    settings: Res<TerrainLodSettings>,
) {
    let Ok(camera_transform) = camera_query.single() else {
        return;
    };
    let camera_pos = camera_transform.translation();

    // A settings edit re-evaluates every chunk in place; otherwise wait for
    // the camera to move far enough to matter.
    let reseat = settings.is_changed();
    let moved_sq = camera_pos.distance_squared(lod_state.last_camera_pos);
    if !reseat && moved_sq < lod_state.update_threshold_sq {
        return;
    }
    lod_state.last_camera_pos = camera_pos;

    let distances_sq = settings.sanitized().distances.map(|d| d * d);

    for (mut chunk, mut mesh3d, aabb, global_transform) in chunk_query.iter_mut() {
        // AABB center is in local space; transform to world space
        let local_center: Vec3 = aabb.center.into();
        let chunk_center = global_transform.transform_point(local_center);
        let distance_sq = camera_pos.distance_squared(chunk_center);

        let current = chunk.current_lod;
        let new_lod = if reseat {
            // New boundaries: map straight to the target level — stepping
            // ±1 would leave a chunk mid-way after a single edit.
            distances_sq
                .iter()
                .position(|&d| distance_sq < d)
                .unwrap_or(LOD_LEVELS - 1)
        } else if current > 0 {
            if distance_sq < distances_sq[current - 1] * HYSTERESIS_SQ {
                current - 1
            } else if current < LOD_LEVELS - 1 && distance_sq >= distances_sq[current] {
                current + 1
            } else {
                current
            }
        } else if distance_sq >= distances_sq[0] {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_makes_distances_non_decreasing() {
        let s = TerrainLodSettings {
            distances: [800.0, 300.0, -5.0],
        }
        .sanitized();
        assert_eq!(s.distances, [800.0, 800.0, 800.0]);
        let ok = TerrainLodSettings::default();
        assert_eq!(ok.sanitized(), ok);
    }
}
