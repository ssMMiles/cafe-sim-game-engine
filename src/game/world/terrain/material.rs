use bevy::{
    pbr::MaterialExtension,
    prelude::*,
    render::render_resource::{AsBindGroup, ShaderType},
    shader::ShaderRef,
};
use engine_api::toon_curve::ToonCurve;

/// Terrain's half of the toon uniform. Mirrors `TerrainShading` in
/// `terrain_toon.wesl`.
///
/// The curve is duplicated here rather than shared with `ToonShading`
/// because the two materials are separate bind groups, but it must hold the
/// *same* values: terrain and the objects standing on it meet along every
/// contact shadow, and a ramp that differs between them shows up as a step
/// right at the join. `sync_toon_look` writes both from one source.
#[derive(Clone, Copy, Debug, ShaderType)]
pub struct TerrainShading {
    /// 1 = shade from the baked lightmap, 0 = flat unlit debug view (Shift+L).
    pub use_lightmap: u32,
    pub _pad: Vec3,
    /// The toon ramp as `(bands, floor, white, gamma)` — see [`ToonCurve`].
    pub curve: Vec4,
}

impl Default for TerrainShading {
    fn default() -> Self {
        Self {
            use_lightmap: 1,
            _pad: Vec3::ZERO,
            curve: ToonCurve::default().to_vec4(),
        }
    }
}

#[derive(Asset, TypePath, AsBindGroup, Debug, Clone)]
pub struct TerrainMaterial {
    #[texture(100)]
    #[sampler(101)]
    pub stone_texture: Handle<Image>,

    /// Per-chunk baked HDR lightmap: outgoing radiance in lightmap units
    /// (`Rgba16Float`, see `baking::lightmap`). Defaults to the shared 1×1
    /// white texel so an unbaked chunk renders fully lit rather than black.
    #[texture(102)]
    #[sampler(103)]
    pub lightmap: Handle<Image>,

    #[uniform(104)]
    pub shading: TerrainShading,
}

impl MaterialExtension for TerrainMaterial {
    fn vertex_shader() -> ShaderRef {
        "shaders/terrain.wesl".into()
    }

    fn fragment_shader() -> ShaderRef {
        "shaders/terrain_toon.wesl".into()
    }

    fn deferred_fragment_shader() -> ShaderRef {
        "shaders/terrain_toon.wesl".into()
    }

    fn alpha_mode() -> Option<AlphaMode> {
        Some(AlphaMode::Opaque)
    }
}
