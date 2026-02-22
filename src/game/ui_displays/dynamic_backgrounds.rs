use bevy::{camera::primitives::Aabb, log, prelude::*};

use super::shared::DynamicBackground;

pub fn update_dynamic_backgrounds(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    dynamic_bg_query: Query<(Entity, &DynamicBackground, &Children)>,
    text_query: Query<(&Node, &ComputedNode, &Text)>,
    mesh_query: Query<(&Transform, &Aabb), With<Mesh3d>>,
) {
    for (background_entity, dynamic_bg, children) in dynamic_bg_query.iter() {
        let mut min_bounds = Vec2::new(f32::INFINITY, f32::INFINITY);
        let mut max_bounds = Vec2::new(f32::NEG_INFINITY, f32::NEG_INFINITY);
        let mut has_content = false;

        log::debug!(
            "Processing DynamicBackground entity {:?} with {} children",
            background_entity,
            children.len()
        );

        for child in children.iter() {
            if let Ok((_, computed_node, _)) = text_query.get(child) {
                has_content = true;
                let size = computed_node.size();

                if size.x > 0.0 && size.y > 0.0 {
                    min_bounds = min_bounds.min(Vec2::ZERO);
                    max_bounds = max_bounds.max(size);
                }
            }

            if let Ok((transform, aabb)) = mesh_query.get(child) {
                has_content = true;
                let aabb_min = aabb.center - aabb.half_extents;
                let aabb_max = aabb.center + aabb.half_extents;

                log::debug!("  Child mesh {:?}:", child);
                log::debug!(
                    "    Transform: pos={:?}, rot={:?}, scale={:?}",
                    transform.translation,
                    transform.rotation,
                    transform.scale
                );
                log::debug!(
                    "    Aabb: center={:?}, half_extents={:?}",
                    aabb.center,
                    aabb.half_extents
                );
                log::debug!("    Local bounds: min={:?}, max={:?}", aabb_min, aabb_max);

                let transformed_min = Vec2::new(
                    transform.translation.x + aabb_min.x,
                    transform.translation.y + aabb_min.y,
                );
                let transformed_max = Vec2::new(
                    transform.translation.x + aabb_max.x,
                    transform.translation.y + aabb_max.y,
                );

                log::debug!(
                    "    Transformed bounds: min={:?}, max={:?}",
                    transformed_min,
                    transformed_max
                );

                min_bounds = min_bounds.min(transformed_min);
                max_bounds = max_bounds.max(transformed_max);
            }
        }

        log::debug!(
            "  Final bounds: min={:?}, max={:?}, has_content={}",
            min_bounds,
            max_bounds,
            has_content
        );

        let background_size = if has_content
            && min_bounds.x.is_finite()
            && min_bounds.y.is_finite()
            && max_bounds.x.is_finite()
            && max_bounds.y.is_finite()
        {
            let content_size = max_bounds - min_bounds;
            log::debug!("  Content size: {:?}", content_size);

            if content_size.x > 0.0
                && content_size.y > 0.0
                && content_size.x.is_finite()
                && content_size.y.is_finite()
            {
                let padded_size = content_size + Vec2::splat(dynamic_bg.padding * 2.0);
                log::debug!(
                    "  Padded size (padding={}): {:?}",
                    dynamic_bg.padding,
                    padded_size
                );

                if let Some(min_size) = dynamic_bg.min_size {
                    let final_size = Vec2::new(
                        padded_size.x.max(min_size.x).max(0.01),
                        padded_size.y.max(min_size.y).max(0.01),
                    );
                    log::debug!(
                        "  Final size with min_size {:?}: {:?}",
                        min_size,
                        final_size
                    );
                    final_size
                } else {
                    let final_size = Vec2::new(padded_size.x.max(0.01), padded_size.y.max(0.01));
                    log::debug!("  Final size (no min_size): {:?}", final_size);
                    final_size
                }
            } else {
                dynamic_bg.min_size.unwrap_or(Vec2::ZERO)
            }
        } else {
            dynamic_bg.min_size.unwrap_or(Vec2::ZERO)
        };

        log::debug!("  Background size: {:?}", background_size);

        if background_size == Vec2::ZERO {
            log::debug!("  -> Hiding background (size is zero)");
            commands
                .entity(background_entity)
                .insert(Visibility::Hidden);
        } else if background_size.x > 0.0
            && background_size.y > 0.0
            && background_size.x.is_finite()
            && background_size.y.is_finite()
            && background_size.x < 100.0
            && background_size.y < 100.0
        {
            log::debug!("  -> Creating mesh with size {:?}", background_size);

            if background_size.x <= 0.0 || background_size.y <= 0.0 {
                log::error!(
                    "Attempted to create mesh with invalid size: {:?}",
                    background_size
                );
                continue;
            }

            let new_mesh = meshes.add(Plane3d::new(Vec3::Z, background_size));
            let material_handle = materials.add(dynamic_bg.material.clone());
            commands.entity(background_entity).insert((
                Mesh3d(new_mesh),
                MeshMaterial3d(material_handle),
                Visibility::Inherited,
            ));
        } else {
            log::debug!("  -> Skipping mesh update (invalid size)");
        }
    }
}
