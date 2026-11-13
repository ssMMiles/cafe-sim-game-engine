use std::f32::consts::PI;

use crate::{
    game::{
        GameEntity, gfx::post_process::EdgeDetectionSettings, simulation::Player,
        world::terrain::HeightmapData,
    },
    settings::{CameraView, InputLock, MovementMode, UserSettings, setting_ranges},
};
use bevy::{
    anti_alias::fxaa::Fxaa,
    // `Hdr` moved from `bevy_render::view` to `bevy_camera` in Bevy 0.19 —
    // it is a camera property, not a view one.
    camera::{Hdr, RenderTarget, visibility::RenderLayers},
    core_pipeline::{
        prepass::{DepthPrepass, NormalPrepass},
        tonemapping::Tonemapping,
    },
    image::ImageSampler,
    input::mouse::{MouseMotion, MouseWheel},
    prelude::*,
    render::render_resource::{
        Extent3d, TextureDescriptor, TextureDimension, TextureFormat, TextureUsages,
    },
    sprite_render::{MeshMaterial2d, SpriteMeshMaterial},
    window::PrimaryWindow,
};

pub const RENDER_TEXTURE_WIDTH: u32 = 1920;
pub const RENDER_TEXTURE_HEIGHT: u32 = 1080;
const CAMERA_CLEARANCE: f32 = 0.5; // Min camera height above terrain in FloorBased mode
const EYE_HEIGHT: f32 = 1.7; // Camera offset above player origin in first-person
const FOV_FIRST_PERSON: f32 = PI / 2.5; // ~72 degrees
const FOV_THIRD_PERSON: f32 = PI / 5.0; // ~36 degrees (original)

pub use engine_api::PlayerCamera;

#[derive(Component)]
pub struct RenderTextureSprite;

/// The image the 3D camera renders into. The editor shows it inside an egui
/// tab (and resizes it to fit); gameplay presents it via the full-window
/// sprite.
#[derive(Resource, Clone)]
pub struct GameRenderTarget(pub Handle<Image>);

