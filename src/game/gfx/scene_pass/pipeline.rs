//! Pipelines for the scene pass: one specializer per material type, keyed on
//! the view (MSAA, colour format, whether it has a normal target) and whatever
//! the material's own key adds (double-sidedness for toon). The mesh's vertex
//! layout is the other half of every key: `SpecializedMeshPipelines` (plain
//! bevy_render, no bevy_pbr) caches on `(layout, key)` and hands the layout to
//! `specialize`, which is where the vertex-buffer layout — and which `VERTEX_*`
//! / `SKINNED` defs are set — comes from.
//!
//! One pass, two colour targets: the view's HDR target and, when the view has
//! a `NormalPrepass`, bevy's `ViewPrepassTextures.normal` as target 1 — the
//! world normal the edge detector reads. There is no separate prepass any
//! more (folded 2026-11-08: the scene was being rasterised twice, and on a
//! tile-based GPU the colour pass already shades nothing hidden). The depth
//! state, the normal format and the reverse-Z compare function have to match
//! bevy's, because bevy's own passes still open the same textures:
//! `CORE_3D_DEPTH_FORMAT`, `NORMAL_PREPASS_FORMAT`, `GreaterEqual`.

use bevy::{
    asset::AssetServer,
    core_pipeline::{core_3d::CORE_3D_DEPTH_FORMAT, prepass::NORMAL_PREPASS_FORMAT},
    mesh::VertexBufferLayout,
    prelude::*,
    render::{
        mesh::MeshVertexBufferLayoutRef,
        render_resource::{
            BindGroupLayoutDescriptor, BlendState, ColorTargetState, ColorWrites, CompareFunction,
            DepthBiasState, DepthStencilState, Face, FragmentState, MultisampleState,
            PrimitiveState, PrimitiveTopology, RenderPipelineDescriptor, SpecializedMeshPipeline,
            SpecializedMeshPipelineError, StencilState, TextureFormat, VertexState,
        },
    },
    shader::{Shader, ShaderDefVal},
};
use engine_api::{TOON_DOUBLE_SIDED_DEF, ToonMaterial, ToonMaterialKey, UnlitMaterial};

use crate::game::world::{sky::SkyDomeMaterial, terrain::material::TerrainMaterial};

use super::{
    SceneBindGroupLayout, SceneLayer,
    material::{MaterialLayout, SceneMaterial, ScenePipeline},
};

/// Everything about a *view* that changes a pipeline.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SceneViewKey {
    pub msaa_samples: u32,
    /// The view target's colour format.
    pub target_format: TextureFormat,
    /// Whether the view has a `NormalPrepass`, and so a second colour target
    /// to write world normals into. The 3D camera does; the thumbnail rigs
    /// don't, and get a one-target variant (the shaders drop the output under
    /// `SCENE_NORMAL_TARGET`, since WebGPU wants outputs and targets to agree).
    pub normal_target: bool,
}

/// Shader def set on the pipelines of a view with a normal target.
const SCENE_NORMAL_TARGET_DEF: &str = "SCENE_NORMAL_TARGET";

/// Bind group 1 is the material's, so the fragment shaders' existing
/// `@group(constants::MATERIAL_BIND_GROUP)` lines need no edit — bevy set the
/// same def to 3 for its own mesh layout, we set it to 1 for ours.
const MATERIAL_BIND_GROUP: u32 = 1;

/// The shader handles and layouts a specializer needs, loaded once.
struct SceneShaders {
    scene_layout: BindGroupLayoutDescriptor,
    material_layout: BindGroupLayoutDescriptor,
    vertex: Handle<Shader>,
    fragment: Handle<Shader>,
}

impl SceneShaders {
    fn load<M: bevy::render::render_resource::AsBindGroup + Asset>(
        world: &World,
        vertex: &'static str,
        fragment: &'static str,
    ) -> Self {
        let assets = world.resource::<AssetServer>();
        Self {
            scene_layout: world.resource::<SceneBindGroupLayout>().0.clone(),
            material_layout: world.resource::<MaterialLayout<M>>().0.clone(),
            vertex: assets.load(vertex),
            fragment: assets.load(fragment),
        }
    }

