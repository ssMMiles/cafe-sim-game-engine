use bevy::prelude::*;

#[derive(Resource, Clone)]
pub struct UiTheme {
    // Backgrounds
    pub bg_overlay: Color, // fullscreen menu backgrounds
    pub bg_dark: Color,    // main menu background
    pub bg_panel: Color,   // HUD panels
    pub bg_surface: Color, // cards, track backgrounds
    pub bg_surface_hover: Color,

    // Buttons — primary
    pub primary: Color,
    pub primary_hover: Color,
    pub primary_pressed: Color,

    // Buttons — danger
    pub danger: Color,
    pub danger_hover: Color,
    pub danger_pressed: Color,

    // Borders
    pub border: Color,        // default subtle border
    pub border_strong: Color, // accent border (hover/focus)

    // Text
    pub text: Color,        // body text
    pub text_dim: Color,    // secondary/dim text
    pub text_accent: Color, // value text (cyan tint)
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
    pub btn_height: f32, // 56.0
    pub btn_border: f32, // 2.0

    // Slider sizes
    pub slider_track_width: f32,  // 300.0
    pub slider_track_height: f32, // 6.0
    pub slider_handle_size: f32,  // 20.0
}

impl Default for UiTheme {
    fn default() -> Self {
        Self {
            // Slate backgrounds — dark, slightly cool
            bg_overlay: Color::srgba(0.04, 0.06, 0.09, 0.85),
            bg_dark: Color::srgb(0.07, 0.09, 0.12),
            bg_panel: Color::srgba(0.09, 0.11, 0.14, 0.82),
            bg_surface: Color::srgb(0.14, 0.17, 0.21),
            bg_surface_hover: Color::srgb(0.20, 0.24, 0.30),

            // Primary — teal/cyan
            primary: Color::srgb(0.16, 0.40, 0.52),
            primary_hover: Color::srgb(0.24, 0.55, 0.70),
            primary_pressed: Color::srgb(0.11, 0.32, 0.44),

            // Danger — muted red
            danger: Color::srgb(0.55, 0.22, 0.27),
            danger_hover: Color::srgb(0.72, 0.32, 0.36),
            danger_pressed: Color::srgb(0.45, 0.18, 0.22),

            border: Color::srgb(0.22, 0.27, 0.34),
            border_strong: Color::srgb(0.45, 0.72, 0.92),

            text: Color::srgb(0.92, 0.93, 0.95),
            text_dim: Color::srgb(0.60, 0.66, 0.72),
            text_accent: Color::srgb(0.55, 0.78, 0.95),
            text_white: Color::WHITE,
            highlight: Color::srgb(0.40, 0.72, 0.95),
            positive: Color::srgb(0.42, 0.85, 0.55),

            size_title: 48.0,
            size_body: 16.0,
            size_label: 14.0,
            size_caption: 12.0,

            pad_sm: 8.0,
            pad_md: 12.0,
            pad_lg: 20.0,

            btn_width: 200.0,
            btn_height: 56.0,
            btn_border: 2.0,

            slider_track_width: 300.0,
            slider_track_height: 6.0,
            slider_handle_size: 20.0,
        }
    }
}
