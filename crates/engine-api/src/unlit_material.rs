//! A translucent, unlit tint: the material for everything that is drawn
//! *over* the world rather than being part of it — the placement ghost, the
//! terrain brush disc, blob shadows, a plugin's preview of a piece before it
//! is placed.
//!
//! Before 2026-11-08 these were `StandardMaterial`s with `unlit: true` and
//! `AlphaMode::Blend`, the last meshes drawn by bevy_pbr. That kept the whole
//! of `PbrPlugin` — clustered lighting, GPU mesh preprocessing, its binned
//! phases — running every frame for four or five translucent discs and ghosts
//! (`docs/SCENE_PASS_PLAN.md`, Phase 5). This is the replacement: our scene
//! pass draws it in `SceneLayer::Transparent` — alpha-blended, depth-tested
//! but not depth-writing, no prepass variant (so outlines and fog ignore it,
//! as they did), after the opaque layer and the sky, back to front.
//!
//! It is in engine-api so plugin dylibs can build one against the same
//! `TypeId` the host renders.

use bevy::{
    prelude::*,
    render::render_resource::{AsBindGroup, ShaderType},
};

/// See the module docs. Rendered by the host's scene pass; the `Material`
/// impl exists only because `MeshMaterial3d<M>` is bounded on it.
#[derive(Asset, TypePath, AsBindGroup, Debug, Clone)]
#[uniform(0, UnlitUniform)]
pub struct UnlitMaterial {
    /// Linear RGBA; the alpha is the opacity, so this is where a ghost's
    /// translucency lives.
    pub base_color: Color,
    /// Multiplied in, sampled through UV0 where the mesh has it. `None`
    /// binds bevy's white fallback, i.e. no texture.
    #[texture(1)]
    #[sampler(2)]
    pub base_color_texture: Option<Handle<Image>>,
}

impl UnlitMaterial {
    pub fn new(base_color: impl Into<Color>) -> Self {
        Self {
            base_color: base_color.into(),
            base_color_texture: None,
        }
    }

    pub fn with_texture(mut self, texture: Handle<Image>) -> Self {
        self.base_color_texture = Some(texture);
        self
    }
}

impl Default for UnlitMaterial {
    fn default() -> Self {
        Self::new(Color::WHITE)
    }
}

/// Mirrors `UnlitSurface` in `unlit.wesl`.
#[derive(Clone, Copy, Debug, ShaderType)]
pub struct UnlitUniform {
    pub base_color: Vec4,
}

impl From<&UnlitMaterial> for UnlitUniform {
    fn from(m: &UnlitMaterial) -> Self {
        Self {
            base_color: LinearRgba::from(m.base_color).to_vec4(),
        }
    }
}

impl Material for UnlitMaterial {}
