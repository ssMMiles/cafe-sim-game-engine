//! Blob shadows under dynamic objects.
//!
//! Every static shadow in the game is baked into a lightmap, and the baked
//! sun casts no runtime shadow map (`BakedSun` has shadows off). So a moving
//! object — the player, a plugin's NPC — stands on sunlit ground with no
//! visual contact at all. This is the cheapest fix that works everywhere: a
//! soft dark disc placed on whatever is directly under the object.
//!
//! The disc is a *separate* entity, not a child, so the caster's rotation
//! and scale never tilt or stretch it. Each frame an avian ray goes straight
//! down from the caster's origin, skipping the caster's own colliders, and the
//! disc is set on the hit — terrain or a placed floor mesh alike — aligned to
//! the hit normal and shrunk with height above it. Height drives *scale*, not
//! material alpha, so the shared material is never rewritten and the discs
//! cost nothing per frame beyond their transforms.
//!
//! The material is unlit and alpha-blended: it stays out of the depth
//! prepass, so the edge-detection post-process never draws an outline around
//! it, and it never casts into any shadow pass.

use avian3d::prelude::*;
use bevy::{
    asset::RenderAssetUsages,
    image::{ImageSampler, ImageSamplerDescriptor},
    light::NotShadowCaster,
    prelude::*,
    render::render_resource::{Extent3d, TextureDimension, TextureFormat},
};
use std::f32::consts::FRAC_PI_2;

pub use engine_api::BlobShadow;

use crate::game::GameEntity;

/// How far above the caster's origin the ray starts, so an origin resting
/// exactly on the surface still finds it.
const RAY_LIFT: f32 = 0.3;
/// Longest drop the disc will follow a falling object through.
const RAY_REACH: f32 = 40.0;
/// Offset off the surface so the disc doesn't z-fight it.
const SURFACE_OFFSET: f32 = 0.02;
/// Opacity at the disc centre. The texture fades it to zero at the rim.
const BLOB_ALPHA: f32 = 0.55;
/// Side length of the generated radial falloff texture.
const TEXTURE_SIZE: u32 = 64;

/// Mesh and material shared by every disc, built once at startup.
#[derive(Resource)]
pub struct BlobShadowAssets {
    mesh: Handle<Mesh>,
    material: Handle<StandardMaterial>,
}

/// The disc entity that follows one caster.
#[derive(Component)]
pub struct BlobShadowDisc {
    caster: Entity,
}

pub struct BlobShadowPlugin;

impl Plugin for BlobShadowPlugin {
    fn build(&self, app: &mut App) {
        let assets = {
            let world = app.world_mut();
            let texture = world.resource_mut::<Assets<Image>>().add(radial_falloff());
            let material = world
                .resource_mut::<Assets<StandardMaterial>>()
                .add(StandardMaterial {
                    base_color: Color::srgba(0.0, 0.0, 0.0, BLOB_ALPHA),
                    base_color_texture: Some(texture),
                    alpha_mode: AlphaMode::Blend,
                    unlit: true,
                    ..default()
                });
            let mesh = world.resource_mut::<Assets<Mesh>>().add(Circle::new(1.0));
            BlobShadowAssets { mesh, material }
        };
        app.insert_resource(assets);
        app.add_systems(
            Update,
            (spawn_blob_shadow_discs, update_blob_shadow_discs)
                .chain()
                .in_set(crate::game::SimSet::InGame),
        );
    }
}

/// `TEXTURE_SIZE²` RGBA8: opaque white in the middle falling off smoothly to
/// transparent at the rim. Multiplied by the material's base colour, so the
/// colour and peak alpha live in the material, only the shape lives here.
fn radial_falloff() -> Image {
    let n = TEXTURE_SIZE;
    let mut data = Vec::with_capacity((n * n * 4) as usize);
    for y in 0..n {
        for x in 0..n {
            let u = (x as f32 + 0.5) / n as f32 * 2.0 - 1.0;
            let v = (y as f32 + 0.5) / n as f32 * 2.0 - 1.0;
            let r = (u * u + v * v).sqrt();
            // Flat core, then a smooth fall to zero at the edge of the disc.
            let t = ((r - 0.35) / 0.65).clamp(0.0, 1.0);
            let alpha = 1.0 - t * t * (3.0 - 2.0 * t);
            data.extend_from_slice(&[255, 255, 255, (alpha * 255.0).round() as u8]);
        }
    }
    let mut image = Image::new(
        Extent3d {
            width: n,
            height: n,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        data,
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::RENDER_WORLD,
    );
    // The app default is nearest (pixel-art look); a point-sampled gradient
    // is a set of rings.
    image.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor::linear());
    image
}

