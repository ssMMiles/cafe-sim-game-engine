use bevy::{
    light::NotShadowCaster,
    mesh::MeshVertexBufferLayoutRef,
    pbr::{MaterialPipeline, MaterialPipelineKey, OpaqueRendererMethod},
    prelude::*,
    render::render_resource::{
        AsBindGroup, RenderPipelineDescriptor, SpecializedMeshPipelineError,
    },
    shader::ShaderRef,
};

use crate::game::{GameEntity, camera::PlayerCamera};

/// Marker for the sky dome entity, used by `sync_sky_dome` to keep it centred on the camera.
#[derive(Component)]
pub struct SkyDomeMarker;

/// A simple unlit forward material that renders a toon-gradient sky.
#[derive(Asset, TypePath, AsBindGroup, Clone, Default)]
pub struct SkyDomeMaterial {}

impl Material for SkyDomeMaterial {
    fn fragment_shader() -> ShaderRef {
        "shaders/sky_dome.wgsl".into()
    }

    fn alpha_mode(&self) -> AlphaMode {
        AlphaMode::Opaque
    }

    fn opaque_render_method(&self) -> OpaqueRendererMethod {
        // Use forward rendering so it bypasses the deferred g-buffer pipeline.
        OpaqueRendererMethod::Forward
    }

    fn specialize(
        _pipeline: &MaterialPipeline,
        descriptor: &mut RenderPipelineDescriptor,
        _layout: &MeshVertexBufferLayoutRef,
        _key: MaterialPipelineKey<Self>,
    ) -> Result<(), SpecializedMeshPipelineError> {
        // The camera is *inside* the sphere, so disable back-face culling.
        descriptor.primitive.cull_mode = None;
        // Disable depth write so the sky dome always sits behind all other geometry
        // without poisoning the depth buffer.
        if let Some(ref mut ds) = descriptor.depth_stencil {
            ds.depth_write_enabled = false;
        }
        Ok(())
    }
}

/// Spawns the sky dome sphere.
/// Runs `OnEnter(AppState::InGame)` after the main `setup` system.
pub fn setup_sky(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut sky_materials: ResMut<Assets<SkyDomeMaterial>>,
) {
    // A large UV sphere centred on the world origin. The `sync_sky_dome` system
    // re-centres it on the camera every frame so the horizon band stays at eye level.
    commands.spawn((
        GameEntity,
        SkyDomeMarker,
        Mesh3d(meshes.add(Sphere::new(900.0).mesh().uv(32, 16))),
        MeshMaterial3d(sky_materials.add(SkyDomeMaterial {})),
        NotShadowCaster,
    ));
}

/// Keeps the sky dome centred on the player camera so the horizon band stays at eye level.
pub fn sync_sky_dome(
    camera_query: Query<&Transform, With<PlayerCamera>>,
    mut sky_query: Query<&mut Transform, (With<SkyDomeMarker>, Without<PlayerCamera>)>,
) {
    let Ok(camera_transform) = camera_query.single() else {
        return;
    };
    let Ok(mut sky_transform) = sky_query.single_mut() else {
        return;
    };
    sky_transform.translation = camera_transform.translation;
}
