//! Pipelines for the scene pass: one specializer per material type, keyed on
//! the pass it feeds, the view's MSAA and colour format, and whatever the
//! material's own key adds (double-sidedness for toon). The mesh's vertex
//! layout is the other half of every key: `SpecializedMeshPipelines` (plain
//! bevy_render, no bevy_pbr) caches on `(layout, key)` and hands the layout to
//! `specialize`, which is where the vertex-buffer layout — and which `VERTEX_*`
//! / `SKINNED` defs are set — comes from.
//!
//! The depth state, the prepass attachment list and the reverse-Z compare
//! function all have to match bevy's, because bevy's own passes share the same
//! textures for the stragglers: `CORE_3D_DEPTH_FORMAT`, `GreaterEqual`, and
//! the four-slot prepass target list from `prepass_target_descriptors`
//! (normal, motion vectors, deferred, deferred id) with only the first filled.

use bevy::{
    asset::AssetServer,
    core_pipeline::{core_3d::CORE_3D_DEPTH_FORMAT, prepass::prepass_target_descriptors},
    mesh::VertexBufferLayout,
    prelude::*,
    render::{
        mesh::MeshVertexBufferLayoutRef,
        render_resource::{
            BindGroupLayoutDescriptor, ColorTargetState, ColorWrites, CompareFunction,
            DepthBiasState, DepthStencilState, Face, FragmentState, MultisampleState,
            PrimitiveState, PrimitiveTopology, RenderPipelineDescriptor, SpecializedMeshPipeline,
            SpecializedMeshPipelineError, StencilState, TextureFormat, VertexState,
        },
    },
    shader::{Shader, ShaderDefVal},
};
use engine_api::{TOON_DOUBLE_SIDED_DEF, ToonMaterial, ToonMaterialKey};

use crate::game::world::{sky::SkyDomeMaterial, terrain::material::TerrainMaterial};

use super::{
    SceneBindGroupLayout, SceneLayer,
    material::{MaterialLayout, ScenePipeline},
};

/// Which of the two passes a pipeline feeds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ScenePassKind {
    /// Depth + the world-normal target the edge detector reads.
    Prepass,
    /// The view's HDR colour target.
    Colour,
}

/// Everything about a *view* that changes a pipeline.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SceneViewKey {
    pub msaa_samples: u32,
    /// The view target's colour format. Ignored by the prepass variant, whose
    /// format is fixed by `prepass_target_descriptors`.
    pub target_format: TextureFormat,
}

/// Bind group 1 is the material's, so the fragment shaders' existing
/// `@group(constants::MATERIAL_BIND_GROUP)` lines need no edit — bevy set the
/// same def to 3 for its own mesh layout, we set it to 1 for ours.
const MATERIAL_BIND_GROUP: u32 = 1;

/// The shader handles and layouts a specializer needs, loaded once.
struct SceneShaders {
    scene_layout: BindGroupLayoutDescriptor,
    material_layout: BindGroupLayoutDescriptor,
    vertex: Handle<Shader>,
    colour: Handle<Shader>,
    prepass: Handle<Shader>,
}

impl SceneShaders {
    fn load<M: bevy::render::render_resource::AsBindGroup + Asset>(
        world: &World,
        vertex: &'static str,
        colour: &'static str,
        prepass: &'static str,
    ) -> Self {
        let assets = world.resource::<AssetServer>();
        Self {
            scene_layout: world.resource::<SceneBindGroupLayout>().0.clone(),
            material_layout: world.resource::<MaterialLayout<M>>().0.clone(),
            vertex: assets.load(vertex),
            colour: assets.load(colour),
            prepass: assets.load(prepass),
        }
    }

