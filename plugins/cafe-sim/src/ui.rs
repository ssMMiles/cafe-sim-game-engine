//! Plugin-local UI primitives.
//!
//! Forked from the engine's `src/ui/{theme,widgets}.rs` as a starting base.
//! Lives in the plugin (rather than `engine_api`) so cafe-sim can diverge its
//! visual language without churning the shared engine surface. Every spawned
//! widget is tagged with `ScriptOwned` so plugin teardown sweeps it up.

use bevy::{ecs::relationship::RelatedSpawnerCommands, prelude::*};

use crate::spawn::{SpawnOwnedCommands, WithChildOwned};

#[derive(Resource, Clone)]
#[allow(dead_code)]
pub struct UiTheme {
    pub bg_overlay: Color,
    pub bg_panel: Color,
    pub bg_surface: Color,
    pub bg_surface_hover: Color,

    pub primary: Color,
    pub primary_hover: Color,
    pub primary_pressed: Color,

    pub danger: Color,
    pub danger_hover: Color,
    pub danger_pressed: Color,

    pub border: Color,
    pub border_strong: Color,

    pub text: Color,
    pub text_dim: Color,
    pub text_white: Color,

    pub size_title: f32,
    pub size_body: f32,
    pub size_label: f32,

    pub pad_sm: f32,
    pub pad_md: f32,
    pub pad_lg: f32,

    pub btn_width: f32,
    pub btn_height: f32,
    pub btn_border: f32,
}

impl Default for UiTheme {
    fn default() -> Self {
        Self {
            bg_overlay: Color::srgba(0.04, 0.06, 0.09, 0.85),
            bg_panel: Color::srgba(0.09, 0.11, 0.14, 0.82),
            bg_surface: Color::srgb(0.14, 0.17, 0.21),
            bg_surface_hover: Color::srgb(0.20, 0.24, 0.30),

            primary: Color::srgb(0.16, 0.40, 0.52),
            primary_hover: Color::srgb(0.24, 0.55, 0.70),
            primary_pressed: Color::srgb(0.11, 0.32, 0.44),

            danger: Color::srgb(0.55, 0.22, 0.27),
            danger_hover: Color::srgb(0.72, 0.32, 0.36),
            danger_pressed: Color::srgb(0.45, 0.18, 0.22),

            border: Color::srgb(0.22, 0.27, 0.34),
            border_strong: Color::srgb(0.45, 0.72, 0.92),

            text: Color::srgb(0.92, 0.93, 0.95),
            text_dim: Color::srgb(0.60, 0.66, 0.72),
            text_white: Color::WHITE,

            size_title: 48.0,
            size_body: 16.0,
            size_label: 14.0,

            pad_sm: 8.0,
            pad_md: 12.0,
            pad_lg: 20.0,

            btn_width: 200.0,
            btn_height: 56.0,
            btn_border: 2.0,
        }
    }
}

#[derive(Clone, Copy)]
pub enum ButtonVariant {
    Primary,
    Danger,
}

impl ButtonVariant {
    fn colors(&self, theme: &UiTheme) -> (Color, Color, Color) {
        match self {
            ButtonVariant::Primary => (theme.primary, theme.primary_hover, theme.primary_pressed),
            ButtonVariant::Danger => (theme.danger, theme.danger_hover, theme.danger_pressed),
        }
    }
}

/// Per-state visuals for any interactive widget. Attach alongside `Button` and
/// the [`update_interactive_visuals`] system will swap colors as `Interaction`
/// changes.
#[derive(Component, Clone, Copy)]
pub struct InteractiveStyle {
    pub idle_bg: Color,
    pub hover_bg: Color,
    pub pressed_bg: Color,
    pub idle_border: Color,
    pub hover_border: Color,
    pub pressed_border: Color,
}

impl InteractiveStyle {
    pub fn button(theme: &UiTheme, variant: ButtonVariant) -> Self {
        let (idle, hover, pressed) = variant.colors(theme);
        Self {
            idle_bg: idle,
            hover_bg: hover,
            pressed_bg: pressed,
            idle_border: theme.border,
            hover_border: theme.border_strong,
            pressed_border: theme.border_strong,
        }
    }
}

pub fn update_interactive_visuals(
    mut q: Query<
        (
            &Interaction,
            &InteractiveStyle,
            &mut BackgroundColor,
            Option<&mut BorderColor>,
        ),
        Changed<Interaction>,
    >,
) {
    for (interaction, style, mut bg, border) in &mut q {
        let (target_bg, target_border) = match *interaction {
            Interaction::None => (style.idle_bg, style.idle_border),
            Interaction::Hovered => (style.hover_bg, style.hover_border),
            Interaction::Pressed => (style.pressed_bg, style.pressed_border),
        };
        bg.0 = target_bg;
        if let Some(mut border) = border {
            *border = BorderColor::all(target_border);
        }
    }
}

pub trait UiExt {
    fn ui_title(&mut self, text: &str, theme: &UiTheme) -> Entity;
    fn ui_button<A: Component>(
        &mut self,
        text: &str,
        action: A,
        variant: ButtonVariant,
        theme: &UiTheme,
    ) -> Entity;
}

impl UiExt for RelatedSpawnerCommands<'_, ChildOf> {
    fn ui_title(&mut self, text: &str, theme: &UiTheme) -> Entity {
        self.spawn_owned((
            Text::new(text),
            TextFont {
                font_size: theme.size_title,
                ..default()
            },
            TextColor(theme.text_white),
            Node {
                margin: UiRect::bottom(Val::Px(50.0)),
                ..default()
            },
        ))
        .id()
    }

    fn ui_button<A: Component>(
        &mut self,
        text: &str,
        action: A,
        variant: ButtonVariant,
        theme: &UiTheme,
    ) -> Entity {
        let style = InteractiveStyle::button(theme, variant);
        self.spawn_owned((
            Button,
            Node {
                width: Val::Px(theme.btn_width),
                height: Val::Px(theme.btn_height),
                border: UiRect::all(Val::Px(theme.btn_border)),
                border_radius: BorderRadius::all(Val::Px(6.0)),
                justify_content: JustifyContent::Center,
                align_items: AlignItems::Center,
                margin: UiRect::all(Val::Px(theme.pad_sm)),
                ..default()
            },
            BackgroundColor(style.idle_bg),
            BorderColor::all(style.idle_border),
            style,
            action,
        ))
        .with_child_owned((
            Text::new(text),
            TextFont {
                font_size: theme.size_body,
                ..default()
            },
            TextColor(theme.text),
        ))
        .id()
    }
}
