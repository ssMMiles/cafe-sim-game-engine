use bevy::{
    asset::RenderAssetUsages,
    image::{ImageAddressMode, ImageSampler, ImageSamplerDescriptor},
    math::Affine2,
    mesh::{MeshVertexBufferLayoutRef, UvChannel},
    pbr::{MaterialPipeline, MaterialPipelineKey, MeshPipelineKey},
    prelude::*,
    render::render_resource::{
        AsBindGroup, Extent3d, Face, RenderPipelineDescriptor, ShaderType,
        SpecializedMeshPipelineError, TextureDimension, TextureFormat,
    },
    shader::ShaderRef,
};

use crate::toon_curve::ToonCurve;

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

/// Where a toon surface gets its radiance from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum ToonSource {
    /// A baked lightmap sampled through UV1 — static level geometry.
    Lightmap = 1,
    /// The irradiance probe volume sampled at the fragment's world position
    /// — anything that moves. See `docs/DYNAMIC_LIGHTING_PLAN.md`.
    ProbeVolume = 2,
}

/// The per-material uniform behind the toon shader. Layout mirrors
/// `ToonShading` in `toon_lighting.wesl` field for field.
#[derive(Clone, Copy, Debug, ShaderType)]
pub struct ToonShading {
    /// A [`ToonSource`] discriminant.
    pub source: u32,
    /// 1 = flat unlit debug view (Shift+L). Orthogonal to `source`, so
    /// toggling it never forgets which path the material is on.
    pub unlit: u32,
    /// [`PROBE_CELL_SIZE`], so the shader and the bake agree on the lattice.
    pub probe_cell_size: f32,
    pub _pad: f32,
    /// [`PROBE_VOLUME_DIMS`] as floats (w unused).
    pub probe_dims: Vec4,
    /// Unit direction *toward* the sun (w unused). Probe-lit surfaces
    /// evaluate the sun exactly from the probe's stored visibility; the host
    /// keeps this in step with the level's environment.
    pub sun_dir: Vec4,
    /// Sun colour × intensity in lightmap units (w unused).
    pub sun_rgb: Vec4,
    /// The toon ramp, as `(bands, floor, white, gamma)` — see [`ToonCurve`].
    /// It lives in the uniform rather than in the shader so a world can be
    /// exposed and graded from the inspector without a recompile; the host
    /// keeps it in step with the level's environment for every material.
    pub curve: Vec4,
}

impl ToonShading {
    pub fn new(source: ToonSource) -> Self {
        let d = PROBE_VOLUME_DIMS;
        Self {
            source: source as u32,
            unlit: 0,
            probe_cell_size: PROBE_CELL_SIZE,
            _pad: 0.0,
            probe_dims: Vec4::new(d[0] as f32, d[1] as f32, d[2] as f32, 0.0),
            sun_dir: Vec4::new(0.0, 1.0, 0.0, 0.0),
            sun_rgb: Vec4::ZERO,
            curve: ToonCurve::default().to_vec4(),
        }
    }

    pub fn set_sun(&mut self, dir_to_sun: Vec3, rgb: Vec3) {
        self.sun_dir = dir_to_sun.normalize_or(Vec3::Y).extend(0.0);
        self.sun_rgb = rgb.extend(0.0);
    }
}

/// The four 3D textures every toon material binds for probe-lit shading.
///
/// One set for the whole app, created by the host's `GfxPlugin` and refilled
/// in place as the player moves (`world::probe_volume`): the textures keep
/// their size and format, so bevy rewrites them without touching a single
/// material bind group. Per texel: `c0` holds the constant L1 term and the
/// sun visibility in alpha; `c1`–`c3` hold the `n.x`, `n.y`, `n.z` terms.
#[derive(Resource, Clone)]
pub struct ProbeVolumeImages {
    pub c0: Handle<Image>,
    pub c1: Handle<Image>,
    pub c2: Handle<Image>,
    pub c3: Handle<Image>,
}

impl ProbeVolumeImages {
    pub fn handles(&self) -> [&Handle<Image>; 4] {
        [&self.c0, &self.c1, &self.c2, &self.c3]
    }
}

