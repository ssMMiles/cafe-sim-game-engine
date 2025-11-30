use bevy::{
    pbr::MaterialExtension,
    prelude::*,
    render::render_resource::{AsBindGroup, ShaderType},
    shader::ShaderRef,
};

#[derive(Asset, TypePath, AsBindGroup, Debug, Clone)]
pub struct TerrainMaterial {
    // Note: Material is now minimal since vertex data comes from mesh attributes
    // Keeping config for potential future use (e.g., fragment shader effects)
    #[uniform(100)]
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

    fn prepass_vertex_shader() -> ShaderRef {
        "shaders/terrain.wgsl".into()
    }

    fn prepass_fragment_shader() -> ShaderRef {
        "shaders/terrain.wgsl".into() // Custom prepass fragment without normal requirements
    }                                                                                                                           

    fn alpha_mode() -> Option<AlphaMode> {
        Some(AlphaMode::Opaque)
    }
}
