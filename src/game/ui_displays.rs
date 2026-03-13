use bevy::{
    diagnostic::{DiagnosticsStore, FrameTimeDiagnosticsPlugin},
    ecs::relationship::RelatedSpawnerCommands,
    prelude::*,
};

use super::GameEntity;
use super::{
    gfx::camera::PlayerCamera,
    gltf_loading::{HoveredOver, Interactable},
};
use crate::{
    settings::UserSettings,
    ui::{UiAnchors, UiTheme},
};

// ── Marker components ─────────────────────────────────────────────────────────

#[derive(Component)]
pub struct FpsDisplay;

#[derive(Component)]
pub struct KeybindsDisplay;

#[derive(Component)]
pub struct InteractionInfoDisplay;

#[derive(Component)]
pub struct PositionDisplay;

// ── FPS Display ───────────────────────────────────────────────────────────────

pub fn setup_fps_display(mut commands: Commands, anchors: Res<UiAnchors>, theme: Res<UiTheme>) {
    let entity = commands
        .spawn((
            GameEntity,
            FpsDisplay,
            Text::new("FPS: ..."),
            TextFont {
                font_size: theme.size_body,
                ..default()
            },
            TextColor(theme.text_white),
            Node {
                padding: UiRect::all(Val::Px(theme.pad_md)),
                min_width: Val::Px(80.0),
                ..default()
            },
            BackgroundColor(theme.bg_panel),
        ))
        .id();
    commands.entity(anchors.top_left).add_child(entity);
}

pub fn update_fps_display(
    diagnostics: Res<DiagnosticsStore>,
    mut query: Query<&mut Text, With<FpsDisplay>>,
    mut last_fps: Local<u32>,
) {
    let Some(diag) = diagnostics.get(&FrameTimeDiagnosticsPlugin::FPS) else {
        return;
    };
    let Some(smoothed) = diag.smoothed() else {
        return;
    };
    let fps = smoothed.floor() as u32;
    if fps == *last_fps {
        return;
    }
    *last_fps = fps;

    for mut text in query.iter_mut() {
        **text = format!("FPS: {fps}");
    }
}

// ── Keybinds Display ──────────────────────────────────────────────────────────

pub fn setup_keybinds_display(
    mut commands: Commands,
    settings: Res<UserSettings>,
    anchors: Res<UiAnchors>,
    theme: Res<UiTheme>,
) {
    let entity = spawn_keybinds_panel(&mut commands, &settings, &theme);
    commands.entity(anchors.top_right).add_child(entity);
}

pub fn update_keybinds_display(
    mut commands: Commands,
    settings: Res<UserSettings>,
    theme: Res<UiTheme>,
    query: Query<Entity, With<KeybindsDisplay>>,
) {
    if !settings.is_changed() {
        return;
    }

    for entity in query.iter() {
        let settings_info = settings.get_settings_info();
        commands.entity(entity).despawn_related::<Children>();
        commands.entity(entity).with_children(|parent| {
            spawn_keybinds_children(parent, &settings_info, &theme);
        });
    }
}

fn spawn_keybinds_panel(
    commands: &mut Commands,
    settings: &UserSettings,
    theme: &UiTheme,
) -> Entity {
    let settings_info = settings.get_settings_info();
    commands
        .spawn((
            GameEntity,
            KeybindsDisplay,
            Node {
                flex_direction: FlexDirection::Column,
                padding: UiRect::all(Val::Px(theme.pad_md)),
                ..default()
            },
            BackgroundColor(theme.bg_panel),
        ))
        .with_children(|parent| {
            spawn_keybinds_children(parent, &settings_info, &theme);
        })
        .id()
}

fn spawn_keybinds_children(
    parent: &mut RelatedSpawnerCommands<'_, ChildOf>,
    settings_info: &[crate::settings::SettingInfo],
    theme: &UiTheme,
) {
    parent.spawn((
        Text::new("Controls"),
        TextFont {
            font_size: theme.size_body,
            ..default()
        },
        TextColor(theme.text_white),
        Node {
            margin: UiRect::bottom(Val::Px(theme.pad_sm)),
            ..default()
        },
    ));

    parent
        .spawn((Node {
            flex_direction: FlexDirection::Column,
            row_gap: Val::Px(theme.pad_sm),
            ..default()
        },))
        .with_children(|col| {
            for setting in settings_info {
                col.spawn((Node {
                    flex_direction: FlexDirection::Row,
                    justify_content: JustifyContent::SpaceBetween,
                    width: Val::Px(200.0),
                    ..default()
                },))
                    .with_children(|row| {
                        row.spawn((
                            Text::new(format!("{} [{}]:", setting.name, setting.keybind)),
                            TextFont {
                                font_size: theme.size_label,
                                ..default()
                            },
                            TextColor(theme.text_dim),
                        ));
                        row.spawn((
                            Text::new(setting.current_value.clone()),
                            TextFont {
                                font_size: theme.size_label,
                                ..default()
                            },
                            TextColor(theme.text_accent),
                        ));
                    });
            }
        });
}

// ── Interaction Info Display ───────────────────────────────────────────────────

pub fn setup_interaction_info_display(
    mut commands: Commands,
    anchors: Res<UiAnchors>,
    theme: Res<UiTheme>,
) {
    let entity = commands
        .spawn((
            GameEntity,
            InteractionInfoDisplay,
            Text::new("No interaction"),
            TextFont {
                font_size: theme.size_label,
                ..default()
            },
            TextColor(theme.text_dim),
            Node {
                padding: UiRect::all(Val::Px(theme.pad_md)),
                ..default()
            },
            BackgroundColor(theme.bg_panel),
        ))
        .id();
    commands.entity(anchors.top_right).add_child(entity);
}

pub fn update_interaction_info_display(
    mut query: Query<(&mut Text, &mut TextColor), With<InteractionInfoDisplay>>,
    interactable_query: Query<&Interactable, With<HoveredOver>>,
    theme: Res<UiTheme>,
) {
    let current = interactable_query.iter().next();
    for (mut text, mut color) in query.iter_mut() {
        match current {
            Some(interactable) => {
                **text = format!("Interacting: {}", interactable.name);
                color.0 = theme.positive;
            }
            None => {
                **text = "No interaction".to_string();
                color.0 = theme.text_dim;
            }
        }
    }
}

// ── Position Display ──────────────────────────────────────────────────────────

pub fn setup_position_display(
    mut commands: Commands,
    anchors: Res<UiAnchors>,
    theme: Res<UiTheme>,
) {
    let entity = commands
        .spawn((
            GameEntity,
            PositionDisplay,
            Text::new("X: 0.0\nY: 0.0\nZ: 0.0"),
            TextFont {
                font_size: theme.size_caption,
                ..default()
            },
            TextColor(theme.text_dim),
            Node {
                padding: UiRect::all(Val::Px(theme.pad_md)),
                ..default()
            },
            BackgroundColor(theme.bg_panel),
        ))
        .id();
    commands.entity(anchors.top_right).add_child(entity);
}

pub fn update_position_display(
    mut query: Query<&mut Text, With<PositionDisplay>>,
    camera_query: Query<&Transform, (With<PlayerCamera>, Changed<Transform>)>,
) {
    let Ok(transform) = camera_query.single() else {
        return;
    };
    let pos = transform.translation;
    for mut text in query.iter_mut() {
        **text = format!("X: {:.1}\nY: {:.1}\nZ: {:.1}", pos.x, pos.y, pos.z);
    }
}