/// Allocate the probe volume textures, black. [`PROBE_VOLUME_DIMS`]
/// `Rgba16Float`, render-world only — the host never rewrites the `Image`,
/// it writes regions straight into the GPU textures (`world::probe_volume`),
/// so bevy may drop the CPU copy after the first upload — and sampled
/// linearly with **repeat** addressing: the lattice is toroidal, so the
/// shader never wraps coordinates itself.
pub fn create_probe_volume_images(images: &mut Assets<Image>) -> ProbeVolumeImages {
    let [w, h, d] = PROBE_VOLUME_DIMS;
    let mut make = || {
        let mut image = Image::new(
            Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: d,
            },
            TextureDimension::D3,
            vec![0u8; (w * h * d * 8) as usize],
            TextureFormat::Rgba16Float,
            RenderAssetUsages::RENDER_WORLD,
        );
        image.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor {
            address_mode_u: ImageAddressMode::Repeat,
            address_mode_v: ImageAddressMode::Repeat,
            address_mode_w: ImageAddressMode::Repeat,
            ..ImageSamplerDescriptor::linear()
        });
        images.add(image)
    };
    ProbeVolumeImages {
        c0: make(),
        c1: make(),
        c2: make(),
        c3: make(),
    }
}

/// The material every toon-shaded mesh renders with.
///
/// A standalone [`Material`], not an extension of `StandardMaterial`: all
/// lighting is baked, so the only surface properties the shader reads are a
/// base colour, an optional base-colour texture and whether the surface is
/// double-sided. Everything here is plain data and handles — no PBR types —
/// so the bind group and the [`ToonMaterialKey`] are the whole contract a
/// renderer has to honour (`docs/GRAPHICS.md` §5.1).
///
/// Built from a glTF's `StandardMaterial` with [`ToonMaterial::with_surface_of`];
/// whatever that drops is listed by [`dropped_standard_features`].
#[derive(Asset, TypePath, AsBindGroup, Debug, Clone)]
#[uniform(0, ToonSurfaceUniform)]
#[bind_group_data(ToonMaterialKey)]
pub struct ToonMaterial {
    /// Multiplies the base-colour texture (if any) and then the toon-shaded
    /// lighting. Its alpha is written out but the material is always opaque.
    pub base_color: Color,
    /// Sampled through UV0 (and [`Self::uv_transform`]) on meshes that have
    /// UV0; `None` binds bevy's white fallback, i.e. no texture.
    #[texture(1)]
    #[sampler(2)]
    pub base_color_texture: Option<Handle<Image>>,
    /// Applied to UV0 before the base-colour lookup (glTF `KHR_texture_transform`).
    pub uv_transform: Affine2,
    /// No back-face culling, and back faces shade with the normal flipped
    /// towards the viewer — in the colour pass (probe-lit lighting) and in
    /// the normal prepass the edge detector reads. glTF `doubleSided`.
    pub double_sided: bool,

    /// Per-instance baked HDR lightmap: outgoing radiance in lightmap units
    /// where 1.0 shows the full base colour. Sun, ambient×AO, bounce and
    /// every placed light are folded in at bake time; the shader only
    /// samples, bands, and multiplies. Defaults to the shared 1×1 white
    /// texel so the binding is always valid.
    #[texture(100)]
    #[sampler(101)]
    pub lightmap: Handle<Image>,

    #[uniform(102)]
    pub shading: ToonShading,

    /// The shared probe volume (see [`ProbeVolumeImages`]). Bound on every
    /// toon material so a material can switch source without a rebind; the
    /// handles are the same four for the life of the app.
    #[texture(103, dimension = "3d")]
    #[sampler(104)]
    pub probe_c0: Handle<Image>,
    #[texture(105, dimension = "3d")]
    pub probe_c1: Handle<Image>,
    #[texture(106, dimension = "3d")]
    pub probe_c2: Handle<Image>,
    #[texture(107, dimension = "3d")]
    pub probe_c3: Handle<Image>,
}

/// The surface half of the material, as the shader sees it. Mirrors
/// `ToonSurface` in `toon_lighting.wesl`.
#[derive(Clone, Copy, Debug, ShaderType)]
pub struct ToonSurfaceUniform {
    /// Linear RGBA.
    pub base_color: Vec4,
    pub uv_transform: Mat3,
}

impl From<&ToonMaterial> for ToonSurfaceUniform {
    fn from(m: &ToonMaterial) -> Self {
        Self {
            base_color: LinearRgba::from(m.base_color).to_vec4(),
            uv_transform: m.uv_transform.into(),
        }
    }
}

/// What makes two toon materials need different pipelines. Only culling —
/// and the matching normal flip, which is the same switch.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ToonMaterialKey {
    pub double_sided: bool,
}

impl From<&ToonMaterial> for ToonMaterialKey {
    fn from(m: &ToonMaterial) -> Self {
        Self {
            double_sided: m.double_sided,
        }
    }
}

/// Shader def set on both passes of a double-sided toon pipeline.
pub const TOON_DOUBLE_SIDED_DEF: &str = "TOON_DOUBLE_SIDED";

