//! Toggleable checkbox widget. Click to flip [`Checkbox::checked`].
//!
//! Spawn one with [`spawn_checkbox`] and read its state by querying for the
//! `Checkbox` component together with whatever marker you attached.

use bevy::prelude::*;

use super::theme::UiTheme;
use super::widgets::InteractiveStyle;

#[derive(Component, Debug)]
pub struct Checkbox {
    pub checked: bool,
}

#[derive(Component)]
struct CheckboxLabel;

#[derive(Component)]
struct CheckboxBox;

pub fn spawn_checkbox<M: Component>(
    parent: &mut bevy::ecs::relationship::RelatedSpawnerCommands<ChildOf>,
    label: &str,
    checked: bool,
    marker: M,
    theme: &UiTheme,
) -> Entity {
    let row_style = InteractiveStyle::surface(theme);
    parent
        .spawn((
            Button,
            Node {
                width: Val::Px(theme.btn_width * 1.5),
                height: Val::Px(32.0),
                align_items: AlignItems::Center,
                padding: UiRect::horizontal(Val::Px(theme.pad_md)),
                margin: UiRect::bottom(Val::Px(4.0)),
                border: UiRect::all(Val::Px(1.0)),
                border_radius: BorderRadius::all(Val::Px(4.0)),
                ..default()
            },
            BackgroundColor(row_style.idle_bg),
            BorderColor::all(row_style.idle_border),
            row_style,
            Checkbox { checked },
            marker,
        ))
        .with_children(|row| {
            row.spawn((
                Node {
                    width: Val::Px(16.0),
                    height: Val::Px(16.0),
                    margin: UiRect::right(Val::Px(theme.pad_sm)),
                    border: UiRect::all(Val::Px(2.0)),
                    border_radius: BorderRadius::all(Val::Px(3.0)),
                    ..default()
                },
                BorderColor::all(theme.text_dim),
                BackgroundColor(if checked { theme.positive } else { Color::NONE }),
                CheckboxBox,
            ));
            row.spawn((
                Text::new(label),
                TextFont {
                    font_size: FontSize::Px(theme.size_body),
                    ..default()
                },
                TextColor(theme.text),
                CheckboxLabel,
            ));
        })
        .id()
}

pub struct CheckboxPlugin;

impl Plugin for CheckboxPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, (handle_checkbox_clicks, sync_box_color).chain());
    }
}

fn handle_checkbox_clicks(
    mut interactions: Query<(&Interaction, &mut Checkbox), (Changed<Interaction>, With<Button>)>,
) {
    for (interaction, mut checkbox) in &mut interactions {
        if *interaction == Interaction::Pressed {
            checkbox.checked = !checkbox.checked;
        }
    }
}

fn sync_box_color(
    checkboxes: Query<(&Checkbox, &Children), Changed<Checkbox>>,
    mut boxes: Query<(&mut BackgroundColor, &mut BorderColor), With<CheckboxBox>>,
    theme: Res<UiTheme>,
) {
    for (checkbox, children) in &checkboxes {
        for child in children.iter() {
            if let Ok((mut bg, mut border)) = boxes.get_mut(child) {
                if checkbox.checked {
                    bg.0 = theme.positive;
                    *border = BorderColor::all(theme.positive);
                } else {
                    bg.0 = Color::NONE;
                    *border = BorderColor::all(theme.text_dim);
                }
            }
        }
    }
}
