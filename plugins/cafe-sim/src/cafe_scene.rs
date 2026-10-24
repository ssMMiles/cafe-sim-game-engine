//! The cafe shell: a placed level entity this plugin reads.
//!
//! The shell used to be spawned here, by plugin code, every time gameplay
//! started. That stopped working once plugins became inactive in editor mode
//! — the shell would simply be absent while authoring, including from
//! lightmap bakes, where it is the main occluder in the scene.
//!
//! It is a level entity now. Place it from the editor's catalog (Tab →
//! `cafe-sim/cafe-shell`), which attaches a `cafe-shell` script binding; the
//! host hydrates that into a [`CafeShell`] marker, in both modes.
//!
//! **The level owns the shell outright** — geometry, collider, *and*
//! materials. `Cafe.gltf` carries its own default materials (brick walls, a
//! tiled floor), so the engine's `ConvertToToon` dresses it as the scene
//! loads and it looks right in the editor with this plugin stopped.
//!
//! This module used to hold a `dress_cafe_shell` system that assigned those
//! materials at runtime, because the glTF shipped geometry only. That was
//! wrong twice over: the shell stayed white until gameplay started, and a
//! gameplay plugin was writing plugin-private state onto a level entity it
//! does not own — which nothing unwinds on unload. See
//! `docs/PLUGIN_LEVEL_ENTITY_CONTRACT.md`.
//!
//! All that is left is [`sync_cafe_anchor`], which reads the shell's
//! transform into [`CafeAnchor`] so the furniture grid follows wherever the
//! shell was placed. It only ever *reads* the level entity.
//!
//! Re-skinnable wallpaper is still on the table as a gameplay feature — it
//! just has to be a deliberate, revertible override of the asset default
//! rather than the only thing that makes the room look right.

use bevy::{log, prelude::*};

use crate::grid::CafeAnchor;

/// Marker hydrated onto the placed shell entity by the `cafe-shell` script.
#[derive(Component, Debug)]
pub struct CafeShell;

/// Mirror the placed shell's world position into [`CafeAnchor`].
///
/// Runs every frame rather than once: the shell can be grabbed and moved in
/// the editor, and it may stream in after gameplay has already started.
pub fn sync_cafe_anchor(
    mut anchor: ResMut<CafeAnchor>,
    shell_q: Query<&GlobalTransform, With<CafeShell>>,
    mut warned_missing: Local<bool>,
    mut warned_rotation: Local<bool>,
) {
    let Some(gt) = shell_q.iter().next() else {
        if !*warned_missing {
            *warned_missing = true;
            log::warn!(
                "[cafe-sim] no cafe shell placed in this level — falling back to the world \
                 origin. Place one from the editor catalog (Tab → cafe-sim/cafe-shell) and \
                 save with Shift+S."
            );
        }
        return;
    };
    *warned_missing = false;

    let (_, rotation, translation) = gt.to_scale_rotation_translation();

    if !*warned_rotation && rotation.angle_between(Quat::IDENTITY) > 1e-3 {
        *warned_rotation = true;
        log::warn!(
            "[cafe-sim] the placed cafe shell is rotated; the furniture grid ignores rotation \
             and stays axis-aligned. Place it unrotated to keep the grid inside the walls."
        );
    }

    if anchor.origin != translation {
        anchor.origin = translation;
    }
}
