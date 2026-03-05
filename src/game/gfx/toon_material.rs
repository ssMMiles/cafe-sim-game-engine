use bevy::{
    pbr::MaterialExtension, prelude::*, render::render_resource::AsBindGroup, shader::ShaderRef,
};

// Define the Material Extension
#[derive(Asset, TypePath, AsBindGroup, Debug, Clone)]
pub struct ToonMaterial {
    #[uniform(100)]
    pub color: LinearRgba,
}

impl MaterialExtension for ToonMaterial {
    fn fragment_shader() -> ShaderRef {
        "shaders/toon_lighting.wgsl".into()
    }

    fn deferred_fragment_shader() -> ShaderRef {
        "shaders/toon_lighting.wgsl".into()
    }

    fn alpha_mode() -> Option<AlphaMode> {
        Some(AlphaMode::Opaque)
    }
}
