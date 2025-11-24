use bevy::{
    pbr::MaterialExtension,
    prelude::*,
    render::render_resource::{AsBindGroup, ShaderType},
    shader::ShaderRef,
};

#[derive(Asset, TypePath, AsBindGroup, Debug, Clone)]
pub struct TerrainMaterial {
    #[texture(100)]
    #[sampler(101)]
    pub heightmap: Handle<Image>,

    #[uniform(102)]
    pub config: TerrainConfig,
}

#[derive(ShaderType, Clone, Debug)]
pub struct TerrainConfig {
    pub grid_size: Vec2,   // e.g., [256.0, 256.0]
    pub tile_size: Vec2,   // e.g., [0.5, 0.5]
    pub height_scale: f32, // e.g., 10.0
}

impl MaterialExtension for TerrainMaterial {
    fn vertex_shader() -> ShaderRef {
        "shaders/terrain.wgsl".into()
    }

    fn alpha_mode() -> Option<AlphaMode> {
        Some(AlphaMode::Opaque)
    }
}
