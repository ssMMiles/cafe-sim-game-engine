use bevy::{
    ecs::relationship::RelatedSpawnerCommands, picking::hover::Hovered, prelude::*, ui::Pressed,
    ui_widgets::Button,
};

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
/// and `Hovered` (and optionally `BorderColor`); [`update_interactive_visuals`]
/// swaps colors as the `Pressed` / `Hovered` state changes.
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

/// Widgets whose interaction state changed this frame. Releases arrive
/// separately, through `RemovedComponents<Pressed>`.
type InteractionChanged = Or<(Changed<Hovered>, Added<Pressed>)>;

/// Everything [`update_interactive_visuals`] reads and writes on a widget.
type InteractiveWidget = (
    Has<Pressed>,
    &'static Hovered,
    &'static InteractiveStyle,
    &'static mut BackgroundColor,
    Option<&'static mut BorderColor>,
);

/// Re-colour interactive widgets whose state changed this frame.
///
/// Bevy 0.20 replaced the polled `Interaction` enum with two components:
/// `Pressed`, which `ui_widgets::Button` inserts on pointer-down and removes
/// on release, and picking's `Hovered(bool)`, which is re-inserted whenever
/// hover changes (it is immutable, so `Changed<Hovered>` catches it). Only
/// entities that actually changed are written to, because UI rendering is
/// retained now and every write re-extracts the node.
pub fn update_interactive_visuals(
    changed: Query<Entity, InteractionChanged>,
    mut released: RemovedComponents<Pressed>,
    mut widgets: Query<InteractiveWidget>,
) {
    for entity in changed.iter().chain(released.read()) {
        let Ok((pressed, hovered, style, mut bg, border)) = widgets.get_mut(entity) else {
            continue;
        };
        let (target_bg, target_border) = if pressed {
            (style.pressed_bg, style.pressed_border)
        } else if hovered.0 {
            (style.hover_bg, style.hover_border)
        } else {
            (style.idle_bg, style.idle_border)
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
            Hovered::default(),
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
