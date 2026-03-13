use bevy::{
    asset::RenderAssetUsages, pbr::MaterialExtension, prelude::*,
    render::render_resource::AsBindGroup, shader::ShaderRef,
};

/// A 1×1 white R8Unorm image handle used as the default (no bake) lightmap
/// for placed mesh instances.
///
/// Inserted as a resource by `GfxPlugin::build` so that `ConvertToToon` can
/// attach a valid lightmap binding before any bake has been run.
#[derive(Resource, Clone)]
pub struct DefaultToonLightmap(pub Handle<Image>);

/// Create a 1×1 R8Unorm white image for use as a default (identity) lightmap.
pub fn create_default_toon_lightmap(images: &mut Assets<Image>) -> Handle<Image> {
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
pub struct ToonMaterial {
    /// Per-instance baked lightmap (shadow + AO). Defaults to a 1×1 white texel
    /// so the shader always has a valid binding before a bake is run.
    #[texture(100)]
    #[sampler(101)]
    pub lightmap: Handle<Image>,
}

impl Default for ToonMaterial {
    fn default() -> Self {
        // This will be replaced by the resource-backed default in ConvertToToon.
        // A default Handle is a weak/empty handle — callers must inject the proper
        // DefaultToonLightmap handle before this material is rendered.
        Self {
            lightmap: Handle::default(),
        }
    }
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
