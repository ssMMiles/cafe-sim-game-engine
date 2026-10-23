//! Collapsible dropdown / disclosure widget.
//!
//! Spawn one with [`spawn_dropdown`]. The closure populates the body — its
//! contents are hidden via `Display::None` when the dropdown is collapsed so
//! surrounding layout reflows.

use bevy::{ecs::relationship::RelatedSpawnerCommands, prelude::*};

use super::theme::UiTheme;
use super::widgets::InteractiveStyle;

#[derive(Component, Debug)]
pub struct Dropdown {
    pub open: bool,
}

#[derive(Component)]
struct DropdownHeader;

#[derive(Component)]
struct DropdownChevron;

#[derive(Component)]
struct DropdownContent;

const CHEVRON_OPEN: &str = "\u{25BE}"; // ▾
const CHEVRON_CLOSED: &str = "\u{25B8}"; // ▸

pub fn spawn_dropdown<M: Component>(
    parent: &mut RelatedSpawnerCommands<ChildOf>,
    label: &str,
    initial_open: bool,
    marker: M,
    theme: &UiTheme,
    content: impl FnOnce(&mut RelatedSpawnerCommands<ChildOf>),
) -> Entity {
    let header_style = InteractiveStyle {
        idle_bg: Color::NONE,
        hover_bg: theme.bg_surface_hover,
        pressed_bg: theme.bg_panel,
        idle_border: Color::NONE,
        hover_border: Color::NONE,
        pressed_border: Color::NONE,
    };
    parent
        .spawn((
            Node {
                width: Val::Px(theme.btn_width * 1.5),
                flex_direction: FlexDirection::Column,
                margin: UiRect::bottom(Val::Px(theme.pad_sm)),
                padding: UiRect::all(Val::Px(theme.pad_sm)),
                border: UiRect::all(Val::Px(1.0)),
                border_radius: BorderRadius::all(Val::Px(6.0)),
                ..default()
            },
            BackgroundColor(theme.bg_surface),
            BorderColor::all(theme.border),
            Dropdown { open: initial_open },
            marker,
        ))
        .with_children(|root| {
            root.spawn((
                Button,
                Node {
                    width: Val::Percent(100.0),
                    height: Val::Px(30.0),
                    align_items: AlignItems::Center,
                    padding: UiRect::horizontal(Val::Px(theme.pad_sm)),
                    border_radius: BorderRadius::all(Val::Px(4.0)),
                    ..default()
                },
                BackgroundColor(header_style.idle_bg),
                header_style,
                DropdownHeader,
            ))
            .with_children(|header| {
                header.spawn((
                    Text::new(if initial_open {
                        CHEVRON_OPEN
                    } else {
                        CHEVRON_CLOSED
                    }),
                    TextFont {
                        font_size: theme.size_body,
                        ..default()
                    },
                    TextColor(theme.text_dim),
                    Node {
                        width: Val::Px(16.0),
                        margin: UiRect::right(Val::Px(theme.pad_sm)),
                        ..default()
                    },
                    DropdownChevron,
                ));
                header.spawn((
                    Text::new(label),
                    TextFont {
                        font_size: theme.size_body,
                        ..default()
                    },
                    TextColor(theme.text),
                ));
            });

            let mut body = root.spawn((
                Node {
                    flex_direction: FlexDirection::Column,
                    align_items: AlignItems::Center,
                    width: Val::Percent(100.0),
                    padding: UiRect::top(Val::Px(theme.pad_sm)),
                    display: if initial_open {
                        Display::Flex
                    } else {
                        Display::None
                    },
                    ..default()
                },
                DropdownContent,
            ));
            body.with_children(|body| content(body));
        })
        .id()
}

pub struct DropdownPlugin;

impl Plugin for DropdownPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(
            Update,
            (
                handle_dropdown_clicks,
                sync_dropdown_visibility,
                sync_dropdown_chevron,
            )
                .chain(),
        );
    }
}

fn handle_dropdown_clicks(
    headers: Query<(&Interaction, &ChildOf), (Changed<Interaction>, With<DropdownHeader>)>,
    mut dropdowns: Query<&mut Dropdown>,
) {
    for (interaction, parent) in &headers {
        if *interaction != Interaction::Pressed {
            continue;
        }
        if let Ok(mut dropdown) = dropdowns.get_mut(parent.parent()) {
            dropdown.open = !dropdown.open;
        }
    }
}

fn sync_dropdown_visibility(
    dropdowns: Query<(&Dropdown, &Children), Changed<Dropdown>>,
    mut content: Query<&mut Node, With<DropdownContent>>,
) {
    for (dropdown, children) in &dropdowns {
        for child in children.iter() {
            if let Ok(mut node) = content.get_mut(child) {
                node.display = if dropdown.open {
                    Display::Flex
                } else {
                    Display::None
                };
            }
        }
    }
}

fn sync_dropdown_chevron(
    dropdowns: Query<(&Dropdown, &Children), Changed<Dropdown>>,
    headers: Query<&Children, With<DropdownHeader>>,
    mut chevrons: Query<&mut Text, With<DropdownChevron>>,
) {
    for (dropdown, top_children) in &dropdowns {
        for top_child in top_children.iter() {
            let Ok(header_children) = headers.get(top_child) else {
                continue;
            };
            for header_child in header_children.iter() {
                if let Ok(mut text) = chevrons.get_mut(header_child) {
                    *text = Text::new(if dropdown.open {
                        CHEVRON_OPEN
                    } else {
                        CHEVRON_CLOSED
                    });
                }
            }
        }
    }
}