    /// The whole descriptor given the per-material pieces: the vertex layout
    /// the mesh supports, the defs both stages share, the cull mode, and the
    /// layer (which decides depth *write* — a background material tests depth
    /// but must not add to it — and whether the normal target is written).
    fn descriptor(
        &self,
        label: &'static str,
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
        let mut targets = vec![Some(ColorTargetState {
            format: view.target_format,
            // Only the transparent layer blends; everything else overwrites.
            blend: (layer == SceneLayer::Transparent).then_some(BlendState::ALPHA_BLENDING),
            write_mask: ColorWrites::ALL,
        })];
        if view.normal_target {
            shader_defs.push(SCENE_NORMAL_TARGET_DEF.into());
            targets.push(Some(ColorTargetState {
                format: NORMAL_PREPASS_FORMAT,
                blend: None,
                // Only solid geometry leaves a normal behind. The sky must
                // not (the edge detector and the atmosphere treat "nothing
                // written" as sky) and neither may a translucent overlay (a
                // blob shadow would get its own outline). The shader still
                // declares the output — WebGPU wants outputs and targets to
                // agree — and the mask drops it.
                write_mask: if layer == SceneLayer::Opaque {
                    ColorWrites::ALL
                } else {
                    ColorWrites::empty()
                },
            }));
        }
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
                shader: self.fragment.clone(),
                shader_defs,
                targets,
                ..default()
            }),
            primitive: PrimitiveState {
                // Every mesh this pass draws is a triangle list (terrain/mesh.rs
                // builds one; glTF primitives default to one), so the topology
                // is not in the key.
                topology: PrimitiveTopology::TriangleList,
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
        ))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TerrainPipelineKey {
    pub view: SceneViewKey,
}

impl ScenePipeline<TerrainMaterial> for TerrainScenePipeline {
    fn key(view: SceneViewKey, _: &()) -> Self::Key {
        TerrainPipelineKey { view }
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
        // Terrain is single-sided; back faces culled.
        Ok(self.0.descriptor(
            "terrain_scene_pipeline",
            key.view,
            <TerrainMaterial as SceneMaterial>::layer(&()),
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
        ))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ToonPipelineKey {
    pub view: SceneViewKey,
    pub material: ToonMaterialKey,
}

impl ScenePipeline<ToonMaterial> for ToonScenePipeline {
    fn key(view: SceneViewKey, material: &ToonMaterialKey) -> Self::Key {
        ToonPipelineKey {
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
    /// shader's existing `@if`s are untouched. Shader locations are fixed by
    /// `scene/mesh_vertex.wesl`'s `Vertex` struct.
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

        // Double-sided: no culling, and the fragment shader flips the normal
        // on back faces (`TOON_DOUBLE_SIDED` in `facing_normal`) — for the
        // probe lighting and for the normal target alike.
        let cull_mode = if key.material.double_sided {
            shader_defs.push(TOON_DOUBLE_SIDED_DEF.into());
            None
        } else {
            Some(Face::Back)
        };
        // The layer comes from the key (`alpha_blend` → Transparent), through
        // the same function `queue_draws` files the draw under, so the
        // pipeline's blend/depth/mask state always matches its layer. The
        // shader needs no variant: it computes and writes alpha either way,
        // and only the blend state decides whether that means anything.
        Ok(self.0.descriptor(
            "toon_scene_pipeline",
            key.view,
            <ToonMaterial as SceneMaterial>::layer(&key.material),
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
        ))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SkyPipelineKey {
    pub view: SceneViewKey,
}

impl ScenePipeline<SkyDomeMaterial> for SkyScenePipeline {
    fn key(view: SceneViewKey, _: &()) -> Self::Key {
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
            key.view,
            <SkyDomeMaterial as SceneMaterial>::layer(&()),
            vertex_layout,
            Vec::new(),
            // The camera is *inside* the sphere.
            None,
        ))
    }
}

// ---------------------------------------------------------------------------
// Unlit (translucent overlays)
// ---------------------------------------------------------------------------

#[derive(Resource)]
pub struct UnlitScenePipeline(SceneShaders);

impl FromWorld for UnlitScenePipeline {
    fn from_world(world: &mut World) -> Self {
        Self(SceneShaders::load::<UnlitMaterial>(
            world,
            "shaders/scene/mesh_vertex.wesl",
            "shaders/unlit.wesl",
        ))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct UnlitPipelineKey {
    pub view: SceneViewKey,
}

impl ScenePipeline<UnlitMaterial> for UnlitScenePipeline {
    fn key(view: SceneViewKey, _: &()) -> Self::Key {
        UnlitPipelineKey { view }
    }
}

impl SpecializedMeshPipeline for UnlitScenePipeline {
    type Key = UnlitPipelineKey;

    /// Position, normal (the shared vertex stage wants one) and UV0 when the
    /// mesh has it. Culling off: a disc is seen from both sides, and a ghost
    /// is translucent so its back faces are part of the look.
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
        let vertex_layout = layout.0.get_layout(&attributes)?;
        Ok(self.0.descriptor(
            "unlit_scene_pipeline",
            key.view,
            <UnlitMaterial as SceneMaterial>::layer(&()),
            vertex_layout,
            shader_defs,
            None,
        ))
    }
}
