use bevy::prelude::*;

#[derive(Resource, Clone)]
pub struct UiTheme {
    // Colors
    pub bg_overlay: Color,  // fullscreen menu backgrounds
    pub bg_dark: Color,     // main menu background
    pub bg_panel: Color,    // HUD panels
    pub bg_surface: Color,  // cards, track backgrounds
    pub primary: Color,     // primary buttons
    pub danger: Color,      // destructive buttons
    pub text: Color,        // body text
    pub text_dim: Color,    // secondary/dim text
    pub text_accent: Color, // value text (blue tint)
    pub text_white: Color,
    pub highlight: Color, // selection highlight
    pub positive: Color,  // positive/active state

    // Typography
    pub size_title: f32,   // 48.0
    pub size_body: f32,    // 16.0
    pub size_label: f32,   // 14.0
    pub size_caption: f32, // 12.0

    // Spacing
    pub pad_sm: f32, // 8.0
    pub pad_md: f32, // 12.0
    pub pad_lg: f32, // 20.0

    // Button sizes
    pub btn_width: f32,  // 200.0
    pub btn_height: f32, // 65.0
    pub btn_border: f32, // 5.0

    // Slider sizes
    pub slider_track_width: f32,  // 300.0
    pub slider_track_height: f32, // 20.0
    pub slider_handle_size: f32,  // 20.0
}

impl Default for UiTheme {
    fn default() -> Self {
        Self {
            bg_overlay: Color::srgba(0.0, 0.0, 0.0, 0.8),
            bg_dark: Color::srgba(0.1, 0.1, 0.8, 1.0),
            bg_panel: Color::srgba(0.0, 0.0, 0.0, 0.7),
            bg_surface: Color::srgb(0.3, 0.3, 0.3),
            primary: Color::srgb(0.4, 0.4, 0.2),
            danger: Color::srgb(0.6, 0.2, 0.2),
            text: Color::srgb(0.9, 0.9, 0.9),
            text_dim: Color::srgb(0.8, 0.8, 0.8),
            text_accent: Color::srgb(0.6, 0.8, 1.0),
            text_white: Color::WHITE,
            highlight: Color::srgb(0.3, 0.6, 1.0),
            positive: Color::srgb(0.4, 1.0, 0.4),

            size_title: 48.0,
            size_body: 16.0,
            size_label: 14.0,
            size_caption: 12.0,

            pad_sm: 8.0,
            pad_md: 12.0,
            pad_lg: 20.0,

            btn_width: 200.0,
            btn_height: 65.0,
            btn_border: 5.0,

            slider_track_width: 300.0,
            slider_track_height: 20.0,
            slider_handle_size: 20.0,
        }
    }
}
