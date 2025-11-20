use bevy::{
    prelude::*,
    render::render_resource::{AsBindGroup, ShaderRef, ShaderType},
    sprite::{Material2d, Material2dPlugin},
};

pub struct EdgeDetectionPlugin;

impl Plugin for EdgeDetectionPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(Material2dPlugin::<EdgeDetectionMaterial>::default());
    }
}

#[derive(Asset, TypePath, AsBindGroup, Clone)]
pub struct EdgeDetectionMaterial {
    // Bind the color render texture
    #[texture(0)]
    #[sampler(1)]
    pub color_texture: Handle<Image>,

    // Depth texture from prepass
    #[texture(2)]
    #[sampler(3)]
    pub depth_texture: Handle<Image>,

    // Normal texture from prepass
    #[texture(4)]
    #[sampler(5)]
    pub normal_texture: Handle<Image>,

    // Edge detection parameters
    #[uniform(6)]
    pub settings: EdgeDetectionSettings,
}

#[derive(Clone, Copy, ShaderType)]
pub struct EdgeDetectionSettings {
    pub edge_thickness: f32,
    pub depth_sensitivity: f32,
    pub normal_sensitivity: f32,
    pub edge_color: LinearRgba,
}

impl Default for EdgeDetectionSettings {
    fn default() -> Self {
        Self {
            edge_thickness: 1.0,         // 1 pixel
            depth_sensitivity: 100.0,    // Tune based on testing
            normal_sensitivity: 2.0,     // Tune based on testing
            edge_color: LinearRgba::BLACK, // Black outlines
        }
    }
}

impl Material2d for EdgeDetectionMaterial {
    fn fragment_shader() -> ShaderRef {
        "shaders/edge_detection.wgsl".into()
    }
}