pub fn setup_camera(commands: &mut Commands, images: &mut ResMut<Assets<Image>>) {
    let initial_transform = Transform::default();
    let (yaw, pitch, _) = initial_transform.rotation.to_euler(EulerRot::YXZ);

    let size = Extent3d {
        width: RENDER_TEXTURE_WIDTH,
        height: RENDER_TEXTURE_HEIGHT,
        ..default()
    };

    let mut render_texture = Image {
        texture_descriptor: TextureDescriptor {
            label: None,
            size,
            dimension: TextureDimension::D2,
            format: TextureFormat::Rgba16Float, // HDR format required for atmosphere
            mip_level_count: 1,
            sample_count: 1,
            usage: TextureUsages::TEXTURE_BINDING
                | TextureUsages::COPY_DST
                | TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        },
        sampler: ImageSampler::nearest(), // For pixelated look
        ..default()
    };

    // Fill image.data with zeroes
    render_texture.resize(size);

    let render_texture_handle = images.add(render_texture);
    commands.insert_resource(GameRenderTarget(render_texture_handle.clone()));
    let render_texture_layer = RenderLayers::layer(0);

    // The camera to render all entities on render layer #0, which is the default layer for
    // entities, unless otherwise specified.
    let _camera_entity = commands
        .spawn((
            GameEntity,
            Camera3d::default(),
            Camera::default(),
            RenderTarget::Image(render_texture_handle.clone().into()),
            PlayerCamera { yaw, pitch },
            Projection::Perspective(PerspectiveProjection {
                fov: (PI / 5.),
                ..default()
            }),
            initial_transform,
            render_texture_layer,
            // Enable depth and normal prepasses for edge detection
            DepthPrepass,
            NormalPrepass,
            // Both filled by their sync systems before the first frame
            // renders — from `EdgeStyle` and `AtmosphereStyle`.
            EdgeDetectionSettings::default(),
            crate::game::gfx::atmosphere::AtmosphereSettings::default(),
            Fxaa::default(),
        ))
        .insert((
            // HDR intermediate render target: the toon bands are quantised
            // from unclamped radiance.
            Hdr,
            // Toon shaders already map HDR→display; skip the filmic LUT to avoid double-compression
            // 0.20 made `None` a full passthrough (no `DebandDither`, no clamp of negative
            // channels). `Linear` is the new name for "no tone curve, keep the clamp" — the
            // behaviour this camera had before, and the safe input to the 5-band quantiser.
            Tonemapping::Linear,
            // No `DistanceFog` / `VolumetricFog`: fog is our own post-process
            // (`gfx::atmosphere`), which reaches every surface that wrote
            // depth rather than only `StandardMaterial` ones, and leaves the
            // sky alone. Bevy's volumetric fog needs a real-time directional
            // shadow map, which `BakedSun` deliberately has not — see
            // `docs/ATMOSPHERE.md` §5.
        ))
        .id();

    //
    // A sprite and orthographic camera is used to present the render texture.
    // These are on render layer #1
    //
    commands.spawn((
        GameEntity,
        Sprite::from_image(render_texture_handle),
        RenderTextureSprite,
        RenderLayers::layer(1),
    ));

    commands.spawn((
        GameEntity,
        IsDefaultUiCamera,
        // The editor's egui context draws into this camera's target (the
        // window), above the sprite. Attached here rather than auto-created
        // because auto-creation would pick the main menu's camera.
        bevy_egui::PrimaryEguiContext,
        Camera2d,
        // PixelZoom::FitSize {
        //         width: RENDER_TEXTURE_WIDTH as i32,
        //         height: RENDER_TEXTURE_HEIGHT as i32,
        // },
        // PixelViewport,
        RenderLayers::layer(1),
        // This camera only blits the already-anti-aliased render texture and
        // hosts UI, at full window resolution, so neither MSAA nor FXAA buys
        // anything here — and `Camera` requires `Msaa`, whose default is
        // Sample4: without the override the 2D passes render the fullscreen
        // sprite into a 4× multisampled window-size texture and resolve it
        // (~1 ms/frame of pure bandwidth measured, `tools/gpu-trace.py`).
        // bevy_ui and bevy_egui draw into the *unsampled* attachment with
        // 1-sample pipelines either way, so they don't notice. A second
        // `Fxaa` here used to blur the nearest-upscaled pixels for another
        // ~0.3 ms.
        Msaa::Off,
    ));
}

/// System 1: Mouse look + mode/view toggles. Applies to camera transform directly.
pub fn handle_camera_input(
    keyboard_input: Res<ButtonInput<KeyCode>>,
    mut mouse_motion: MessageReader<MouseMotion>,
    mut settings: ResMut<UserSettings>,
    input_lock: Res<InputLock>,
    mut camera_query: Query<(&mut Transform, &mut PlayerCamera), Without<Player>>,
    mut player_query: Query<&mut Transform, With<Player>>,
    heightmap: Option<Res<HeightmapData>>,
) {
    let Ok((mut camera_transform, mut camera)) = camera_query.single_mut() else {
        return;
    };
    let Ok(mut player_transform) = player_query.single_mut() else {
        return;
    };

    // Mode toggles
    let toggle_mode =
        keyboard_input.just_pressed(KeyCode::KeyM) && keyboard_input.pressed(KeyCode::ShiftLeft);
    let toggle_third_person =
        keyboard_input.just_pressed(KeyCode::KeyV) && keyboard_input.pressed(KeyCode::ShiftLeft);

    if toggle_mode {
        match settings.movement_mode {
            MovementMode::FreeCam => {
                // Snap to terrain when switching to floor-based
                if let Some(ref hm) = heightmap
                    && let Some(h) = hm.sample_height(
                        player_transform.translation.x,
                        player_transform.translation.z,
                    )
                {
                    player_transform.translation.y = h;
                }
            }
            MovementMode::FloorBased => {}
        };
        settings.toggle_movement_mode();
    }
    if toggle_third_person {
        settings.toggle_camera_view();
    }

    // Drain motion events even when locked so input doesn't pile up and
    // snap the camera the moment the lock is released.
    let mut mouse_delta = Vec2::ZERO;
    for motion in mouse_motion.read() {
        mouse_delta += motion.delta;
    }
    if input_lock.is_locked() {
        return;
    }

    if mouse_delta != Vec2::ZERO {
        camera.yaw -= mouse_delta.x * settings.mouse_sensitivity;
        camera.pitch -= mouse_delta.y * settings.mouse_sensitivity;
        camera.pitch = camera.pitch.clamp(-PI / 2.0 + 0.01, PI / 2.0 - 0.01);

        camera_transform.rotation = Quat::from_euler(EulerRot::YXZ, camera.yaw, camera.pitch, 0.0);
    }
}

