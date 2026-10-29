use bevy::{
    asset::RenderAssetUsages,
    image::{ImageAddressMode, ImageSampler, ImageSamplerDescriptor},
    pbr::MaterialExtension,
    prelude::*,
    render::render_resource::{AsBindGroup, Extent3d, ShaderType, TextureDimension, TextureFormat},
    shader::ShaderRef,
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
/// `Rgba16Float`, kept in the main world too so the host can rewrite them,
/// and sampled linearly with **repeat** addressing — the lattice is
/// toroidal, so the shader never wraps coordinates itself.
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
            RenderAssetUsages::MAIN_WORLD | RenderAssetUsages::RENDER_WORLD,
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

#[derive(Asset, TypePath, AsBindGroup, Debug, Clone)]
pub struct ToonMaterial {
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

impl ToonMaterial {
    /// A material shading from `source`. `lightmap` is only sampled for
    /// [`ToonSource::Lightmap`]; pass the shared default for anything else.
    /// The sun fields start empty — the host fills them for every toon
    /// material from the level's environment.
    pub fn new(source: ToonSource, lightmap: Handle<Image>, probes: &ProbeVolumeImages) -> Self {
        Self {
            lightmap,
            shading: ToonShading::new(source),
            probe_c0: probes.c0.clone(),
            probe_c1: probes.c1.clone(),
            probe_c2: probes.c2.clone(),
            probe_c3: probes.c3.clone(),
        }
    }
}

impl Default for ToonMaterial {
    fn default() -> Self {
        Self {
            lightmap: Handle::default(),
            shading: ToonShading::new(ToonSource::Lightmap),
            probe_c0: Handle::default(),
            probe_c1: Handle::default(),
            probe_c2: Handle::default(),
            probe_c3: Handle::default(),
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

// ============================================================================
// Probe volume geometry
// ============================================================================

/// Side of one irradiance-probe lattice cell, in metres, on every axis. The
/// lattice is world-aligned: cell `i` spans `[i·c, (i+1)·c)` and its probe
/// sits at the centre. Bakes place probes on it and the runtime volume
/// texture is indexed by it, so this is the one number both must agree on.
pub const PROBE_CELL_SIZE: f32 = 2.0;

/// Cells per axis `[x, y, z]` of the runtime probe volume textures. Addressed
/// toroidally — cell `i` lives in texel `i mod dims` — so the window follows
/// the player without any origin uniform or material rebind. At
/// [`PROBE_CELL_SIZE`] 2 this is 256 m × 48 m × 256 m; four `Rgba16Float`
/// textures of this size are ~12.6 MB.
pub const PROBE_VOLUME_DIMS: [u32; 3] = [128, 24, 128];

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
