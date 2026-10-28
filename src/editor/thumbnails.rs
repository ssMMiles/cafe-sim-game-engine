//! Rendered thumbnails for the Assets tab.
//!
//! One persistent offscreen rig — a `Camera3d` and a `DirectionalLight` on
//! their own [`RenderLayers`] layer, parked far below the world — renders
//! one queued asset at a time into a small linear-sampled image, which the
//! [`ThumbnailCache`] keeps by catalog key and `editor_ui` binds for egui.
//! Subjects are spawned *without* `ConvertToToon`, so they render with their
//! glTF `StandardMaterial`s under the rig's own light (the toon pipeline
//! would sample a lightmap that doesn't exist for them).
//!
//! The job pipeline advances one state per frame: spawn the subject, wait for
//! its meshes to load (`Aabb`s appear), frame them from a 3/4 view, let the
//! camera render for two frames, then cache the image and despawn. A subject
//! that never loads times out to its glyph. Only cells that are actually
//! drawn request thumbnails, so a large catalog is rendered lazily.

use std::collections::{HashMap, HashSet, VecDeque};

use bevy::{
    camera::{RenderTarget, primitives::Aabb, visibility::RenderLayers},
    core_pipeline::tonemapping::Tonemapping,
    image::ImageSampler,
    prelude::*,
    render::render_resource::{
        Extent3d, TextureDescriptor, TextureDimension, TextureFormat, TextureUsages,
    },
};

use super::catalog::{AssetEntry, AssetKind};
use crate::game::GameEntity;
use crate::game::world::objects::PlacedKind;
use crate::game::world::prefabs::PrefabLibrary;

/// Layers 0 (world) and 1 (presentation sprite + UI) are taken.
const THUMBNAIL_LAYER: usize = 7;
const THUMBNAIL_SIZE: u32 = 128;
/// Where subjects are spawned: far enough below the terrain that nothing in
/// the world (or its bounds) can wander into frame.
const RIG_ORIGIN: Vec3 = Vec3::new(0.0, -5000.0, 0.0);
/// Frames to wait for a subject's meshes before giving up.
const LOAD_TIMEOUT_FRAMES: u32 = 240;
/// Frames the camera renders once framed: one for transform propagation
/// and extraction, one to be safe.
const SETTLE_FRAMES: u32 = 2;
const FOV: f32 = std::f32::consts::PI / 5.0;

#[derive(Resource, Default)]
pub struct ThumbnailCache {
    /// Finished thumbnails by catalog thumbnail key.
    images: HashMap<String, Handle<Image>>,
    /// Entries waiting to be rendered, oldest first; `requested` dedups and
    /// also remembers failures so a cell that can't be rendered keeps its
    /// glyph instead of re-queueing every pass.
    pending: VecDeque<(String, ThumbnailJob)>,
    requested: HashSet<String>,
}

#[derive(Debug, Clone)]
pub enum ThumbnailJob {
    Mesh { path: String },
    Prefab { name: String },
}

impl ThumbnailCache {
    pub fn get(&self, key: &str) -> Option<&Handle<Image>> {
        self.images.get(key)
    }

    pub fn images(&self) -> impl Iterator<Item = &Handle<Image>> {
        self.images.values()
    }

    /// Queue a render for `entry` if it can have one and isn't already
    /// queued.
    pub fn request(&mut self, entry: &AssetEntry) {
        let key = entry.thumbnail_key().to_owned();
        let job = match (entry.kind, &entry.prefab) {
            (AssetKind::Prefab, Some(name)) => ThumbnailJob::Prefab { name: name.clone() },
            // Presets key on their mesh path, so they share its thumbnail.
            (AssetKind::Mesh, None) => ThumbnailJob::Mesh { path: key.clone() },
            // Built-ins, textures and audio draw glyphs or themselves.
            _ => return,
        };
        if self.requested.insert(key.clone()) {
            self.pending.push_back((key, job));
        }
    }

    /// A prefab was re-saved: its thumbnail is stale.
    pub fn invalidate(&mut self, key: &str) {
        self.images.remove(key);
        self.requested.remove(key);
    }
}

// ── The rig ──────────────────────────────────────────────────────────────────

#[derive(Component)]
struct ThumbnailSubject;

enum RigState {
    Idle,
    /// Subject spawned; waiting for its meshes.
    Loading {
        key: String,
        root: Entity,
        frames: u32,
        /// Mesh descendants seen last frame, to detect a settled scene.
        seen: usize,
    },
    /// Framed and rendering.
    Settling {
        key: String,
        root: Entity,
        target: Handle<Image>,
        frames_left: u32,
    },
}

#[derive(Resource)]
pub struct ThumbnailRig {
    camera: Entity,
    state: RigState,
}

