//! The editor's view of the world: a fly camera, a free cursor, and the
//! render target sized to the viewport tab.
//!
//! There is still only one 3D camera (`PlayerCamera`, with its HDR target and
//! post-processing stack). In `EditorMode::Editor` this module drives its
//! transform and the gameplay drivers (`camera_follow`, free-cam movement,
//! walking physics) are gated off; in Gameplay nothing here runs, so the
//! player picks up exactly where the camera's yaw/pitch left off.
//!
//! Cursor policy in the editor: free and visible, except while a right-button
//! look-drag is in progress, when it is locked and hidden so the view can turn
//! without the pointer hitting the window edge. The position is restored on
//! release so the pointer lands where the drag began.

use bevy::input::mouse::MouseMotion;
use bevy::render::render_resource::Extent3d;
use bevy::window::{CursorGrabMode, CursorOptions, PrimaryWindow};
use bevy::{log, prelude::*};

use super::input::{ScrollMod, scroll_modifier, scroll_notches};
use super::picking::ViewportInput;
use crate::game::gfx::camera::{
    GameRenderTarget, PlayerCamera, RENDER_TEXTURE_HEIGHT, RENDER_TEXTURE_WIDTH,
    RenderTextureSprite,
};
use crate::settings::UserSettings;
use engine_api::Player;

/// Where the editor camera starts relative to the player when a world opens:
/// behind and above, looking down at the spawn point.
const INITIAL_OFFSET: Vec3 = Vec3::new(0.0, 6.0, 10.0);
/// Fly speed bounds, metres per second.
const FLY_SPEED_RANGE: (f32, f32) = (0.5, 200.0);
/// Scroll notches → fly speed multiplier.
const FLY_SPEED_STEP: f32 = 1.2;
/// Dolly distance per scroll notch while hovering idle, in metres.
const DOLLY_STEP: f32 = 1.5;
/// Passes the tab rect must hold still before the render target is resized,
/// so dragging a splitter doesn't reallocate the texture every frame.
const RESIZE_SETTLE_PASSES: u8 = 2;

/// Fly mode without holding the right button: toggled by the viewport's
/// overlay button, cleared by Escape (`tool::escape_cancels`, first in its
/// back-out order) and on leaving the editor. While set, the viewport tab
/// reports `look_active`, so the cursor lock, the fly camera, and
/// `pointer_free()` all behave exactly as during a right-drag look.
#[derive(Resource, Default, Debug)]
pub struct FlyLock(pub bool);

/// `OnExit(EditorMode::Editor)`: a stale lock must not re-grab the cursor the
/// moment the editor comes back.
pub fn release_fly_lock(mut lock: ResMut<FlyLock>) {
    lock.0 = false;
}

#[derive(Resource, Debug)]
pub struct EditorCameraSettings {
    /// Fly speed in m/s while looking (Shift multiplies by 4).
    pub fly_speed: f32,
}

impl Default for EditorCameraSettings {
    fn default() -> Self {
        Self { fly_speed: 12.0 }
    }
}

/// Place the camera sensibly the moment it exists. `Added` rather than an
/// `OnEnter` hook because the camera is spawned by a command in the game's
/// own `OnEnter(InGame)` system and may not exist yet when ours run.
pub fn init_editor_camera(
    mut camera: Query<(&mut Transform, &mut PlayerCamera), Added<PlayerCamera>>,
    player: Query<&Transform, (With<Player>, Without<PlayerCamera>)>,
) {
    let Ok((mut transform, mut cam)) = camera.single_mut() else {
        return;
    };
    let target = player.single().map(|t| t.translation).unwrap_or(Vec3::ZERO);
    place_camera_at_spawn(&mut transform, &mut cam, target);
}

/// Behind and above `target`, looking at it. Also used after a world switch.
pub fn place_camera_at_spawn(transform: &mut Transform, cam: &mut PlayerCamera, target: Vec3) {
    transform.translation = target + INITIAL_OFFSET;
    transform.look_at(target, Vec3::Y);
    let (yaw, pitch, _) = transform.rotation.to_euler(EulerRot::YXZ);
    cam.yaw = yaw;
    cam.pitch = pitch;
}

