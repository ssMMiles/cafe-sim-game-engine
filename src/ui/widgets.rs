use bevy::{ecs::relationship::RelatedSpawnerCommands, prelude::*};

use super::theme::UiTheme;

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

/// Per-state visuals for any interactive widget. Attach alongside `Button`
/// (and optionally `BorderColor`) and the `update_interactive_visuals` system
/// will swap colors as `Interaction` changes.
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

    /// Subtle hover lift for surface-level rows (checkbox row, list items).
    pub fn surface(theme: &UiTheme) -> Self {
        Self {
            idle_bg: theme.bg_panel,
            hover_bg: theme.bg_surface_hover,
            pressed_bg: theme.bg_surface,
            idle_border: Color::NONE,
            hover_border: theme.border,
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
        self.spawn((
            Text::new(text),
            TextFont {
                font_size: FontSize::Px(theme.size_title),
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
        self.spawn((
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
        .with_child((
            Text::new(text),
            TextFont {
                font_size: FontSize::Px(theme.size_body),
                ..default()
            },
            TextColor(theme.text),
        ))
        .id()
    }
}
