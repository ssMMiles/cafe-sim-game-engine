//! The render pass, as a plain `Core3d` system — the same shape as
//! `post_process.rs` and `atmosphere.rs`, which is the extension mechanism
//! bevy 0.20 leaves in place of the deleted render graph.
//!
//! One pass draws everything the scene pass owns: colour into the view's HDR
//! target, depth into the view's depth texture, and — for a view with a
//! `NormalPrepass` — world normals into `ViewPrepassTextures.normal` as a
//! second colour target. Until 2026-11-08 the normals came from a separate
//! prepass that rasterised the whole scene a second time; see `pipeline.rs`.
//!
//! It runs in `Core3dSystems::Prepass`, *before* bevy's `early_prepass`, which
//! matters for two reasons:
//!
//! - **Clearing.** `ColorAttachment` and `DepthAttachment` clear on their
//!   first use in a frame and load afterwards, tracked with an atomic, so
//!   whoever draws first owns the clear. Running first means we clear and
//!   bevy loads; running second would mean bevy clears our work away.
//! - **The prepass depth copy.** `ViewPrepassTextures.depth` is what the edge
//!   detector and the atmosphere read, and it is a *copy* of the view's depth
//!   texture, made after the geometry is in it. Bevy's prepass used to make
//!   it; since `suppress_bevy_core3d_phases` (mod.rs) stops bevy's passes
//!   from running at all for a view we draw — each one began and resolved a
//!   full-size MSAA render pass even empty — the copy is made here instead,
//!   right after our pass, same encoder, same semantics (the gizmo pass draws
//!   later and was never in the copy).

use bevy::{
    core_pipeline::prepass::ViewPrepassTextures,
    prelude::*,
    render::{
        camera::ExtractedCamera,
        diagnostic::RecordDiagnostics,
        mesh::{RenderMesh, RenderMeshBufferInfo, allocator::MeshAllocator},
        render_asset::RenderAssets,
        render_resource::{PipelineCache, RenderPassDescriptor, StoreOp},
        renderer::{RenderContext, ViewQuery},
        view::{ViewDepthStencilTexture, ViewTarget, ViewUniformOffset},
    },
};

use bevy::render::render_phase::TrackedRenderPass;

use super::{SceneBindGroup, SceneDraw, SceneDraws};

/// Colour, depth and (where the view has one) the normal target, in one pass.
pub fn scene_pass(
    view: ViewQuery<(
        &ViewTarget,
        &ViewDepthStencilTexture,
        Option<&ViewPrepassTextures>,
        &ViewUniformOffset,
        &ExtractedCamera,
    )>,
    draws: Res<SceneDraws>,
    bind_group: Option<Res<SceneBindGroup>>,
    pipeline_cache: Res<PipelineCache>,
    meshes: Res<RenderAssets<RenderMesh>>,
    mesh_allocator: Res<MeshAllocator>,
    mut ctx: RenderContext,
) {
    let view_entity = view.entity();
    let (target, depth, prepass_textures, view_offset, camera) = view.into_inner();
    let (Some(bind_group), Some(draws)) = (bind_group, draws.0.get(&view_entity)) else {
        return;
    };
    if draws.is_empty() {
        return;
    }

    // Target 1 exists exactly when the pipelines were specialized with
    // `normal_target`: both follow the view's `NormalPrepass` marker
    // (`material::queue_draws`), and bevy only creates the normal texture for
    // such a view. The thumbnail rigs have neither.
    let mut color_attachments = vec![Some(target.get_color_attachment())];
    if let Some(normal) = prepass_textures.and_then(|t| t.normal.as_ref()) {
        color_attachments.push(Some(normal.get_attachment()));
    }

    let diagnostics = ctx.diagnostic_recorder();
    let diagnostics = diagnostics.as_deref();
    let mut pass = ctx.begin_tracked_render_pass(RenderPassDescriptor {
        label: Some("scene_pass"),
        color_attachments: &color_attachments,
        depth_stencil_attachment: Some(depth.get_attachment(StoreOp::Store)),
        timestamp_writes: None,
        occlusion_query_set: None,
        multiview_mask: None,
    });
    let span = diagnostics.pass_span(&mut pass, "scene pass");

    if let Some(viewport) = camera.viewport.as_ref() {
        pass.set_camera_viewport(viewport);
    }
    render_draws(
        &mut pass,
        draws,
        &bind_group,
        view_offset,
        &pipeline_cache,
        &meshes,
        &mesh_allocator,
    );

    span.end(&mut pass);
    drop(pass);

    // The depth copy bevy's suppressed prepass used to make (see the module
    // docs). `ViewPrepassTextures.depth` only exists on a view with a
    // `DepthPrepass`, the same views whose post passes read it.
    if let Some(prepass) = prepass_textures
        && let Some(prepass_depth) = prepass.depth.as_ref()
    {
        ctx.command_encoder().copy_texture_to_texture(
            depth.texture().as_image_copy(),
            prepass_depth.texture.texture.as_image_copy(),
            prepass.size,
        );
    }
}

/// The draw loop: one `draw_indexed` per [`SceneDraw`]. Material-agnostic: a
/// draw carries its prepared bind group.
///
/// `@builtin(instance_index)` is `firstInstance + i` under WebGPU, so passing
/// the draw's instance range is all the shader needs to find its own slice of
/// the instance buffer — no per-draw uniform, no dynamic offset.
fn render_draws<'w>(
    pass: &mut TrackedRenderPass<'w>,
    draws: &'w [SceneDraw],
    bind_group: &'w SceneBindGroup,
    view_offset: &ViewUniformOffset,
    pipeline_cache: &'w PipelineCache,
    meshes: &RenderAssets<RenderMesh>,
    mesh_allocator: &'w MeshAllocator,
) {
    pass.set_bind_group(0, &bind_group.0, &[view_offset.offset]);

    for draw in draws {
        // `None`: the pipeline is still compiling.
        let Some(pipeline) = pipeline_cache.get_render_pipeline(draw.pipeline) else {
            continue;
        };
        let Some(mesh) = meshes.get(draw.mesh) else {
            continue;
        };
        let Some(vertices) = mesh_allocator.mesh_vertex_slice(&draw.mesh) else {
            continue;
        };

        pass.set_render_pipeline(pipeline);
        pass.set_bind_group(1, &draw.material, &[]);
        pass.set_vertex_buffer(0, vertices.buffer.slice(..));

        match &mesh.buffer_info {
            RenderMeshBufferInfo::Indexed {
                index_format,
                count,
            } => {
                let Some(indices) = mesh_allocator.mesh_index_slice(&draw.mesh) else {
                    continue;
                };
                pass.set_index_buffer(indices.buffer.slice(..), *index_format);
                pass.draw_indexed(
                    indices.range.start..(indices.range.start + *count),
                    vertices.range.start as i32,
                    draw.instances.clone(),
                );
            }
            RenderMeshBufferInfo::NonIndexed => {
                pass.draw(vertices.range.clone(), draw.instances.clone());
            }
        }
    }
}
