//! Single-line text input widget.
//!
//! Spawn one with [`UiExt::ui_text_input`]. Click to focus, type to edit.
//! Backspace deletes the previous character. Enter and Escape clear focus.
//!
//! Read the current value by querying for [`TextInput`] together with whatever
//! marker component you attached when spawning.

use bevy::input::keyboard::{Key, KeyboardInput};
use bevy::{picking::hover::Hovered, prelude::*, ui::Pressed, ui_widgets::Button};

use super::theme::UiTheme;

// ---------------------------------------------------------------------------
// Components
// ---------------------------------------------------------------------------

/// State for a text input. The current value lives on this component.
#[derive(Component, Debug, Default)]
pub struct TextInput {
    pub value: String,
    pub placeholder: String,
}

/// Marker for the `Text` child node owned by a [`TextInput`]. The display
/// system updates the text on this entity whenever the parent's value changes.
#[derive(Component)]
pub struct TextInputDisplay;

/// Tag added to the currently-focused input. At most one input is focused at
/// a time across the whole app.
#[derive(Component)]
pub struct TextInputFocus;

// ---------------------------------------------------------------------------
// Widget builder
// ---------------------------------------------------------------------------

/// Spawn a text input under the current parent.
///
/// `marker` is your domain-specific component (e.g. `IdInput`, `WorldInput`)
/// that lets you find this input later when reading its value.
pub fn spawn_text_input<M: Component>(
    parent: &mut bevy::ecs::relationship::RelatedSpawnerCommands<ChildOf>,
    label: &str,
    initial: &str,
    placeholder: &str,
    marker: M,
    theme: &UiTheme,
) -> Entity {
    parent
        .spawn(Node {
            width: Val::Px(theme.btn_width * 1.5),
            height: Val::Px(theme.btn_height),
            flex_direction: FlexDirection::Column,
            margin: UiRect::bottom(Val::Px(theme.pad_md)),
            ..default()
        })
        .with_children(|col| {
            col.spawn((
                Text::new(label),
                TextFont {
                    font_size: FontSize::Px(theme.size_label),
                    ..default()
                },
                TextColor(theme.text_dim),
                Node {
                    margin: UiRect::bottom(Val::Px(theme.pad_sm)),
                    ..default()
                },
            ));
            col.spawn((
                Button,
                Hovered::default(),
                Node {
                    width: Val::Percent(100.0),
                    height: Val::Px(36.0),
                    border: UiRect::all(Val::Px(2.0)),
                    border_radius: BorderRadius::all(Val::Px(4.0)),
                    align_items: AlignItems::Center,
                    padding: UiRect::horizontal(Val::Px(theme.pad_md)),
                    ..default()
                },
                BorderColor::all(theme.border),
                BackgroundColor(theme.bg_surface),
                TextInput {
                    value: initial.to_owned(),
                    placeholder: placeholder.to_owned(),
                },
                marker,
            ))
            .with_children(|input| {
                input.spawn((
                    Text::new(if initial.is_empty() {
                        placeholder
                    } else {
                        initial
                    }),
                    TextFont {
                        font_size: FontSize::Px(theme.size_body),
                        ..default()
                    },
                    TextColor(if initial.is_empty() {
                        theme.text_dim
                    } else {
                        theme.text
                    }),
                    TextInputDisplay,
                ));
            });
        })
        .id()
}

// ---------------------------------------------------------------------------
// Systems
// ---------------------------------------------------------------------------

/// Plugin wiring for text-input systems. Always-on (focus state is cleared
/// automatically when no input is interacted with).
pub struct TextInputPlugin;

impl Plugin for TextInputPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(
            Update,
            (
                update_focus_on_click,
                apply_keyboard,
                sync_display,
                sync_focus_border,
            )
                .chain(),
        );
    }
}

/// Click-to-focus / click-elsewhere-to-unfocus. We only react to `Pressed`
/// events on entities that have a `TextInput` component, so clicks on other
/// buttons leave focus alone — call [`clear_focus`] from your screen's
/// cleanup if you want focus released on screen exit.
fn update_focus_on_click(
    mut commands: Commands,
    pressed: Query<Entity, (Added<Pressed>, With<TextInput>)>,
    focused: Query<Entity, With<TextInputFocus>>,
) {
    for entity in &pressed {
        for old in &focused {
            commands.entity(old).remove::<TextInputFocus>();
        }
        commands.entity(entity).insert(TextInputFocus);
    }
}

/// Append/delete characters on the focused input.
fn apply_keyboard(
    mut commands: Commands,
    mut events: MessageReader<KeyboardInput>,
    mut focused: Query<(Entity, &mut TextInput), With<TextInputFocus>>,
) {
    let Ok((entity, mut input)) = focused.single_mut() else {
        events.clear();
        return;
    };
    for ev in events.read() {
        if !ev.state.is_pressed() {
            continue;
        }
        match &ev.logical_key {
            Key::Character(s) => {
                for ch in s.chars() {
                    if !ch.is_control() {
                        input.value.push(ch);
                    }
                }
            }
            Key::Space => input.value.push(' '),
            Key::Backspace => {
                input.value.pop();
            }
            Key::Enter | Key::Escape => {
                commands.entity(entity).remove::<TextInputFocus>();
            }
            _ => {}
        }
    }
}

/// Mirror `TextInput.value` (or its placeholder) into the child `Text` node.
fn sync_display(
    inputs: Query<(&TextInput, &Children), Changed<TextInput>>,
    mut texts: Query<(&mut Text, &mut TextColor), With<TextInputDisplay>>,
    theme: Res<UiTheme>,
) {
    for (input, children) in &inputs {
        for child in children.iter() {
            if let Ok((mut text, mut color)) = texts.get_mut(child) {
                if input.value.is_empty() {
                    *text = Text::new(input.placeholder.clone());
                    color.0 = theme.text_dim;
                } else {
                    *text = Text::new(input.value.clone());
                    color.0 = theme.text;
                }
            }
        }
    }
}

/// Convenience: drop focus from any currently-focused input. Call from
/// `OnExit` of screens that contain text inputs.
pub fn clear_focus(mut commands: Commands, focused: Query<Entity, With<TextInputFocus>>) {
    for e in &focused {
        commands.entity(e).remove::<TextInputFocus>();
    }
}

/// Color the border of the focused input differently so the user can see
/// where keystrokes will land.
fn sync_focus_border(
    mut inputs: Query<(&mut BorderColor, Has<TextInputFocus>), With<TextInput>>,
    theme: Res<UiTheme>,
) {
    for (mut border, focused) in &mut inputs {
        let target = if focused {
            theme.border_strong
        } else {
            theme.border
        };
        if border.top != target {
            *border = BorderColor::all(target);
        }
    }
}
