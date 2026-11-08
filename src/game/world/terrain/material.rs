use bevy::{
    prelude::*,
    render::render_resource::{AsBindGroup, ShaderType},
};

/// Terrain's per-material uniform. Mirrors `TerrainShading` in
/// `terrain_toon.wesl`.
///
/// The toon curve used to be here too (and in `ToonShading`), written by
/// `sync_toon_look` into every material; it is per scene, not per material,
/// and travels in the scene pass's uniform now (`scene_pass::SceneUniform`),
/// so terrain and the objects standing on it read literally the same ramp.
#[derive(Clone, Copy, Debug, ShaderType)]
pub struct TerrainShading {
    /// 1 = shade from the baked lightmap, 0 = flat unlit debug view (Shift+L).
    pub use_lightmap: u32,
    pub _pad: Vec3,
    /// Ground colour where the stone texture doesn't darken it, linear RGBA.
    pub base_color: Vec4,
}

impl Default for TerrainShading {
    fn default() -> Self {
        Self {
            use_lightmap: 1,
            _pad: Vec3::ZERO,
            base_color: Vec4::ONE,
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

/// Terrain is **not** drawn by `MaterialPlugin` any more: `gfx::scene_pass`
/// owns its extraction, pipelines and passes (docs/SCENE_PASS_PLAN.md,
/// Phase 1). This impl survives only because `MeshMaterial3d<M>` — the
/// authoring component, which deliberately did not change — is bounded on
/// `M: Material`. Nothing reads it.
///
/// The shaders live in `assets/shaders/scene/terrain_vertex.wesl`,
/// `assets/shaders/scene/terrain_prepass.wesl` and
/// `assets/shaders/terrain_toon.wesl`, and are named by
/// `scene_pass::pipeline::init_terrain_scene_pipeline`. They are *not* usable
/// through bevy's mesh pipeline: they read our bind group 0, not
/// `mesh_view` + `mesh`. Naming them here would be a trap, so the defaults
/// stand.
impl Material for TerrainMaterial {
    /// See `ToonMaterial::enable_shadows`: nothing casts real-time shadows.
    fn enable_shadows() -> bool {
        false
    }
}
