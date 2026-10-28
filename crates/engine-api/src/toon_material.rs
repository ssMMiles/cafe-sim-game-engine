use bevy::{
    asset::RenderAssetUsages, pbr::MaterialExtension, prelude::*,
    render::render_resource::AsBindGroup, shader::ShaderRef,
};

/// A 1×1 white `Rgba16Float` image used as the default (unbaked) lightmap for
/// every toon-shaded surface — placed meshes and terrain alike.
///
/// Inserted as a resource by the host's `GfxPlugin` so plugins and scene
/// converters can attach a valid lightmap binding before any bake has run.
/// White means "fully lit": an unbaked surface shows its plain base colour.
#[derive(Resource, Clone)]
pub struct DefaultToonLightmap(pub Handle<Image>);

/// Create the 1×1 white `Rgba16Float` identity lightmap.
pub fn create_default_toon_lightmap(images: &mut Assets<Image>) -> Handle<Image> {
    // f16 1.0 is 0x3C00, little-endian; four channels.
    const ONE: [u8; 2] = [0x00, 0x3C];
    let image = Image::new(
        bevy::render::render_resource::Extent3d {
            width: 1,
            height: 1,
            depth_or_array_layers: 1,
        },
        bevy::render::render_resource::TextureDimension::D2,
        [ONE, ONE, ONE, ONE].concat(),
        bevy::render::render_resource::TextureFormat::Rgba16Float,
        RenderAssetUsages::RENDER_WORLD,
    );
    images.add(image)
}

#[derive(Asset, TypePath, AsBindGroup, Debug, Clone)]
pub struct ToonMaterial {
    /// Per-instance baked HDR lightmap: outgoing radiance in lightmap units
    /// where 1.0 shows the full base colour. All lighting — sun, ambient,
    /// and (soon) point/spot lights — is folded in at bake time; the shader
    /// only samples, bands, and multiplies. Defaults to the shared 1×1 white
    /// texel so the binding is always valid.
    #[texture(100)]
    #[sampler(101)]
    pub lightmap: Handle<Image>,

    /// 1 = shade from the baked lightmap, 0 = flat unlit debug view (Shift+L).
    #[uniform(102)]
    pub use_lightmap: u32,
}

impl Default for ToonMaterial {
    fn default() -> Self {
        Self {
            lightmap: Handle::default(),
            use_lightmap: 1,
        }
    }
}

impl MaterialExtension for ToonMaterial {
    fn fragment_shader() -> ShaderRef {
        "shaders/toon_lighting.wesl".into()
    }

    fn deferred_fragment_shader() -> ShaderRef {
        "shaders/toon_lighting.wesl".into()
    }

    fn alpha_mode() -> Option<AlphaMode> {
        Some(AlphaMode::Opaque)
    }
}