/// Right-drag look + WASD/QE fly, and the wheel: speed while looking, dolly
/// while hovering idle.
#[allow(clippy::too_many_arguments)]
pub fn editor_fly_camera(
    time: Res<Time>,
    keys: Res<ButtonInput<KeyCode>>,
    mut motion: MessageReader<MouseMotion>,
    viewport: Res<ViewportInput>,
    settings: Res<UserSettings>,
    mut cam_settings: ResMut<EditorCameraSettings>,
    mut camera: Query<(&mut Transform, &mut PlayerCamera)>,
) {
    // Always drain motion so a burst doesn't snap the view when a look starts.
    let mut delta = Vec2::ZERO;
    for m in motion.read() {
        delta += m.delta;
    }
    let Ok((mut transform, mut cam)) = camera.single_mut() else {
        return;
    };

    if viewport.look_active {
        if delta != Vec2::ZERO {
            cam.yaw -= delta.x * settings.mouse_sensitivity;
            cam.pitch = (cam.pitch - delta.y * settings.mouse_sensitivity).clamp(
                -std::f32::consts::FRAC_PI_2 + 0.01,
                std::f32::consts::FRAC_PI_2 - 0.01,
            );
            transform.rotation = Quat::from_euler(EulerRot::YXZ, cam.yaw, cam.pitch, 0.0);
        }

        let notches = scroll_notches(&viewport);
        if notches != 0.0 {
            cam_settings.fly_speed = (cam_settings.fly_speed * FLY_SPEED_STEP.powf(notches))
                .clamp(FLY_SPEED_RANGE.0, FLY_SPEED_RANGE.1);
        }

        let mut wish = Vec3::ZERO;
        if keys.pressed(KeyCode::KeyW) {
            wish += transform.forward().as_vec3();
        }
        if keys.pressed(KeyCode::KeyS) {
            wish -= transform.forward().as_vec3();
        }
        if keys.pressed(KeyCode::KeyD) {
            wish += transform.right().as_vec3();
        }
        if keys.pressed(KeyCode::KeyA) {
            wish -= transform.right().as_vec3();
        }
        if keys.pressed(KeyCode::KeyE) {
            wish += Vec3::Y;
        }
        if keys.pressed(KeyCode::KeyQ) {
            wish -= Vec3::Y;
        }
        if wish != Vec3::ZERO {
            let boost = if keys.pressed(KeyCode::ShiftLeft) || keys.pressed(KeyCode::ShiftRight) {
                4.0
            } else {
                1.0
            };
            transform.translation +=
                wish.normalize() * cam_settings.fly_speed * boost * time.delta_secs();
        }
        return;
    }

    // Idle hover: plain wheel dollies, unless a modifier claims the wheel for
    // placement scale/rotation.
    if viewport.hovered && scroll_modifier(&keys) == ScrollMod::None {
        let notches = scroll_notches(&viewport);
        if notches != 0.0 {
            let forward = transform.forward().as_vec3();
            transform.translation += forward * notches * DOLLY_STEP;
        }
    }
}

/// Lock the cursor while a look-drag is held, and put it back afterwards.
pub fn sync_look_cursor(
    viewport: Res<ViewportInput>,
    mut window: Single<&mut Window, With<PrimaryWindow>>,
    mut cursor: Single<&mut CursorOptions>,
    mut saved: Local<Option<Option<Vec2>>>,
) {
    match (viewport.look_active, saved.is_some()) {
        (true, false) => {
            *saved = Some(window.cursor_position());
            cursor.grab_mode = CursorGrabMode::Locked;
            cursor.visible = false;
        }
        (false, true) => {
            let pos = saved.take().flatten();
            cursor.grab_mode = CursorGrabMode::None;
            cursor.visible = true;
            if pos.is_some() {
                window.set_cursor_position(pos);
            }
        }
        _ => {}
    }
}

/// The editor owns a free cursor. Runs on every path that could have left it
/// locked: entering the editor, entering the game in editor mode, and the
/// pause menu's resume buttons (which still lock for gameplay's sake).
pub fn free_cursor_for_editor(cursor: Option<Single<&mut CursorOptions>>) {
    let Some(mut cursor) = cursor else {
        return;
    };
    cursor.grab_mode = CursorGrabMode::None;
    cursor.visible = true;
}

/// Keep the render target the size of the viewport tab (physical pixels) and
/// the full-window sprite hidden while the editor is showing the texture
/// through egui instead.
pub fn sync_render_target(
    viewport: Res<ViewportInput>,
    target: Option<Res<GameRenderTarget>>,
    mut images: ResMut<Assets<Image>>,
    mut sprite: Query<&mut Visibility, With<RenderTextureSprite>>,
    mut settle: Local<(UVec2, u8)>,
) {
    for mut vis in sprite.iter_mut() {
        if *vis != Visibility::Hidden {
            *vis = Visibility::Hidden;
        }
    }

    let Some(target) = target else {
        return;
    };
    let wanted = viewport.size_px.max(UVec2::ONE);
    if wanted == UVec2::ONE {
        // No pass has measured the tab yet (first frame, or dock hidden).
        return;
    }
    if settle.0 == wanted {
        settle.1 = settle.1.saturating_add(1);
    } else {
        *settle = (wanted, 0);
    }
    if settle.1 < RESIZE_SETTLE_PASSES {
        return;
    }

    // Compare through `get` first: `get_mut` flags the asset modified and
    // would re-upload the whole texture every frame.
    if images
        .get(&target.0)
        .is_some_and(|img| img.size() != wanted)
        && let Some(mut image) = images.get_mut(&target.0)
    {
        log::debug!("Viewport render target -> {}x{}", wanted.x, wanted.y);
        image.resize(Extent3d {
            width: wanted.x,
            height: wanted.y,
            ..default()
        });
    }
}

/// `OnExit(EditorMode::Editor)`: gameplay presents the fixed-size texture via
/// the full-window sprite again.
pub fn restore_render_target(
    target: Option<Res<GameRenderTarget>>,
    mut images: ResMut<Assets<Image>>,
    mut sprite: Query<&mut Visibility, With<RenderTextureSprite>>,
) {
    for mut vis in sprite.iter_mut() {
        *vis = Visibility::Inherited;
    }
    let Some(target) = target else {
        return;
    };
    let gameplay_size = UVec2::new(RENDER_TEXTURE_WIDTH, RENDER_TEXTURE_HEIGHT);
    if images
        .get(&target.0)
        .is_some_and(|img| img.size() != gameplay_size)
        && let Some(mut image) = images.get_mut(&target.0)
    {
        image.resize(Extent3d {
            width: RENDER_TEXTURE_WIDTH,
            height: RENDER_TEXTURE_HEIGHT,
            ..default()
        });
    }
}
