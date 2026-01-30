use std::f32::consts::PI;

use crate::{
    game::{post_process::EdgeDetectionSettings, simulation::Player},
    settings::{setting_ranges, MovementMode, UserSettings},
};
use bevy::{
    anti_alias::fxaa::Fxaa,
    camera::{visibility::RenderLayers, RenderTarget},
    core_pipeline::prepass::{DepthPrepass, NormalPrepass},
    image::ImageSampler,
    input::mouse::{MouseMotion, MouseWheel},
    prelude::*,
    render::render_resource::{Extent3d, TextureDescriptor, TextureDimension, TextureFormat, TextureUsages},
    window::PrimaryWindow,
};
use bevy_modern_pixel_camera::prelude::*;

const RENDER_TEXTURE_WIDTH: u32 = 1920;
const RENDER_TEXTURE_HEIGHT: u32 = 1080;

#[derive(Component, Debug, Default)]
pub struct PlayerCamera {
    pub velocity: Vec3,
    pub yaw: f32,
    pub pitch: f32,
}

#[derive(Component)]
pub struct RenderTextureSprite;

// Movement speed is now controlled by UserSettings
const ACCELERATION: f32 = 0.2;
const MAX_VELOCITY: f32 = 1.0;
const FLOOR_HEIGHT: f32 = 2.0;

#[derive(Default)]
struct MovementInput {
    forward: bool,
    backward: bool,
    left: bool,
    right: bool,
    up: bool,
    down: bool,
    toggle_mode: bool,
    toggle_third_person: bool,
}

pub fn setup_camera(
    commands: &mut Commands,
    images: &mut ResMut<Assets<Image>>,
    meshes: &mut ResMut<Assets<Mesh>>,
    materials: &mut ResMut<Assets<StandardMaterial>>,
) {
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
    let render_texture_layer = RenderLayers::layer(0);

    // The camera to render all entities on render layer #0, which is the default layer for
    // entities, unless otherwise specified.
    let camera_entity = commands.spawn((
        Camera3d::default(),
        Camera::default(),
        RenderTarget::Image(render_texture_handle.clone().into()),
        PlayerCamera {
            velocity: Vec3::ZERO,
            yaw,
            pitch,
        },
        Projection::Perspective(PerspectiveProjection {
            fov: (PI / 5.),
            ..default()
        }),
        initial_transform,
        render_texture_layer,
        // Enable depth and normal prepasses for edge detection
        DepthPrepass,
        NormalPrepass,
        // Edge detection post-processing settings
        EdgeDetectionSettings::new(),
        Fxaa::default(),
    )).id();

    // Add atmosphere components separately to avoid bundle size limit
    // DISABLED: Atmosphere system conflicts with render-to-texture setup
    // commands.entity(camera_entity).insert((
    //     Atmosphere::EARTH,
    //     AtmosphereSettings::default(),
    //     Exposure { ev100: 13.0 },
    //     Tonemapping::AcesFitted,
    //     Bloom::NATURAL,
    //     AtmosphereEnvironmentMapLight::default(),
    // ));

    let cube_size = 4.0;
    let cube_handle = meshes.add(Cuboid::new(cube_size, cube_size, cube_size));

    // This material has the texture that has been rendered.
    // let material_handle = materials.add(StandardMaterial {
    //     base_color_texture: Some(render_texture_handle),
    //     reflectance: 0.02,
    //     unlit: false,
    //     ..default()
    // });

    // let alt_material_handle = materials.add(StandardMaterial {
    //     base_color: Color::srgb(0.8, 0.2, 0.2),
    //     reflectance: 0.02,
    //     unlit: false,
    //     ..default()
    // });

    // Main pass cube, with material containing the rendered first pass texture.
    // commands.spawn((
    //     Mesh3d(cube_handle),
    //     MeshMaterial3d(material_handle),
    //     Transform::from_xyz(0.0, 0.0, 1.5).with_rotation(Quat::from_rotation_x(-PI / 5.0)),
    //     RenderLayers::layer(1),
    // ));

    // commands.spawn((
    //     Camera3d::default(),
    //     Transform::from_xyz(0.0, 0.0, 15.0).looking_at(Vec3::ZERO, Vec3::Y),
    //     RenderLayers::layer(1),
    // ));

    //
    // A sprite and orthographic camera is used to present the render texture.
    // These are on render layer #1
    //
    commands.spawn((
        Sprite::from_image(render_texture_handle),
        RenderTextureSprite,
        RenderLayers::layer(1),
    ));

    // commands.spawn((Camera2d::default(), RenderLayers::layer(1)));

    commands.spawn((
        IsDefaultUiCamera,
        Camera2d::default(),
        // PixelZoom::FitSize {
        //         width: RENDER_TEXTURE_WIDTH as i32,
        //         height: RENDER_TEXTURE_HEIGHT as i32,
        // },
        // PixelViewport,
        RenderLayers::layer(1),
        // Fxaa::default(),
    ));
}

