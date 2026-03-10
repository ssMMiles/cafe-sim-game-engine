use bevy::{
    asset::RenderAssetUsages,
    log,
    prelude::*,
    render::render_resource::{Extent3d, TextureDimension, TextureFormat},
};

#[derive(Resource, Default)]
pub struct SvgTextureCache {
    pub cake_slice: Option<Handle<Image>>,
    pub cupcake: Option<Handle<Image>>,
}

fn render_svg_to_image(svg_bytes: &[u8], width: u32, height: u32) -> Result<Image, String> {
    // Parse SVG with usvg
    let opt = usvg::Options::default();
    let tree =
        usvg::Tree::from_data(svg_bytes, &opt).map_err(|e| format!("SVG parse error: {}", e))?;

    // Create tiny-skia pixmap
    let mut pixmap = tiny_skia::Pixmap::new(width, height).ok_or("Failed to create pixmap")?;

    // Clear pixmap to transparent
    pixmap.fill(tiny_skia::Color::TRANSPARENT);

    // Render SVG to pixmap with scaling to fit
    let size = tree.size();
    let scale_x = width as f32 / size.width();
    let scale_y = height as f32 / size.height();
    let scale = scale_x.min(scale_y); // Maintain aspect ratio

    let transform = tiny_skia::Transform::from_scale(scale, scale);
    resvg::render(&tree, transform, &mut pixmap.as_mut());

    // Convert all pixels to white while preserving alpha
    let pixels = pixmap.pixels_mut();
    for pixel in pixels {
        if pixel.alpha() > 0 {
            *pixel = tiny_skia::ColorU8::from_rgba(255, 255, 255, pixel.alpha()).premultiply();
        }
    }

    // Convert to Bevy Image
    Ok(Image::new(
        Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        pixmap.take(),
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::RENDER_WORLD,
    ))
}

pub fn load_svg_icons(mut images: ResMut<Assets<Image>>, mut svg_cache: ResMut<SvgTextureCache>) {
    // Only render if we haven't cached them yet
    if svg_cache.cake_slice.is_none() {
        match std::fs::read("assets/icons/cake-slice.svg") {
            Ok(svg_bytes) => {
                log::info!(
                    "Successfully loaded cake-slice.svg ({} bytes)",
                    svg_bytes.len()
                );
                match render_svg_to_image(&svg_bytes, 128, 128) {
                    Ok(image) => {
                        let handle = images.add(image);
                        svg_cache.cake_slice = Some(handle);
                        log::info!("Successfully rendered cake-slice.svg to texture");
                    }
                    Err(e) => {
                        log::error!("Failed to render cake-slice.svg: {}", e);
                    }
                }
            }
            Err(e) => {
                log::error!("Could not read assets/icons/cake-slice.svg: {}", e);
            }
        }
    }

    if svg_cache.cupcake.is_none() {
        match std::fs::read("assets/icons/cupcake.svg") {
            Ok(svg_bytes) => {
                log::info!(
                    "Successfully loaded cupcake.svg ({} bytes)",
                    svg_bytes.len()
                );
                match render_svg_to_image(&svg_bytes, 128, 128) {
                    Ok(image) => {
                        let handle = images.add(image);
                        svg_cache.cupcake = Some(handle);
                        log::info!("Successfully rendered cupcake.svg to texture");
                    }
                    Err(e) => {
                        log::error!("Failed to render cupcake.svg: {}", e);
                    }
                }
            }
            Err(e) => {
                log::error!("Could not read assets/icons/cupcake.svg: {}", e);
            }
        }
    }
}