fn thumbnail_target(images: &mut Assets<Image>) -> Handle<Image> {
    let size = Extent3d {
        width: THUMBNAIL_SIZE,
        height: THUMBNAIL_SIZE,
        ..default()
    };
    let mut image = Image {
        texture_descriptor: TextureDescriptor {
            label: Some("asset thumbnail"),
            size,
            dimension: TextureDimension::D2,
            format: TextureFormat::Rgba8UnormSrgb,
            mip_level_count: 1,
            sample_count: 1,
            usage: TextureUsages::TEXTURE_BINDING
                | TextureUsages::COPY_DST
                | TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        },
        // Explicit: the app's default sampler is nearest (pixel-art look),
        // which would make a scaled-down thumbnail sparkle.
        sampler: ImageSampler::linear(),
        ..default()
    };
    image.resize(size);
    images.add(image)
}

fn spawn_rig(commands: &mut Commands, images: &mut Assets<Image>) -> ThumbnailRig {
    let layer = RenderLayers::layer(THUMBNAIL_LAYER);
    // The camera needs *a* target even while inactive; it is re-pointed per
    // job.
    let placeholder = thumbnail_target(images);
    let camera = commands
        .spawn((
            GameEntity,
            Name::new("thumbnail camera"),
            Camera3d::default(),
            Camera {
                is_active: false,
                order: -5,
                clear_color: ClearColorConfig::Custom(Color::srgb(0.16, 0.16, 0.18)),
                ..default()
            },
            RenderTarget::Image(placeholder.into()),
            Projection::Perspective(PerspectiveProjection {
                fov: FOV,
                ..default()
            }),
            Tonemapping::Linear,
            Transform::from_translation(RIG_ORIGIN + Vec3::new(0.0, 2.0, 6.0))
                .looking_at(RIG_ORIGIN, Vec3::Y),
            layer.clone(),
        ))
        .id();
    // Key light from the camera's side; the world's own sun is on layer 0 and
    // never reaches this view.
    commands.spawn((
        GameEntity,
        Name::new("thumbnail light"),
        DirectionalLight {
            illuminance: 12_000.0,
            shadow_maps_enabled: false,
            ..default()
        },
        Transform::from_translation(RIG_ORIGIN + Vec3::new(3.0, 6.0, 4.0))
            .looking_at(RIG_ORIGIN, Vec3::Y),
        layer,
    ));
    ThumbnailRig {
        camera,
        state: RigState::Idle,
    }
}

/// Spawn the subject for `job` under one root at the rig origin. Returns
/// `None` when there is nothing renderable (an unknown prefab, or one with no
/// meshes).
fn spawn_subject(
    commands: &mut Commands,
    asset_server: &AssetServer,
    prefabs: &PrefabLibrary,
    job: &ThumbnailJob,
) -> Option<Entity> {
    let scenes: Vec<(String, Transform)> = match job {
        ThumbnailJob::Mesh { path } => vec![(path.clone(), Transform::IDENTITY)],
        ThumbnailJob::Prefab { name } => prefabs
            .get(name)?
            .entities
            .iter()
            .filter_map(|e| match &e.kind {
                PlacedKind::Mesh { asset_path, .. } => Some((asset_path.clone(), e.transform())),
                _ => None,
            })
            .collect(),
    };
    if scenes.is_empty() {
        return None;
    }
    let root = commands
        .spawn((
            GameEntity,
            ThumbnailSubject,
            Transform::from_translation(RIG_ORIGIN),
            GlobalTransform::default(),
            Visibility::Visible,
            RenderLayers::layer(THUMBNAIL_LAYER),
        ))
        .id();
    for (path, transform) in scenes {
        let scene: Handle<WorldAsset> = asset_server.load(path);
        commands.spawn((
            GameEntity,
            ChildOf(root),
            WorldAssetRoot(scene),
            transform,
            GlobalTransform::default(),
            Visibility::Inherited,
        ));
    }
    Some(root)
}

/// Where to put the camera to frame `(center, radius)` from a 3/4 view.
fn frame(center: Vec3, radius: f32) -> Transform {
    let dir = Vec3::new(1.0, 0.75, 1.4).normalize();
    let distance = (radius / (FOV * 0.5).sin()) * 1.15;
    Transform::from_translation(center + dir * distance.max(0.5)).looking_at(center, Vec3::Y)
}

