use bevy::{ecs::relationship::RelatedSpawnerCommands, prelude::*};

use super::theme::UiTheme;

#[derive(Clone, Copy)]
pub enum ButtonVariant {
    Primary,
    Danger,
}

impl ButtonVariant {
    fn color(&self, theme: &UiTheme) -> Color {
        match self {
            ButtonVariant::Primary => theme.primary,
            ButtonVariant::Danger => theme.danger,
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
    fn ui_slider<C: Component, L: Component, T: Component>(
        &mut self,
        label: &str,
        value: f32,
        min: f32,
        max: f32,
        action: C,
        label_component: L,
        track_component: T,
        theme: &UiTheme,
    ) -> Entity;
}

/// Calculates the left-percent position for a slider handle given a normalized value [0,1].
fn handle_left_percent(normalized_value: f32, track_width: f32, handle_size: f32) -> f32 {
    let usable_width = track_width - handle_size;
    let handle_left = normalized_value * usable_width;
    (handle_left / track_width) * 100.0
}

impl UiExt for RelatedSpawnerCommands<'_, ChildOf> {
    fn ui_title(&mut self, text: &str, theme: &UiTheme) -> Entity {
        self.spawn((
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
        self.spawn((
            Button,
            Node {
                width: Val::Px(theme.btn_width),
                height: Val::Px(theme.btn_height),
                border: UiRect::all(Val::Px(theme.btn_border)),
                justify_content: JustifyContent::Center,
                align_items: AlignItems::Center,
                margin: UiRect::bottom(Val::Px(theme.pad_lg)),
                ..default()
            },
            BackgroundColor(variant.color(theme)),
            action,
        ))
        .with_child((Text::new(text), TextColor(theme.text)))
        .id()
    }

    fn ui_slider<C: Component, L: Component, T: Component>(
        &mut self,
        label: &str,
        value: f32,
        min: f32,
        max: f32,
        action: C,
        label_component: L,
        track_component: T,
        theme: &UiTheme,
    ) -> Entity {
        let normalized_value = (value - min) / (max - min);
        let handle_left = handle_left_percent(
            normalized_value,
            theme.slider_track_width,
            theme.slider_handle_size,
        );

        self.spawn((Node {
            width: Val::Px(theme.slider_track_width),
            height: Val::Px(80.0),
            flex_direction: FlexDirection::Column,
            margin: UiRect::bottom(Val::Px(theme.pad_lg)),
            ..default()
        },))
            .with_children(|parent| {
                parent.spawn((
                    Text::new(format!("{}: {:.3}", label, value)),
                    TextFont {
                        font_size: theme.size_body,
                        ..default()
                    },
                    TextColor(theme.text),
                    Node {
                        margin: UiRect::bottom(Val::Px(theme.pad_sm)),
                        ..default()
                    },
                    label_component,
                ));

                parent
                    .spawn((Node {
                        width: Val::Percent(100.0),
                        height: Val::Px(30.0),
                        align_items: AlignItems::Center,
                        ..default()
                    },))
                    .with_children(|slider_parent| {
                        slider_parent.spawn((
                            Button,
                            Node {
                                width: Val::Percent(100.0),
                                height: Val::Px(theme.slider_track_height),
                                ..default()
                            },
                            BackgroundColor(theme.bg_surface),
                            track_component,
                        ));

                        slider_parent.spawn((
                            Button,
                            Node {
                                width: Val::Px(theme.slider_handle_size),
                                height: Val::Px(theme.slider_handle_size),
                                position_type: PositionType::Absolute,
                                left: Val::Percent(handle_left),
                                border: UiRect::all(Val::Px(2.0)),
                                ..default()
                            },
                            BackgroundColor(theme.primary),
                            action,
                        ));
                    });
            })
            .id()
    }
}
