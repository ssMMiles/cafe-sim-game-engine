use bevy::{
    asset::RenderAssetUsages, pbr::MaterialExtension, prelude::*,
    render::render_resource::AsBindGroup, shader::ShaderRef,
};

/// A 1×1 white R8Unorm image handle used as the default (no bake) lightmap.
///
/// Inserted as a resource by `TerrainPlugin::build` so every chunk can
/// reference it before any bake has been run. A single white texel multiplies
/// the lighting by 1.0, giving identical behaviour to having no lightmap.
#[derive(Resource, Clone)]
pub struct DefaultLightmap(pub Handle<Image>);

/// Create a 1×1 R8Unorm white image for use as a default (identity) lightmap.
pub fn create_default_lightmap(images: &mut Assets<Image>) -> Handle<Image> {
    let image = Image::new(
        bevy::render::render_resource::Extent3d {
            width: 1,
            height: 1,
            depth_or_array_layers: 1,
        },
        bevy::render::render_resource::TextureDimension::D2,
        vec![255u8],
        bevy::render::render_resource::TextureFormat::R8Unorm,
        RenderAssetUsages::RENDER_WORLD,
    );
    images.add(image)
}

#[derive(Asset, TypePath, AsBindGroup, Debug, Clone)]
pub struct TerrainMaterial {
    #[texture(100)]
    #[sampler(101)]
    pub stone_texture: Handle<Image>,

    /// Per-chunk baked lightmap (shadow + AO). Defaults to a 1×1 white texel
    /// so the shader always has a valid binding before a bake is run.
    #[texture(102)]
    #[sampler(103)]
    pub lightmap: Handle<Image>,
}

impl MaterialExtension for TerrainMaterial {
    fn vertex_shader() -> ShaderRef {
        "shaders/terrain.wgsl".into()
    }

    fn fragment_shader() -> ShaderRef {
        "shaders/terrain_toon.wgsl".into()
    }

    fn deferred_fragment_shader() -> ShaderRef {
        "shaders/terrain_toon.wgsl".into()
    }

    fn alpha_mode() -> Option<AlphaMode> {
        Some(AlphaMode::Opaque)
    }
}