impl ToonMaterial {
    /// A white, single-sided material shading from `source`. `lightmap` is
    /// only sampled for [`ToonSource::Lightmap`]; pass the shared default for
    /// anything else. The sun fields start empty — the host fills them for
    /// every toon material from the level's environment.
    pub fn new(source: ToonSource, lightmap: Handle<Image>, probes: &ProbeVolumeImages) -> Self {
        Self {
            lightmap,
            shading: ToonShading::new(source),
            probe_c0: probes.c0.clone(),
            probe_c1: probes.c1.clone(),
            probe_c2: probes.c2.clone(),
            probe_c3: probes.c3.clone(),
            ..default()
        }
    }

    pub fn with_base_color(mut self, color: impl Into<Color>) -> Self {
        self.base_color = color.into();
        self
    }

    /// Take the surface — colour, texture, UV transform, sidedness — from a
    /// `StandardMaterial` (what glTF scenes spawn with). Everything else it
    /// carries is ignored; [`dropped_standard_features`] says what that loses.
    pub fn with_surface_of(mut self, standard: &StandardMaterial) -> Self {
        self.base_color = standard.base_color;
        self.base_color_texture = standard.base_color_texture.clone();
        self.uv_transform = standard.uv_transform;
        // `cull_mode` rather than `double_sided`: it is what decides whether
        // back faces are visible, and the glTF loader sets both together.
        self.double_sided = standard.cull_mode.is_none();
        self
    }
}

/// The `StandardMaterial` features a toon conversion throws away that would
/// visibly change the result — not the PBR parameters (metallic, roughness,
/// reflectance) that have no meaning under baked toon lighting.
pub fn dropped_standard_features(standard: &StandardMaterial) -> Vec<&'static str> {
    let mut dropped = Vec::new();
    if standard.normal_map_texture.is_some() {
        dropped.push("normal map");
    }
    if standard.emissive != LinearRgba::BLACK || standard.emissive_texture.is_some() {
        dropped.push("emissive");
    }
    if !matches!(standard.alpha_mode, AlphaMode::Opaque) {
        dropped.push("transparency (toon is always opaque)");
    }
    if standard.unlit {
        dropped.push("unlit");
    }
    if standard.base_color_channel != UvChannel::Uv0 {
        dropped.push("base colour on UV1 (toon reads UV0)");
    }
    dropped
}

impl Default for ToonMaterial {
    fn default() -> Self {
        Self {
            base_color: Color::WHITE,
            base_color_texture: None,
            uv_transform: Affine2::IDENTITY,
            double_sided: false,
            lightmap: Handle::default(),
            shading: ToonShading::new(ToonSource::Lightmap),
            probe_c0: Handle::default(),
            probe_c1: Handle::default(),
            probe_c2: Handle::default(),
            probe_c3: Handle::default(),
        }
    }
}

impl Material for ToonMaterial {
    fn fragment_shader() -> ShaderRef {
        "shaders/toon_lighting.wesl".into()
    }

    /// Writes the prepass normal with the double-sided flip; bevy's default
    /// prepass fragment would hand the edge detector inverted normals on the
    /// back of every leaf.
    fn prepass_fragment_shader() -> ShaderRef {
        "shaders/toon_prepass.wesl".into()
    }

    /// Nothing casts a real-time shadow: all lighting is baked and
    /// `BakedSun` has shadows off. Saying so skips the shadow specialisation.
    fn enable_shadows() -> bool {
        false
    }

    /// Runs for the colour pass and the prepass alike.
    fn specialize(
        _pipeline: &MaterialPipeline,
        descriptor: &mut RenderPipelineDescriptor,
        _layout: &MeshVertexBufferLayoutRef,
        key: MaterialPipelineKey<Self>,
    ) -> Result<(), SpecializedMeshPipelineError> {
        let double_sided = key.bind_group_data.double_sided;
        // Mirrors bevy's own rule: a view with INVERT_CULLING (mirrors)
        // culls the opposite face.
        descriptor.primitive.cull_mode = match (
            double_sided,
            key.mesh_key.contains(MeshPipelineKey::INVERT_CULLING),
        ) {
            (true, _) => None,
            (false, false) => Some(Face::Back),
            (false, true) => Some(Face::Front),
        };
        if double_sided && let Some(fragment) = descriptor.fragment.as_mut() {
            fragment.shader_defs.push(TOON_DOUBLE_SIDED_DEF.into());
        }
        Ok(())
    }
}

// ============================================================================
// Probe volume geometry
// ============================================================================