/// System 2: FreeCam movement — only active when movement mode is FreeCam.
/// Reads keyboard, moves player transform directly (existing behavior).
pub fn apply_freecam_movement(
    keyboard_input: Res<ButtonInput<KeyCode>>,
    settings: Res<UserSettings>,
    input_lock: Res<InputLock>,
    camera_query: Query<&Transform, (With<PlayerCamera>, Without<Player>)>,
    mut player_query: Query<&mut Transform, With<Player>>,
) {
    if settings.movement_mode != MovementMode::FreeCam || input_lock.is_locked() {
        return;
    }

    let Ok(camera_transform) = camera_query.single() else {
        return;
    };
    let Ok(mut player_transform) = player_query.single_mut() else {
        return;
    };

    let mut movement = Vec3::ZERO;

    if keyboard_input.pressed(KeyCode::KeyW) {
        movement += camera_transform.forward().as_vec3();
    }
    if keyboard_input.pressed(KeyCode::KeyS) {
        movement -= camera_transform.forward().as_vec3();
    }
    if keyboard_input.pressed(KeyCode::KeyA) {
        movement -= camera_transform.right().as_vec3();
    }
    if keyboard_input.pressed(KeyCode::KeyD) {
        movement += camera_transform.right().as_vec3();
    }
    if keyboard_input.pressed(KeyCode::Space) {
        movement += Vec3::Y;
    }
    if keyboard_input.pressed(KeyCode::ControlLeft) {
        movement -= Vec3::Y;
    }

    if movement.length() > 0.0 {
        movement = movement.normalize() * settings.movement_speed;
    }

    player_transform.translation += movement;
}

/// System 3: Position camera relative to player (runs after physics or freecam movement).
pub fn camera_follow(
    settings: Res<UserSettings>,
    heightmap: Option<Res<HeightmapData>>,
    mut player_query: Query<(&Transform, &mut Visibility), With<Player>>,
    mut camera_query: Query<(&mut Transform, &PlayerCamera, &mut Projection), Without<Player>>,
) {
    let Ok((player_transform, mut player_visibility)) = player_query.single_mut() else {
        return;
    };
    let Ok((mut camera_transform, _camera, mut projection)) = camera_query.single_mut() else {
        return;
    };

    match settings.camera_view {
        CameraView::FirstPerson => {
            // Hide the character model to avoid seeing inside it
            *player_visibility = Visibility::Hidden;

            // Place camera at eye level
            camera_transform.translation = player_transform.translation + Vec3::Y * EYE_HEIGHT;

            // Use a wider FOV appropriate for first-person
            if let Projection::Perspective(ref mut persp) = *projection {
                persp.fov = FOV_FIRST_PERSON;
            }
        }
        CameraView::ThirdPerson => {
            // Show the character model
            *player_visibility = Visibility::Inherited;

            let offset_direction = camera_transform.forward().as_vec3();
            let mut desired_pos =
                player_transform.translation - offset_direction * settings.third_person_distance;

            // In FloorBased mode, prevent the camera from clipping into terrain.
            if settings.movement_mode == MovementMode::FloorBased
                && let Some(ref hm) = heightmap
                && let Some(terrain_h) = hm.sample_height(desired_pos.x, desired_pos.z)
            {
                let min_y = terrain_h + CAMERA_CLEARANCE;
                if desired_pos.y < min_y {
                    desired_pos.y = min_y;
                }
            }

            camera_transform.translation = desired_pos;

            // Restore third-person FOV
            if let Projection::Perspective(ref mut persp) = *projection {
                persp.fov = FOV_THIRD_PERSON;
            }
        }
    }
}