/// One state step per frame.
#[allow(clippy::too_many_arguments)]
pub fn drive_thumbnails(
    mut commands: Commands,
    mut rig: Option<ResMut<ThumbnailRig>>,
    mut cache: ResMut<ThumbnailCache>,
    mut images: ResMut<Assets<Image>>,
    asset_server: Res<AssetServer>,
    prefabs: Res<PrefabLibrary>,
    children: Query<&Children>,
    meshes: Query<
        (
            Entity,
            Option<&Aabb>,
            &GlobalTransform,
            Option<&RenderLayers>,
        ),
        With<Mesh3d>,
    >,
    mut cameras: Query<(&mut Camera, &mut RenderTarget, &mut Transform)>,
) {
    let Some(rig) = rig.as_deref_mut() else {
        // Nothing queued yet: don't spend a camera on an empty catalog.
        if !cache.pending.is_empty() {
            let rig = spawn_rig(&mut commands, &mut images);
            commands.insert_resource(rig);
        }
        return;
    };

    match &mut rig.state {
        RigState::Idle => {
            let Some((key, job)) = cache.pending.pop_front() else {
                return;
            };
            match spawn_subject(&mut commands, &asset_server, &prefabs, &job) {
                Some(root) => {
                    rig.state = RigState::Loading {
                        key,
                        root,
                        frames: 0,
                        seen: 0,
                    };
                }
                None => info!("thumbnail '{key}': nothing renderable; keeping its glyph"),
            }
        }

        RigState::Loading {
            key,
            root,
            frames,
            seen,
        } => {
            *frames += 1;
            // Stamp our layer onto every mesh as it appears (`RenderLayers`
            // isn't inherited), and gather bounds once they exist.
            let mut bounds: Vec<(Vec3, Vec3)> = Vec::new();
            let mut mesh_count = 0;
            let mut unbounded = false;
            for e in children.iter_descendants(*root) {
                let Ok((entity, aabb, gt, layers)) = meshes.get(e) else {
                    continue;
                };
                mesh_count += 1;
                if layers.is_none_or(|l| !l.intersects(&RenderLayers::layer(THUMBNAIL_LAYER))) {
                    commands
                        .entity(entity)
                        .insert(RenderLayers::layer(THUMBNAIL_LAYER));
                }
                match aabb {
                    Some(aabb) => {
                        let c = Vec3::from(aabb.center);
                        let h = Vec3::from(aabb.half_extents);
                        let mut min = Vec3::splat(f32::INFINITY);
                        let mut max = Vec3::splat(f32::NEG_INFINITY);
                        for i in 0..8 {
                            let corner = c + h * Vec3::new(
                                if i & 1 == 0 { -1.0 } else { 1.0 },
                                if i & 2 == 0 { -1.0 } else { 1.0 },
                                if i & 4 == 0 { -1.0 } else { 1.0 },
                            );
                            let p = gt.transform_point(corner);
                            min = min.min(p);
                            max = max.max(p);
                        }
                        bounds.push((min, max));
                    }
                    None => unbounded = true,
                }
            }

            // Settled: the mesh count stopped changing, every mesh has bounds,
            // and transforms have had a frame to propagate.
            let settled = mesh_count > 0 && !unbounded && mesh_count == *seen && *frames > 2;
            *seen = mesh_count;

            if settled {
                let (min, max) = bounds.iter().fold(
                    (Vec3::splat(f32::INFINITY), Vec3::splat(f32::NEG_INFINITY)),
                    |(lo, hi), (a, b)| (lo.min(*a), hi.max(*b)),
                );
                let center = (min + max) * 0.5;
                let radius = ((max - min) * 0.5).length().max(0.05);
                let target = thumbnail_target(&mut images);
                if let Ok((mut camera, mut render_target, mut transform)) =
                    cameras.get_mut(rig.camera)
                {
                    *render_target = RenderTarget::Image(target.clone().into());
                    camera.is_active = true;
                    *transform = frame(center, radius);
                }
                rig.state = RigState::Settling {
                    key: std::mem::take(key),
                    root: *root,
                    target,
                    frames_left: SETTLE_FRAMES,
                };
            } else if *frames > LOAD_TIMEOUT_FRAMES {
                warn!("thumbnail '{key}': subject never loaded; keeping its glyph");
                commands.entity(*root).despawn();
                rig.state = RigState::Idle;
            }
        }

        RigState::Settling {
            key,
            root,
            target,
            frames_left,
        } => {
            *frames_left -= 1;
            if *frames_left == 0 {
                if let Ok((mut camera, _, _)) = cameras.get_mut(rig.camera) {
                    camera.is_active = false;
                }
                commands.entity(*root).despawn();
                cache.images.insert(std::mem::take(key), target.clone());
                rig.state = RigState::Idle;
            }
        }
    }
}

/// `OnExit(AppState::InGame)`: the rig's entities go with the world
/// (`GameEntity` cleanup), so forget them; cached images stay valid.
pub fn reset_thumbnail_rig(mut commands: Commands, rig: Option<Res<ThumbnailRig>>) {
    if rig.is_some() {
        commands.remove_resource::<ThumbnailRig>();
    }
}