/// Side of one irradiance-probe lattice cell, in metres, on every axis. The
/// lattice is world-aligned: cell `i` spans `[i·c, (i+1)·c)` and its probe
/// sits at the centre. Bakes place probes on it and the runtime volume
/// texture is indexed by it, so this is the one number both must agree on.
///
/// Was 2 m until 2026-11-02. A lamp 2.5 m over a 1.1 m tabletop lights a pool
/// only a couple of metres across — one cell — and no reconstruction can put
/// back detail that was never sampled (`docs/LIGHTING.md` §2.3). Halving the
/// cell costs 8× in every per-cell quantity: probes baked, bytes per chunk
/// file, the `ProbeStore`'s memory, and the volume textures for the same
/// coverage. [`PROBE_VOLUME_DIMS`] was *not* scaled by the full 8× — see it.
/// Changing this changes what a stored cell index means, so it bumps
/// `PROBE_FORMAT_VERSION`.
pub const PROBE_CELL_SIZE: f32 = 1.0;

/// Cells per axis `[x, y, z]` of the runtime probe volume textures. Addressed
/// toroidally — cell `i` lives in texel `i mod dims` — so the window follows
/// the player without any origin uniform or material rebind.
///
/// At [`PROBE_CELL_SIZE`] 1 this is 192 m × 48 m × 192 m: ±96 m around the
/// focus horizontally (it was ±128 m at the 2 m cell) and the same 48 m
/// vertically. Four `Rgba16Float` textures of this size are 56.6 MB, and the
/// CPU keeps two more copies (the volume's own buffers and the `Image`
/// data), so ~170 MB in all; keeping the old ±128 m would have been 100 MB
/// per copy. Anything probe-lit outside the window samples wrapped texels
/// from the far side, so this is the radius dynamic objects stay correct to.
pub const PROBE_VOLUME_DIMS: [u32; 3] = [192, 48, 192];

/// Lattice index of the cell containing world coordinate `w` on one axis.
#[inline]
pub fn probe_cell_index(w: f32) -> i32 {
    (w / PROBE_CELL_SIZE).floor() as i32
}

/// World coordinate of the centre of lattice cell `i` on one axis.
#[inline]
pub fn probe_cell_centre(i: i32) -> f32 {
    (i as f32 + 0.5) * PROBE_CELL_SIZE
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What a glTF `doubleSided` material looks like after bevy's loader.
    fn gltf_double_sided() -> StandardMaterial {
        StandardMaterial {
            base_color: Color::srgb(0.2, 0.4, 0.6),
            base_color_texture: Some(Handle::default()),
            uv_transform: Affine2::from_scale(Vec2::splat(2.0)),
            double_sided: true,
            cull_mode: None,
            ..default()
        }
    }

    #[test]
    fn the_surface_survives_conversion() {
        let standard = gltf_double_sided();
        let toon = ToonMaterial::default().with_surface_of(&standard);
        assert_eq!(toon.base_color, standard.base_color);
        assert_eq!(toon.base_color_texture, standard.base_color_texture);
        assert_eq!(toon.uv_transform, standard.uv_transform);
        assert!(toon.double_sided);
        assert!(ToonMaterialKey::from(&toon).double_sided);

        // The default StandardMaterial culls back faces: single-sided.
        let single = ToonMaterial::default().with_surface_of(&StandardMaterial::default());
        assert!(!single.double_sided);
    }

    /// Conversion keeps the lighting the template chose.
    #[test]
    fn conversion_leaves_the_lighting_alone() {
        let template = ToonMaterial {
            shading: ToonShading::new(ToonSource::ProbeVolume),
            ..default()
        };
        let toon = template.clone().with_surface_of(&gltf_double_sided());
        assert_eq!(toon.shading.source, ToonSource::ProbeVolume as u32);
    }

    /// The shader multiplies in linear space, like StandardMaterial did.
    #[test]
    fn the_uniform_carries_linear_colour() {
        let toon = ToonMaterial::default().with_base_color(Color::srgb(0.5, 0.5, 0.5));
        let uniform = ToonSurfaceUniform::from(&toon);
        let expected = LinearRgba::from(Color::srgb(0.5, 0.5, 0.5)).to_vec4();
        assert!((uniform.base_color - expected).abs().max_element() < 1e-6);
        assert!(uniform.base_color.x < 0.25, "sRGB 0.5 is ~0.21 linear");
        assert_eq!(uniform.uv_transform, Mat3::IDENTITY);
    }

    #[test]
    fn only_visible_losses_are_reported() {
        // PBR parameters mean nothing under baked toon lighting: no noise.
        let plain = StandardMaterial {
            metallic: 1.0,
            perceptual_roughness: 0.1,
            ..default()
        };
        assert!(dropped_standard_features(&plain).is_empty());
        assert!(dropped_standard_features(&gltf_double_sided()).is_empty());

        let fancy = StandardMaterial {
            normal_map_texture: Some(Handle::default()),
            emissive: LinearRgba::RED,
            alpha_mode: AlphaMode::Blend,
            ..default()
        };
        let dropped = dropped_standard_features(&fancy);
        assert_eq!(dropped.len(), 3, "{dropped:?}");
    }
}