    /// The whole descriptor given the per-material pieces: the vertex layout
    /// the mesh supports, the defs both stages share, the cull mode, and the
    /// layer (which decides depth *write* — a background material tests depth
    /// but must not add to it).
    #[expect(clippy::too_many_arguments, reason = "pipeline descriptor pieces")]
    fn descriptor(
        &self,
        label: &'static str,
        pass: ScenePassKind,
        view: SceneViewKey,
        layer: SceneLayer,
        vertex_layout: VertexBufferLayout,
        mut shader_defs: Vec<ShaderDefVal>,
        cull_mode: Option<Face>,
    ) -> RenderPipelineDescriptor {
        shader_defs.push(ShaderDefVal::UInt(
            "MATERIAL_BIND_GROUP".into(),
            MATERIAL_BIND_GROUP,
        ));
        let (fragment_shader, targets) = match pass {
            // Normal only. The list must stay four long and in this order:
            // bevy's prepass node hands the render pass four attachment slots
            // whenever a prepass runs at all.
            ScenePassKind::Prepass => (
                self.prepass.clone(),
                prepass_target_descriptors(true, false, false),
            ),
            ScenePassKind::Colour => (
                self.colour.clone(),
                vec![Some(ColorTargetState {
                    format: view.target_format,
                    blend: None,
                    write_mask: ColorWrites::ALL,
                })],
            ),
        };
        RenderPipelineDescriptor {
            label: Some(label.into()),
            layout: vec![self.scene_layout.clone(), self.material_layout.clone()],
            vertex: VertexState {
                shader: self.vertex.clone(),
                shader_defs: shader_defs.clone(),
                buffers: vec![vertex_layout],
                ..default()
            },
            fragment: Some(FragmentState {
                shader: fragment_shader,
                shader_defs,
                targets,
                ..default()
            }),
            primitive: PrimitiveState {
                // Every mesh this pass draws is a triangle list (terrain/mesh.rs
                // builds one; glTF primitives default to one), so the topology
                // is not in the key.
                topology: PrimitiveTopology::TriangleList,
                // The *same* cull mode in both passes. Bevy's prepass leaves
                // culling off while its opaque pass turns it on, which lets a
                // back face win the depth buffer on a pixel the colour pass
                // then discards — a hole with fog in it. Keeping the two passes
                // identical is the whole point of having a prepass.
                cull_mode,
                ..default()
            },
            depth_stencil: Some(DepthStencilState {
                format: CORE_3D_DEPTH_FORMAT,
                depth_write_enabled: Some(layer == SceneLayer::Opaque),
                // Reverse-Z, as everywhere else in the 3D view.
                depth_compare: Some(CompareFunction::GreaterEqual),
                stencil: StencilState::default(),
                bias: DepthBiasState::default(),
            }),
            multisample: MultisampleState {
                count: view.msaa_samples,
                mask: !0,
                alpha_to_coverage_enabled: false,
            },
            ..default()
        }
    }
}

// ---------------------------------------------------------------------------
// Terrain
// ---------------------------------------------------------------------------

#[derive(Resource)]
pub struct TerrainScenePipeline(SceneShaders);