/// Give every new caster a disc.
fn spawn_blob_shadow_discs(
    mut commands: Commands,
    assets: Res<BlobShadowAssets>,
    casters: Query<Entity, Added<BlobShadow>>,
) {
    for caster in &casters {
        commands.spawn((
            GameEntity,
            BlobShadowDisc { caster },
            Mesh3d(assets.mesh.clone()),
            MeshMaterial3d(assets.material.clone()),
            Transform::default(),
            Visibility::Hidden,
            NotShadowCaster,
        ));
    }
}

/// Drop each disc onto the surface under its caster, or hide it. Discs whose
/// caster is gone despawn.
fn update_blob_shadow_discs(
    mut commands: Commands,
    spatial_query: SpatialQuery,
    casters: Query<(&GlobalTransform, &BlobShadow)>,
    children: Query<&Children>,
    mut discs: Query<(Entity, &BlobShadowDisc, &mut Transform, &mut Visibility)>,
) {
    for (disc_entity, disc, mut transform, mut visibility) in &mut discs {
        let Ok((caster_tf, blob)) = casters.get(disc.caster) else {
            commands.entity(disc_entity).despawn();
            continue;
        };

        // The caster's own colliders are usually right where the ray starts.
        let own = std::iter::once(disc.caster).chain(children.iter_descendants(disc.caster));
        let filter = SpatialQueryFilter::from_excluded_entities(own);

        let origin = caster_tf.translation() + Vec3::Y * RAY_LIFT;
        let hit = spatial_query.cast_ray(origin, Dir3::NEG_Y, RAY_REACH, true, &filter);

        let Some(hit) = hit else {
            *visibility = Visibility::Hidden;
            continue;
        };

        let height = (hit.distance - RAY_LIFT).max(0.0);
        let fade = 1.0 - (height / blob.fade_height.max(1e-3)).min(1.0);
        if fade <= 0.0 {
            *visibility = Visibility::Hidden;
            continue;
        }

        let normal = hit.normal.normalize_or(Vec3::Y);
        let point = origin + Vec3::NEG_Y * hit.distance;
        // The circle mesh lies in XY facing +Z; lay it flat, then tilt it
        // onto the surface.
        transform.translation = point + normal * SURFACE_OFFSET;
        transform.rotation =
            Quat::from_rotation_arc(Vec3::Y, normal) * Quat::from_rotation_x(-FRAC_PI_2);
        transform.scale = Vec3::splat(blob.radius * fade);
        *visibility = Visibility::Visible;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The falloff must be opaque at the centre and fully transparent at the
    /// corners, with no hard step in between — a visible ring would read as
    /// an outline rather than a shadow.
    #[test]
    fn radial_texture_is_a_smooth_disc() {
        let image = radial_falloff();
        let n = TEXTURE_SIZE as usize;
        let data = image.data.as_ref().expect("generated image has data");
        let alpha = |x: usize, y: usize| data[(y * n + x) * 4 + 3];

        assert_eq!(alpha(n / 2, n / 2), 255, "centre is opaque");
        assert_eq!(alpha(0, 0), 0, "corner is transparent");
        assert_eq!(alpha(n - 1, n / 2), 0, "rim is transparent");

        // Along a radius, alpha never increases and never jumps by more than
        // a few levels per texel.
        let mut prev = alpha(n / 2, n / 2);
        for x in n / 2..n {
            let a = alpha(x, n / 2);
            assert!(a <= prev, "alpha rose from {prev} to {a} at x={x}");
            assert!(prev - a <= 24, "alpha stepped {} at x={x}", prev - a);
            prev = a;
        }

        match &image.sampler {
            ImageSampler::Descriptor(d) => {
                assert_eq!(d.mag_filter, bevy::image::ImageFilterMode::Linear);
            }
            ImageSampler::Default => panic!("gradient would be point-sampled into rings"),
        }
    }
}
