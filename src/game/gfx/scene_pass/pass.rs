//! The two render passes, as plain `Core3d` systems — the same shape as
//! `post_process.rs` and `atmosphere.rs`, which is the extension mechanism
//! bevy 0.20 leaves in place of the deleted render graph.
//!
//! Both run *before* their bevy counterpart (`early_prepass`,
//! `main_opaque_pass_3d`), which matters for two reasons:
//!
//! - **Clearing.** `ColorAttachment` and `DepthAttachment` clear on their
//!   first use in a frame and load afterwards, tracked with an atomic, so
//!   whoever draws first owns the clear. Running first means we clear and
//!   bevy loads; running second would mean bevy clears our work away.
//! - **The prepass depth copy.** Bevy's prepass renders depth into the view's
//!   main depth texture and copies it into `ViewPrepassTextures.depth` at the
//!   *end* of its pass. The edge detector and the atmosphere read that copy,
//!   so our geometry has to be in the depth buffer before bevy makes it.
//!
//! Bevy's passes still run even with nothing of its own to draw: they
//! early-return on the view's binned phases being *absent*, not empty, and
//! those phases come from bevy_core_pipeline's own extraction.

use bevy::{
    core_pipeline::prepass::ViewPrepassTextures,
    prelude::*,
    render::{
        camera::ExtractedCamera,
        diagnostic::RecordDiagnostics,
        mesh::{RenderMesh, RenderMeshBufferInfo, allocator::MeshAllocator},
        render_asset::RenderAssets,
        render_resource::{CachedRenderPipelineId, PipelineCache, RenderPassDescriptor, StoreOp},
        renderer::{RenderContext, ViewQuery},
        view::{ViewDepthStencilTexture, ViewTarget, ViewUniformOffset},
    },
};

use bevy::render::render_phase::TrackedRenderPass;

use super::{SceneBindGroup, SceneDraw, SceneDraws};

/// Depth + world normals for everything the scene pass owns.
pub fn scene_prepass(
    view: ViewQuery<(
        &ViewDepthStencilTexture,
        &ViewPrepassTextures,
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
    let (depth, prepass_textures, view_offset, camera) = view.into_inner();
    let (Some(bind_group), Some(draws)) = (bind_group, draws.0.get(&view_entity)) else {
        return;
    };
    if draws.is_empty() {
        return;
    }

    // The attachment list has to stay four long and in bevy's order (normal,
    // motion vectors, deferred, deferred id) because the pipelines were
    // specialized against `prepass_target_descriptors`.
    let color_attachments = [
        prepass_textures.normal.as_ref().map(|t| t.get_attachment()),
        None,
        None,
        None,
    ];
    if color_attachments.iter().all(Option::is_none) {
        // A depth-only prepass: our pipelines were specialized with a normal
        // target, so they can't be bound against an empty attachment list.
        // Consequence, if a camera is ever configured that way: terrain is in
        // the colour pass but not in `ViewPrepassTextures.depth`, so outlines
        // and fog would see through it. Our 3D camera always has both
        // (`gfx/camera.rs`), and the fix if that changes is a `normal_prepass`
        // bit in `ScenePipelineKey` with no fragment stage when it is off.
        return;
    }

    let diagnostics = ctx.diagnostic_recorder();
    let diagnostics = diagnostics.as_deref();
    let mut pass = ctx.begin_tracked_render_pass(RenderPassDescriptor {
        label: Some("scene_prepass"),
        color_attachments: &color_attachments,
        depth_stencil_attachment: Some(depth.get_attachment(StoreOp::Store)),
        timestamp_writes: None,
        occlusion_query_set: None,
        multiview_mask: None,
    });
    let span = diagnostics.pass_span(&mut pass, "scene prepass");

    if let Some(viewport) = camera.viewport.as_ref() {
        pass.set_camera_viewport(viewport);
    }
    render_draws(
        &mut pass,
        draws,
        |draw| draw.prepass_pipeline,
        &bind_group,
        view_offset,
        &pipeline_cache,
        &meshes,
        &mesh_allocator,
    );

    span.end(&mut pass);
}

/// Shaded colour into the view's HDR target.
pub fn scene_opaque(
    view: ViewQuery<(
        &ViewTarget,
        &ViewDepthStencilTexture,
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
    let (target, depth, view_offset, camera) = view.into_inner();
    let (Some(bind_group), Some(draws)) = (bind_group, draws.0.get(&view_entity)) else {
        return;
    };
    if draws.is_empty() {
        return;
    }

    let color_attachments = [Some(target.get_color_attachment())];

    let diagnostics = ctx.diagnostic_recorder();
    let diagnostics = diagnostics.as_deref();
    let mut pass = ctx.begin_tracked_render_pass(RenderPassDescriptor {
        label: Some("scene_opaque"),
        color_attachments: &color_attachments,
        depth_stencil_attachment: Some(depth.get_attachment(StoreOp::Store)),
        timestamp_writes: None,
        occlusion_query_set: None,
        multiview_mask: None,
    });
    let span = diagnostics.pass_span(&mut pass, "scene opaque");

    if let Some(viewport) = camera.viewport.as_ref() {
        pass.set_camera_viewport(viewport);
    }
    render_draws(
        &mut pass,
        draws,
        |draw| Some(draw.colour_pipeline),
        &bind_group,
        view_offset,
        &pipeline_cache,
        &meshes,
        &mesh_allocator,
    );

    span.end(&mut pass);
}

/// The draw loop both passes share: one `draw_indexed` per [`SceneDraw`].
/// Material-agnostic: a draw carries its prepared bind group.
///
/// `@builtin(instance_index)` is `firstInstance + i` under WebGPU, so passing
/// the draw's instance range is all the shader needs to find its own slice of
/// the instance buffer — no per-draw uniform, no dynamic offset.
#[expect(clippy::too_many_arguments, reason = "render pass state")]
fn render_draws<'w>(
    pass: &mut TrackedRenderPass<'w>,
    draws: &'w [SceneDraw],
    pipeline_of: impl Fn(&SceneDraw) -> Option<CachedRenderPipelineId>,
    bind_group: &'w SceneBindGroup,
    view_offset: &ViewUniformOffset,
    pipeline_cache: &'w PipelineCache,
    meshes: &RenderAssets<RenderMesh>,
    mesh_allocator: &'w MeshAllocator,
) {
    pass.set_bind_group(0, &bind_group.0, &[view_offset.offset]);

    for draw in draws {
        // `None` is a draw this pass doesn't carry at all — a background
        // material in the prepass; otherwise the pipeline is still compiling.
        let Some(pipeline) =
            pipeline_of(draw).and_then(|id| pipeline_cache.get_render_pipeline(id))
        else {
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
