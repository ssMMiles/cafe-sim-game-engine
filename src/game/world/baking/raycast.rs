//! CPU-side ray marching against the heightmap for shadow and AO computation.

use bevy::math::Vec3;

use crate::game::world::terrain::HeightmapData;

// ============================================================================
// Shadow Ray
// ============================================================================

/// Cast a shadow ray from `origin` toward `sun_dir` using DDA-style steps.
///
/// Returns `0.0` if the ray is occluded (in shadow) or `1.0` if it reaches
/// `max_dist` without hitting terrain. Uses `steps` evenly-spaced samples.
pub fn cast_shadow_ray(
    origin: Vec3,
    sun_dir: Vec3,
    heightmap: &HeightmapData,
    max_dist: f32,
    steps: u32,
) -> f32 {
    let step_size = max_dist / steps as f32;

    for i in 1..=steps {
        let t = i as f32 * step_size;
        let sample_pos = origin + sun_dir * t;

        // Query height at this (x, z) position.
        let terrain_h = heightmap
            .sample_height(sample_pos.x, sample_pos.z)
            .unwrap_or(0.0);

        // If the terrain is above the ray at this point, the origin is in shadow.
        if terrain_h > sample_pos.y {
            return 0.0;
        }
    }

    1.0
}

// ============================================================================
// Ambient Occlusion
// ============================================================================

/// Compute ambient occlusion at `origin` by casting `n_rays` hemisphere rays
/// in the half-space defined by `normal`.
///
/// Returns the fraction of unoccluded rays ∈ [0, 1].  Uses a simple
/// deterministic cosine-weighted hemisphere distribution built from the
/// incoming normal so the result is stable across bakes.
pub fn compute_ao(
    origin: Vec3,
    normal: Vec3,
    heightmap: &HeightmapData,
    radius: f32,
    n_rays: u32,
    steps: u32,
) -> f32 {
    if n_rays == 0 {
        return 1.0;
    }

    // Build a tangent frame for the hemisphere sampling.
    let up = if normal.y.abs() < 0.99 {
        Vec3::Y
    } else {
        Vec3::X
    };
    let tangent = up.cross(normal).normalize();
    let bitangent = normal.cross(tangent).normalize();

    let mut unoccluded: u32 = 0;

    for i in 0..n_rays {
        // Stratified cosine-weighted hemisphere distribution using the
        // Hammersley-like sequence to avoid clustering artefacts.
        let u = (i as f32 + 0.5) / n_rays as f32;
        let angle = std::f32::consts::TAU * golden_ratio_seq(i);

        // Cosine-weighted sample in the hemisphere.
        let r = u.sqrt();
        let cos_theta = (1.0 - u).sqrt();
        let sin_theta = r;
        let local_dir = Vec3::new(sin_theta * angle.cos(), cos_theta, sin_theta * angle.sin());

        // Transform to world space using the tangent frame.
        let world_dir =
            (tangent * local_dir.x + normal * local_dir.y + bitangent * local_dir.z).normalize();

        // Only consider rays that face the correct hemisphere.
        if world_dir.dot(normal) <= 0.0 {
            continue;
        }

        let step_size = radius / steps as f32;
        let mut occluded = false;

        for s in 1..=steps {
            let t = s as f32 * step_size;
            let sample_pos = origin + world_dir * t;
            let terrain_h = heightmap
                .sample_height(sample_pos.x, sample_pos.z)
                .unwrap_or(0.0);

            if terrain_h > sample_pos.y {
                occluded = true;
                break;
            }
        }

        if !occluded {
            unoccluded += 1;
        }
    }

    unoccluded as f32 / n_rays as f32
}

// ============================================================================
// Helpers
// ============================================================================

/// Low-discrepancy angle sequence using the golden ratio, giving uniform
/// coverage of the hemisphere without clustering for any ray count.
#[inline]
fn golden_ratio_seq(i: u32) -> f32 {
    // Golden angle ≈ 2π × (1 − 1/φ)
    const GOLDEN: f32 = 0.618_033_9;
    (i as f32 * GOLDEN).fract()
}
