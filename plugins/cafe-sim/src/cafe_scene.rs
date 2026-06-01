//! Spawns the cafe glTF scene and overrides materials on its named nodes.
//!
//! The Cafe.gltf file ships geometry only — no materials — so without
//! intervention every surface ends up with bevy's default white material and
//! never enters the toon shading pipeline. We attach a one-shot
//! `SceneInstanceReady` observer that walks the top-level nodes and assigns
//! `ExtendedMaterial<StandardMaterial, ToonMaterial>` to each, picking the
//! base color/texture from the node's name (`Floor` / `*Wall`).

use bevy::image::{ImageAddressMode, ImageLoaderSettings, ImageSampler, ImageSamplerDescriptor};
use bevy::math::Affine2;
use bevy::pbr::ExtendedMaterial;
use bevy::{log, prelude::*};
use bevy::scene::SceneInstanceReady;
use engine_api::AutoCollider;
use engine_api::toon_material::{DefaultToonLightmap, ToonMaterial};

use crate::grid::FLOOR_Y;
use crate::spawn::SpawnOwnedCommands;

/// Stashes the brick texture handle on the SceneRoot so the scene-ready
/// observer can read it without needing a global resource.
#[derive(Component)]
pub struct CafeWallTexture(pub Handle<Image>);

pub fn spawn_cafe_scene(commands: &mut Commands, asset_server: &AssetServer) {
    let cafe: Handle<Scene> = asset_server.load("cafe-sim/Cafe.gltf#Scene0");

    let bricks: Handle<Image> = asset_server.load_with_settings(
        "cafe-sim/brick-wall.jpg",
        |s: &mut ImageLoaderSettings| {
            s.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor {
                address_mode_u: ImageAddressMode::Repeat,
                address_mode_v: ImageAddressMode::Repeat,
                ..ImageSamplerDescriptor::default()
            });
        },
    );

    commands
        .spawn_owned((
            SceneRoot(cafe),
            Transform::from_xyz(0.0, FLOOR_Y, 0.0),
            AutoCollider,
            CafeWallTexture(bricks),
        ))
        .observe(on_cafe_scene_ready);
}

fn on_cafe_scene_ready(
``    trigger: On<SceneInstanceReady>,
    mut commands: Commands,
    root_q: Query<&CafeWallTexture>,
    name_q: Query<(&Name, &Transform)>,
    children_q: Query<&Children>,
    mesh_q: Query<Entity, With<Mesh3d>>,
    mut toon_materials: ResMut<Assets<ExtendedMaterial<StandardMaterial, ToonMaterial>>>,
    default_lightmap: Option<Res<DefaultToonLightmap>>,
) {
    let root = trigger.entity;
    log::info!("[cafe-sim] on_cafe_scene_ready fired for {root:?}");

    let Ok(brick_tex) = root_q.get(root) else {
        log::warn!("[cafe-sim] cafe scene ready but no CafeWallTexture on {root:?}");
        return;
    };
    let brick = brick_tex.0.clone();
    let lightmap = default_lightmap
        .as_ref()
        .map(|d| d.0.clone())
        .unwrap_or_default();

    // Walk the whole spawned subtree — the glTF loader can wrap nodes under
    // intermediate parents, so don't assume our named nodes are direct
    // children of the SceneRoot.
    let mut stack: Vec<Entity> = match children_q.get(root) {
        Ok(c) => c.iter().collect(),
        Err(_) => {
            log::warn!("[cafe-sim] SceneRoot {root:?} has no children");
            return;
        }
    };
    let mut named_seen = 0u32;
    let mut meshes_assigned = 0u32;

    while let Some(node) = stack.pop() {
        if let Ok(c) = children_q.get(node) {
            stack.extend(c.iter());
        }

        let Ok((name, tf)) = name_q.get(node) else {
            continue;
        };
        let n = name.as_str();
        named_seen += 1;
        log::info!("[cafe-sim] saw named node {n:?}");

        let material = if n.ends_with("Wall") {
            // Walls are unit cubes whose UVs run 0..1 per face; the node
            // scale stretches the face to world size, so we tile in
            // proportion. Tweak the multipliers to taste.
            let tiles = Vec2::new(tf.scale.x * 12.0, tf.scale.y * 12.0);
            toon_materials.add(ExtendedMaterial {
                base: StandardMaterial {
                    base_color_texture: Some(brick.clone()),
                    perceptual_roughness: 0.9,
                    uv_transform: Affine2::from_scale(tiles),
                    ..default()
                },
                extension: ToonMaterial {
                    lightmap: lightmap.clone(),
                    use_lightmap: 1,
                },
            })
        } else if n == "Floor" {
            toon_materials.add(ExtendedMaterial {
                base: StandardMaterial {
                    base_color: Color::srgb(0.55, 0.3, 0.35),
                    perceptual_roughness: 0.95,
                    ..default()
                },
                extension: ToonMaterial {
                    lightmap: lightmap.clone(),
                    use_lightmap: 1,
                },
            })
        } else {
            continue;
        };

        // Recurse only this node's subtree to find every Mesh3d that
        // belongs to it.
        let mut sub: Vec<Entity> = vec![node];
        while let Some(e) = sub.pop() {
            if mesh_q.get(e).is_ok() {
                commands
                    .entity(e)
                    .remove::<MeshMaterial3d<StandardMaterial>>()
                    .insert(MeshMaterial3d(material.clone()));
                meshes_assigned += 1;
            }
            if let Ok(c) = children_q.get(e) {
                sub.extend(c.iter());
            }
        }
    }

    log::info!(
        "[cafe-sim] cafe scene processed: {named_seen} named node(s), {meshes_assigned} mesh(es) assigned"
    );
}