pub fn handle_movement(
    keyboard_input: Res<ButtonInput<KeyCode>>,
    mut mouse_motion: MessageReader<MouseMotion>,
    mut settings: ResMut<UserSettings>,
    camera_query: Option<Single<(&mut Transform, &mut PlayerCamera), Without<Player>>>,
    player_query: Option<Single<(&mut Transform), With<Player>>>,
) {
    let (mut camera_transform, mut camera) = match camera_query {
        Some(query) => query.into_inner(),
        None => {
            warn!("No camera entity found for camera movement system.");
            return;
        }
    };
    let mut player_transform = match player_query {
        Some(query) => query.into_inner(),
        None => {
            warn!("No player entity found for camera movement system.");
            return;
        }
    };

    let input = collect_movement_input(&keyboard_input);

    handle_mode_toggle(&input, &mut settings, &mut player_transform);
    apply_movement(&input, &mut player_transform, &camera_transform, &settings);

    if settings.third_person_view {
        // Third-person camera with free rotation around player
        // Allow both horizontal and vertical rotation with mouse
        handle_mouse_look(&mut mouse_motion, &mut camera, &mut camera_transform, &settings);

        // Calculate offset direction based on current camera rotation
        let offset_direction = camera_transform.forward().as_vec3();

        // Position camera at offset from player, looking at them
        camera_transform.translation = player_transform.translation - offset_direction * settings.third_person_distance;
    } else {
        // First-person view: sync camera to player
        camera_transform.translation = player_transform.translation;
        handle_mouse_look(&mut mouse_motion, &mut camera, &mut camera_transform, &settings);
    }
}

fn collect_movement_input(keyboard_input: &Res<ButtonInput<KeyCode>>) -> MovementInput {
    MovementInput {
        forward: keyboard_input.pressed(KeyCode::KeyW),
        backward: keyboard_input.pressed(KeyCode::KeyS),
        left: keyboard_input.pressed(KeyCode::KeyA),
        right: keyboard_input.pressed(KeyCode::KeyD),
        up: keyboard_input.pressed(KeyCode::Space),
        down: keyboard_input.pressed(KeyCode::ControlLeft),
        toggle_mode: keyboard_input.just_pressed(KeyCode::KeyM)
            && keyboard_input.pressed(KeyCode::ShiftLeft),
        toggle_third_person: keyboard_input.just_pressed(KeyCode::KeyV)
            && keyboard_input.pressed(KeyCode::ShiftLeft),
    }
}

fn handle_mode_toggle(
    input: &MovementInput,
    settings: &mut UserSettings,
    transform: &mut Transform,
) {
    if input.toggle_mode {
        match settings.movement_mode {
            MovementMode::FreeCam => {
                // Snap to floor when switching to floor-based
                transform.translation.y = FLOOR_HEIGHT;
            }
            MovementMode::FloorBased => {}
        };
        settings.toggle_movement_mode();
    }
    if input.toggle_third_person {
        settings.third_person_view = !settings.third_person_view;
    }
}

