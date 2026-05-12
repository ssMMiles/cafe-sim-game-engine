//! 3D mesh thumbnails for the buy menu.
//!
//! For every piece in the catalog we spin up a private mini-scene on its own
//! render layer: a `Camera3d` rendering to an `Image`, a key light, and the
//! piece itself painted in its catalog color. The shop UI renders the image
//! as an `ImageNode` inside each piece card.

use bevy::asset::RenderAssetUsages;
use bevy::camera::{ClearColorConfig, RenderTarget, visibility::RenderLayers};
use bevy::prelude::*;
use bevy::render::render_resource::{
    Extent3d, TextureDimension, TextureFormat, TextureUsages,
};
use std::collections::HashMap;

use crate::catalog::CATALOG;
use crate::grid::PIECE_SCALE;
use crate::placement::PaintMaterial;
use crate::spawn::SpawnOwnedWorld;

const THUMB_SIZE: u32 = 256;
/// First render layer reserved for thumbnails. Each piece reserves the next
/// layer up, so layers `[FIRST_THUMB_LAYER, FIRST_THUMB_LAYER + N)` are owned
/// by the thumbnail rig.
const FIRST_THUMB_LAYER: usize = 8;
/// Y offset for the hidden thumbnail world. Far enough above the cafe that no
/// stray physics or culling logic in the main scene can reach the previews.
const THUMB_WORLD_Y: f32 = 1000.0;

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
        ));

        // Key light for the thumbnail scene.
        world.spawn_owned((
            DirectionalLight {
                illuminance: 6000.0,
                shadows_enabled: false,
                ..default()
            },
            Transform::from_translation(origin + Vec3::new(3.0, 4.0, 3.0))
                .looking_at(origin, Vec3::Y),
            layers.clone(),
        ));

        // The piece itself, painted with the catalog color.
        let scene: Handle<Scene> = world
            .resource::<AssetServer>()
            .load(piece.asset_path);
        world.spawn_owned((
            SceneRoot(scene),
            Transform::from_translation(origin).with_scale(Vec3::splat(PIECE_SCALE)),
            PaintMaterial {
                color: piece.color(),
                alpha: 1.0,
                unlit: false,
            },
            ThumbnailScene(layers),
        ));

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
