//! 3D mesh thumbnails for the buy menu.
//!
//! For every piece in the catalog we spin up a private mini-scene on its own
//! render layer: a `Camera3d` rendering to an `Image` and the piece itself,
//! flat-shaded in its catalog color (or its own textures). The shop UI
//! renders the image as an `ImageNode` inside each piece card.
//!
//! The cameras render for a fixed number of frames once every piece has its
//! meshes, then switch off: an active `Camera3d` is a whole 3D view every
//! frame, and there is one per piece. The budget is generous because what
//! has to land in that window is a pipeline compile and a texture upload per
//! piece, not just a frame.

use bevy::asset::RenderAssetUsages;
use bevy::camera::{ClearColorConfig, RenderTarget, visibility::RenderLayers};
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat, TextureUsages};
use std::collections::HashMap;

use crate::catalog::CATALOG;
use crate::grid::PIECE_SCALE;
use crate::placement::{PaintMaterial, PaintShading};
use crate::spawn::SpawnOwnedWorld;
use engine_api::ConvertToToon;

const THUMB_SIZE: u32 = 256;
/// First render layer reserved for thumbnails. Each piece reserves the next
/// layer up, so layers `[FIRST_THUMB_LAYER, FIRST_THUMB_LAYER + N)` are owned
/// by the thumbnail rig.
const FIRST_THUMB_LAYER: usize = 8;
/// Y offset for the hidden thumbnail world. Far enough above the cafe that no
/// stray physics or culling logic in the main scene can reach the previews.
const THUMB_WORLD_Y: f32 = 1000.0;
/// Frames the cameras keep rendering after the last piece's meshes appear.
const SETTLE_FRAMES: u32 = 120;

/// A thumbnail camera, and how many frames it still renders once every
/// piece has loaded (`None` until then).
#[derive(Component)]
pub struct ThumbnailCamera {
    frames_left: Option<u32>,
}

#[derive(Resource, Default)]
pub struct PieceThumbnails(pub HashMap<&'static str, Handle<Image>>);

/// Tag for the root of a thumbnail scene. The propagation system walks
/// descendants spawned by the scene loader and applies the same render layer
/// to every mesh entity so only the matching thumbnail camera draws them.
#[derive(Component, Clone)]
pub struct ThumbnailScene(pub RenderLayers);

pub fn init_thumbnails(world: &mut World) {
    let pieces: Vec<&'static crate::catalog::PieceDef> =
        CATALOG.iter().flat_map(|cat| cat.pieces.iter()).collect();

    let mut thumbs = PieceThumbnails::default();

    for (idx, piece) in pieces.iter().enumerate() {
        let layers = RenderLayers::layer(FIRST_THUMB_LAYER + idx);
        let origin = Vec3::new(idx as f32 * 50.0, THUMB_WORLD_Y, 0.0);

        // Render target image
        let size = Extent3d {
            width: THUMB_SIZE,
            height: THUMB_SIZE,
            depth_or_array_layers: 1,
        };
        let mut image = Image::new_fill(
            size,
            TextureDimension::D2,
            &[0, 0, 0, 0],
            TextureFormat::Bgra8UnormSrgb,
            RenderAssetUsages::default(),
        );
        image.texture_descriptor.usage = TextureUsages::TEXTURE_BINDING
            | TextureUsages::COPY_DST
            | TextureUsages::RENDER_ATTACHMENT;
        let image_handle = world.resource_mut::<Assets<Image>>().add(image);

        // Camera — fixed three-quarter view of the piece, rendering only this
        // thumbnail's render layer into its dedicated image.
        let cam_offset = Vec3::new(1.6, 1.2, 1.6);
        world.spawn_owned((
            Camera3d::default(),
            Camera {
                order: -100 - idx as isize,
                clear_color: ClearColorConfig::Custom(Color::srgba(0.18, 0.22, 0.28, 1.0)),
                ..default()
            },
            RenderTarget::Image(image_handle.clone().into()),
            Transform::from_translation(origin + cam_offset).looking_at(origin, Vec3::Y),
            layers.clone(),
            ThumbnailCamera { frames_left: None },
        ));

        // The piece itself — converted to toon by the host (lightmap source
        // with no lightmap: flat, in its own colours), then painted with the
        // catalog color unless the piece opts out of tinting.
        let scene: Handle<WorldAsset> = world.resource::<AssetServer>().load(piece.asset_path);
        let mut thumb = world.spawn_owned((
            WorldAssetRoot(scene),
            Transform::from_translation(origin).with_scale(Vec3::splat(PIECE_SCALE)),
            ThumbnailScene(layers),
            ConvertToToon,
        ));
        if piece.tint {
            thumb.insert(PaintMaterial {
                color: piece.color(),
                alpha: 1.0,
                shading: PaintShading::Flat,
            });
        }

        thumbs.0.insert(piece.name, image_handle);
    }

    world.insert_resource(thumbs);
}

/// Propagates the thumbnail scene's `RenderLayers` onto every mesh descendant
/// once the gltf scene has finished spawning. Runs each frame until at least
/// one mesh shows up; after that the marker is removed.
pub fn propagate_thumbnail_layers(
    mut commands: Commands,
    roots: Query<(Entity, &ThumbnailScene)>,
    children_q: Query<&Children>,
    mesh_q: Query<(), With<Mesh3d>>,
) {
    for (root, marker) in roots.iter() {
        let mut stack = vec![root];
        let mut descendants = Vec::new();
        while let Some(e) = stack.pop() {
            if let Ok(children) = children_q.get(e) {
                for c in children.iter() {
                    descendants.push(c);
                    stack.push(c);
                }
            }
        }
        if !descendants.iter().any(|&e| mesh_q.contains(e)) {
            continue;
        }
        for &e in &descendants {
            commands.entity(e).insert(marker.0.clone());
        }
        commands.entity(root).remove::<ThumbnailScene>();
    }
}

/// Switch the thumbnail cameras off [`SETTLE_FRAMES`] after the last piece
/// has its meshes (every [`ThumbnailScene`] marker gone).
pub fn settle_thumbnail_cameras(
    pending: Query<(), With<ThumbnailScene>>,
    mut cameras: Query<(&mut Camera, &mut ThumbnailCamera)>,
) {
    if !pending.is_empty() {
        return;
    }
    for (mut camera, mut thumb) in &mut cameras {
        if !camera.is_active {
            continue;
        }
        let left = thumb.frames_left.get_or_insert(SETTLE_FRAMES);
        *left = left.saturating_sub(1);
        if *left == 0 {
            camera.is_active = false;
        }
    }
}