impl FromWorld for TerrainScenePipeline {
    fn from_world(world: &mut World) -> Self {
        Self(SceneShaders::load::<TerrainMaterial>(
            world,
            "shaders/scene/terrain_vertex.wesl",
            "shaders/terrain_toon.wesl",
            "shaders/scene/terrain_prepass.wesl",
        ))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TerrainPipelineKey {
    pub pass: ScenePassKind,
    pub view: SceneViewKey,
}

impl ScenePipeline<TerrainMaterial> for TerrainScenePipeline {
    fn key(view: SceneViewKey, pass: ScenePassKind, _: &()) -> Self::Key {
        TerrainPipelineKey { pass, view }
    }
}

impl SpecializedMeshPipeline for TerrainScenePipeline {
    type Key = TerrainPipelineKey;

    fn specialize(
        &self,
        key: Self::Key,
        layout: &MeshVertexBufferLayoutRef,
    ) -> Result<RenderPipelineDescriptor, SpecializedMeshPipelineError> {
        // Only what the shaders read: UV0 exists in the chunk mesh and is all
        // zeros, so it is deliberately not bound.
        let vertex_layout = layout.0.get_layout(&[
            Mesh::ATTRIBUTE_POSITION.at_shader_location(0),
            Mesh::ATTRIBUTE_NORMAL.at_shader_location(1),
            Mesh::ATTRIBUTE_UV_1.at_shader_location(2),
        ])?;
        let label = match key.pass {
            ScenePassKind::Prepass => "terrain_scene_prepass_pipeline",
            ScenePassKind::Colour => "terrain_scene_colour_pipeline",
        };
        // Terrain is single-sided; back faces culled in both passes.
        Ok(self.0.descriptor(
            label,
            key.pass,
            key.view,
            SceneLayer::Opaque,
            vertex_layout,
            Vec::new(),
            Some(Face::Back),
        ))
    }
}

// ---------------------------------------------------------------------------
// Toon
// ---------------------------------------------------------------------------

#[derive(Resource)]
pub struct ToonScenePipeline(SceneShaders);

impl FromWorld for ToonScenePipeline {
    fn from_world(world: &mut World) -> Self {
        Self(SceneShaders::load::<ToonMaterial>(
            world,
            "shaders/scene/mesh_vertex.wesl",
            "shaders/toon_lighting.wesl",
            "shaders/toon_prepass.wesl",
        ))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ToonPipelineKey {
    pub pass: ScenePassKind,
    pub view: SceneViewKey,
    pub material: ToonMaterialKey,
}

impl ScenePipeline<ToonMaterial> for ToonScenePipeline {
    fn key(view: SceneViewKey, pass: ScenePassKind, material: &ToonMaterialKey) -> Self::Key {
        ToonPipelineKey {
            pass,
            view,
            material: *material,
        }
    }
}

impl SpecializedMeshPipeline for ToonScenePipeline {
    type Key = ToonPipelineKey;

    /// The vertex layout and the `VERTEX_UVS_A` / `VERTEX_UVS_B` / `SKINNED`
    /// defs come from what the mesh actually carries — the same rule bevy's
    /// mesh pipeline applied, under the same def names, so the toon fragment
    /// shaders' existing `@if`s are untouched. Shader locations are fixed by
    /// `scene/toon_vertex.wesl`'s `Vertex` struct.
    fn specialize(
        &self,
        key: Self::Key,
        layout: &MeshVertexBufferLayoutRef,
    ) -> Result<RenderPipelineDescriptor, SpecializedMeshPipelineError> {
        let mut attributes = vec![
            Mesh::ATTRIBUTE_POSITION.at_shader_location(0),
            Mesh::ATTRIBUTE_NORMAL.at_shader_location(1),
        ];
        let mut shader_defs = Vec::new();
        if layout.0.contains(Mesh::ATTRIBUTE_UV_0) {
            attributes.push(Mesh::ATTRIBUTE_UV_0.at_shader_location(2));
            shader_defs.push("VERTEX_UVS_A".into());
        }
        if layout.0.contains(Mesh::ATTRIBUTE_UV_1) {
            attributes.push(Mesh::ATTRIBUTE_UV_1.at_shader_location(3));
            shader_defs.push("VERTEX_UVS_B".into());
        }
        if layout.0.contains(Mesh::ATTRIBUTE_JOINT_INDEX)
            && layout.0.contains(Mesh::ATTRIBUTE_JOINT_WEIGHT)
        {
            attributes.push(Mesh::ATTRIBUTE_JOINT_INDEX.at_shader_location(4));
            attributes.push(Mesh::ATTRIBUTE_JOINT_WEIGHT.at_shader_location(5));
            shader_defs.push("SKINNED".into());
        }
        let vertex_layout = layout.0.get_layout(&attributes)?;

        // Double-sided: no culling, and both fragment shaders flip the normal
        // on back faces (`TOON_DOUBLE_SIDED` in `facing_normal`).
        let cull_mode = if key.material.double_sided {
            shader_defs.push(TOON_DOUBLE_SIDED_DEF.into());
            None
        } else {
            Some(Face::Back)
        };
        let label = match key.pass {
            ScenePassKind::Prepass => "toon_scene_prepass_pipeline",
            ScenePassKind::Colour => "toon_scene_colour_pipeline",
        };
        Ok(self.0.descriptor(
            label,
            key.pass,
            key.view,
            SceneLayer::Opaque,
            vertex_layout,
            shader_defs,
            cull_mode,
        ))
    }
}

// ---------------------------------------------------------------------------
// Sky dome
// ---------------------------------------------------------------------------

#[derive(Resource)]
pub struct SkyScenePipeline(SceneShaders);

impl FromWorld for SkyScenePipeline {
    fn from_world(world: &mut World) -> Self {
        Self(SceneShaders::load::<SkyDomeMaterial>(
            world,
            "shaders/scene/mesh_vertex.wesl",
            "shaders/sky_dome.wesl",
            // Never used: the sky has no prepass variant
            // (`SceneLayer::Background`). `SceneShaders` holds one handle per
            // stage, so this names the colour shader rather than inventing a
            // prepass the pass would refuse to draw.
            "shaders/sky_dome.wesl",
        ))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SkyPipelineKey {
    pub view: SceneViewKey,
}

impl ScenePipeline<SkyDomeMaterial> for SkyScenePipeline {
    fn key(view: SceneViewKey, _pass: ScenePassKind, _: &()) -> Self::Key {
        SkyPipelineKey { view }
    }
}

impl SpecializedMeshPipeline for SkyScenePipeline {
    type Key = SkyPipelineKey;

    /// One variant. The dome is a plain UV sphere and the shader reads only
    /// its radial normal — which is the view direction, since the dome is
    /// centred on the camera (`world/sky.rs`).
    fn specialize(
        &self,
        key: Self::Key,
        layout: &MeshVertexBufferLayoutRef,
    ) -> Result<RenderPipelineDescriptor, SpecializedMeshPipelineError> {
        let vertex_layout = layout.0.get_layout(&[
            Mesh::ATTRIBUTE_POSITION.at_shader_location(0),
            Mesh::ATTRIBUTE_NORMAL.at_shader_location(1),
        ])?;
        Ok(self.0.descriptor(
            "sky_scene_pipeline",
            ScenePassKind::Colour,
            key.view,
            SceneLayer::Background,
            vertex_layout,
            Vec::new(),
            // The camera is *inside* the sphere.
            None,
        ))
    }
}
