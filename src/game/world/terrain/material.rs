use bevy::{
    pbr::MaterialExtension, prelude::*, render::render_resource::AsBindGroup, shader::ShaderRef,
};

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

    /// 1 = shade from the baked lightmap, 0 = flat unlit debug view (Shift+L).
    #[uniform(104)]
    pub use_lightmap: u32,
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
