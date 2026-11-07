//! Pipelines for the scene pass.
//!
//! One specializer per material type (just terrain in Phase 1), keyed on the
//! pass it feeds and the view's MSAA and colour format. The mesh's vertex
//! layout is the *other* half of the key: `SpecializedMeshPipelines` (plain
//! bevy_render, no bevy_pbr) caches on `(layout, key)` and hands the layout to
//! `specialize`, which is where the vertex-buffer layout comes from.
//!
//! The depth state, the prepass attachment list and the reverse-Z compare
//! function all have to match bevy's, because bevy's own passes share the same
//! textures for the stragglers: `CORE_3D_DEPTH_FORMAT`,
//! `GreaterEqual`, and the four-slot prepass target list from
//! `prepass_target_descriptors` (normal, motion vectors, deferred, deferred
//! id) with only the first filled.

use bevy::{
    asset::AssetServer,
    core_pipeline::{core_3d::CORE_3D_DEPTH_FORMAT, prepass::prepass_target_descriptors},
    prelude::*,
    render::{
        mesh::MeshVertexBufferLayoutRef,
        render_resource::{
            AsBindGroup, BindGroupLayoutDescriptor, ColorTargetState, ColorWrites, CompareFunction,
            DepthBiasState, DepthStencilState, Face, FragmentState, MultisampleState,
            PrimitiveState, PrimitiveTopology, RenderPipelineDescriptor, SpecializedMeshPipeline,
            SpecializedMeshPipelineError, StencilState, TextureFormat, VertexState,
        },
        renderer::RenderDevice,
    },
    shader::{Shader, ShaderDefVal},
};

use crate::game::world::terrain::material::TerrainMaterial;

use super::SceneBindGroupLayout;

/// Which of the two passes a pipeline feeds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ScenePassKind {
    /// Depth + the world-normal target the edge detector reads.
    Prepass,
    /// The view's HDR colour target.
    Colour,
}

/// Everything about a view that changes a pipeline.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ScenePipelineKey {
    pub pass: ScenePassKind,
    pub msaa_samples: u32,
    /// The view target's colour format. Ignored by the prepass variant, whose
    /// format is fixed by `prepass_target_descriptors`; carried in the key
    /// anyway so the two variants can share one struct.
    pub target_format: TextureFormat,
}

/// Bind group 1 is the material's, so the fragment shaders' existing
/// `@group(constants::MATERIAL_BIND_GROUP)` lines need no edit — bevy set the
/// same def to 3 for its own mesh layout, we set it to 1 for ours.
const MATERIAL_BIND_GROUP: u32 = 1;

#[derive(Resource)]
pub struct TerrainScenePipeline {
    scene_layout: BindGroupLayoutDescriptor,
    /// Bind group 1. Also what `PreparedTerrainMaterial` builds against.
    pub material_layout: BindGroupLayoutDescriptor,
    vertex_shader: Handle<Shader>,
    colour_shader: Handle<Shader>,
    prepass_shader: Handle<Shader>,
}

pub fn init_terrain_scene_pipeline(
    mut commands: Commands,
    asset_server: Res<AssetServer>,
    device: Res<RenderDevice>,
    scene_layout: Res<SceneBindGroupLayout>,
) {
    commands.insert_resource(TerrainScenePipeline {
        scene_layout: scene_layout.0.clone(),
        material_layout: TerrainMaterial::bind_group_layout_descriptor(&device),
        vertex_shader: asset_server.load("shaders/scene/terrain_vertex.wesl"),
        colour_shader: asset_server.load("shaders/terrain_toon.wesl"),
        prepass_shader: asset_server.load("shaders/scene/terrain_prepass.wesl"),
    });
}

impl SpecializedMeshPipeline for TerrainScenePipeline {
    type Key = ScenePipelineKey;

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

        let shader_defs = vec![ShaderDefVal::UInt(
            "MATERIAL_BIND_GROUP".into(),
            MATERIAL_BIND_GROUP,
        )];

        let (label, fragment_shader, targets) = match key.pass {
            ScenePassKind::Prepass => (
                "terrain_scene_prepass_pipeline",
                self.prepass_shader.clone(),
                // Normal only. The list must stay four long and in this order:
                // bevy's prepass node hands the render pass four attachment
                // slots whenever a prepass runs at all.
                prepass_target_descriptors(true, false, false),
            ),
            ScenePassKind::Colour => (
                "terrain_scene_colour_pipeline",
                self.colour_shader.clone(),
                vec![Some(ColorTargetState {
                    format: key.target_format,
                    blend: None,
                    write_mask: ColorWrites::ALL,
                })],
            ),
        };

        Ok(RenderPipelineDescriptor {
            label: Some(label.into()),
            layout: vec![self.scene_layout.clone(), self.material_layout.clone()],
            vertex: VertexState {
                shader: self.vertex_shader.clone(),
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
                // Chunk meshes are always triangle lists (terrain/mesh.rs), so
                // the topology is not in the key.
                topology: PrimitiveTopology::TriangleList,
                // Back faces culled in *both* passes. Bevy's prepass leaves
                // culling off while its opaque pass turns it on, which lets a
                // back face win the depth buffer on a pixel the colour pass
                // then discards — a hole with fog in it. Terrain is
                // single-sided; keeping the two passes identical is the whole
                // point of having a prepass.
                cull_mode: Some(Face::Back),
                ..default()
            },
            depth_stencil: Some(DepthStencilState {
                format: CORE_3D_DEPTH_FORMAT,
                depth_write_enabled: Some(true),
                // Reverse-Z, as everywhere else in the 3D view.
                depth_compare: Some(CompareFunction::GreaterEqual),
                stencil: StencilState::default(),
                bias: DepthBiasState::default(),
            }),
            multisample: MultisampleState {
                count: key.msaa_samples,
                mask: !0,
                alpha_to_coverage_enabled: false,
            },
            ..default()
        })
    }
}