pub fn resize_render_texture_sprite(
    window_query: Query<&Window, With<PrimaryWindow>>,
    mut sprite_query: Query<&mut Sprite, With<RenderTextureSprite>>,
) {
    if let Ok(window) = window_query.single()
        && let Ok(mut sprite) = sprite_query.single_mut()
    {
        sprite.custom_size = Some(Vec2::new(window.width(), window.height()));
    }
}

pub fn adjust_camera_distance(
    mut scroll_events: MessageReader<MouseWheel>,
    mut settings: ResMut<UserSettings>,
    input: Res<ButtonInput<KeyCode>>,
) {
    if settings.camera_view != CameraView::ThirdPerson
        || input.pressed(KeyCode::ShiftLeft)
        || input.pressed(KeyCode::ShiftRight)
    {
        return;
    }

    for event in scroll_events.read() {
        // Adjust distance based on scroll delta
        // Positive scroll = zoom in (decrease distance)
        // Negative scroll = zoom out (increase distance)
        let scroll_sensitivity = 2.0;
        settings.third_person_distance -= event.y * scroll_sensitivity;

        // Clamp to min/max range
        let (min_dist, max_dist) = setting_ranges::THIRD_PERSON_DISTANCE;
        settings.third_person_distance = settings.third_person_distance.clamp(min_dist, max_dist);
    }
}

/// Re-prepare every sprite material whose image changed, one frame later.
///
/// Bevy 0.20 draws each `Sprite` as a `Mesh2d` quad with a cached
/// `SpriteMeshMaterial` (`bevy_sprite_render::sprite_mesh`), and a material's
/// bind group captures its image's GPU texture view when the *material* is
/// prepared. Nothing re-prepares it when only the image changes. Resizing an
/// image replaces its GPU texture, so the material keeps sampling the old one.
///
/// That is what froze gameplay after an editor round trip: the editor resizes
/// [`GameRenderTarget`] to its viewport tab and `restore_render_target` sizes
/// it back, so the render texture gets a new GPU texture each time. The first
/// gameplay entry got a fresh material only because the sprite's
/// `custom_size` was new, which is part of bevy's material cache key. The
/// second entry hit the cache and got gameplay 1's material, still bound to
/// gameplay 1's texture. The camera rendered into the new texture every frame
/// while the screen showed the old one.
///
/// Marking the material modified makes bevy re-prepare it against the current
/// texture. It waits a frame because bevy prepares 2D materials with no
/// ordering against images: on the frame of the change the material could be
/// re-prepared against the old texture again, but by the next frame the new
/// one exists.
pub fn refresh_sprite_materials_after_image_change(
    mut image_events: MessageReader<AssetEvent<Image>>,
    mut changed_last_frame: Local<Vec<AssetId<Image>>>,
    sprites: Query<(&Sprite, &MeshMaterial2d<SpriteMeshMaterial>)>,
    mut materials: ResMut<Assets<SpriteMeshMaterial>>,
) {
    if !changed_last_frame.is_empty() {
        for (sprite, material) in &sprites {
            if changed_last_frame.contains(&sprite.image.id())
                && let Some(mut material) = materials.get_mut(&material.0)
            {
                // `get_mut` only emits `Modified` once the asset is borrowed
                // mutably.
                let _: &mut SpriteMeshMaterial = &mut material;
            }
        }
        changed_last_frame.clear();
    }
    changed_last_frame.extend(image_events.read().filter_map(|event| match event {
        AssetEvent::Modified { id } => Some(*id),
        _ => None,
    }));
}