fn handle_mouse_look(
    mouse_motion: &mut MessageReader<MouseMotion>,
    camera: &mut PlayerCamera,
    transform: &mut Transform,
    settings: &UserSettings,
) {
    let mut mouse_delta = Vec2::ZERO;
    for motion in mouse_motion.read() {
        mouse_delta += motion.delta;
    }

    if mouse_delta != Vec2::ZERO {
        camera.yaw -= mouse_delta.x * settings.mouse_sensitivity;
        camera.pitch -= mouse_delta.y * settings.mouse_sensitivity;
        camera.pitch = camera.pitch.clamp(-PI / 2.0 + 0.01, PI / 2.0 - 0.01);

        // Apply rotation to transform
        transform.rotation = Quat::from_euler(EulerRot::YXZ, camera.yaw, camera.pitch, 0.0);
    }
}

fn handle_mouse_look_isometric(
    mouse_motion: &mut MessageReader<MouseMotion>,
    camera: &mut PlayerCamera,
    settings: &UserSettings,
) {
    let mut mouse_delta = Vec2::ZERO;
    for motion in mouse_motion.read() {
        mouse_delta += motion.delta;
    }

    if mouse_delta != Vec2::ZERO {
        // Only apply horizontal (yaw) rotation for isometric view
        camera.yaw -= mouse_delta.x * settings.mouse_sensitivity;
        // Pitch stays locked at isometric angle
    }
}

fn apply_movement(input: &MovementInput, transform: &mut Transform, camera_transform: &Transform, settings: &UserSettings) {
    match settings.movement_mode {
        MovementMode::FreeCam => apply_freecam_movement(input, transform, camera_transform, settings),
        MovementMode::FloorBased => apply_floor_movement(input, transform, camera_transform, settings),
    }
}

fn apply_freecam_movement(
    input: &MovementInput,
    transform: &mut Transform,
    camera_transform: &Transform,
    settings: &UserSettings,
) {
    let mut movement = Vec3::ZERO;

    if input.forward {
        movement += camera_transform.forward().as_vec3();
    }
    if input.backward {
        movement -= camera_transform.forward().as_vec3();
    }
    if input.left {
        movement -= camera_transform.right().as_vec3();
    }
    if input.right {
        movement += camera_transform.right().as_vec3();
    }
    if input.up {
        movement += Vec3::Y;
    }
    if input.down {
        movement -= Vec3::Y;
    }

    // Normalize and apply movement
    if movement.length() > 0.0 {
        movement = movement.normalize() * settings.movement_speed;
    }

    transform.translation += movement;
}

fn apply_floor_movement(input: &MovementInput, transform: &mut Transform, camera_transform: &Transform, settings: &UserSettings) {
    let mut movement = Vec3::ZERO;

    if input.forward {
        movement += camera_transform.forward().as_vec3();
    }
    if input.backward {
        movement -= camera_transform.forward().as_vec3();
    }
    if input.left {
        movement -= camera_transform.right().as_vec3();
    }
    if input.right {
        movement += camera_transform.right().as_vec3();
    }

    // Project movement to XZ plane and normalize
    movement.y = 0.0;
    if movement.length() > 0.0 {
        movement = movement.normalize() * settings.movement_speed;
    }

    // Apply movement
    transform.translation.x += movement.x;
    transform.translation.z += movement.z;

    // Keep camera at floor height
    transform.translation.y = FLOOR_HEIGHT;
}

pub fn resize_render_texture_sprite(
    window_query: Query<&Window, With<PrimaryWindow>>,
    mut sprite_query: Query<&mut Sprite, With<RenderTextureSprite>>,
) {
    if let Ok(window) = window_query.single() {
        if let Ok(mut sprite) = sprite_query.single_mut() {
            sprite.custom_size = Some(Vec2::new(window.width(), window.height()));
        }
    }
}

pub fn adjust_camera_distance(
    mut scroll_events: MessageReader<MouseWheel>,
    mut settings: ResMut<UserSettings>,
) {
    // Only adjust distance when in third person view
    if !settings.third_person_view {
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
