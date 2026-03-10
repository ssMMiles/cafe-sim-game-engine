use bevy::prelude::*;

/// Persistent anchor nodes for attaching UI panels. These are set up once at
/// startup and are NOT marked `GameEntity`, so they survive game state changes.
/// Individual panels spawned as children should use `GameEntity` for cleanup.
#[derive(Resource)]
pub struct UiAnchors {
    pub top_left: Entity,
    pub top_right: Entity,
    pub bottom: Entity,
}

pub fn setup_ui_anchors(mut commands: Commands) {
    let top_left = commands
        .spawn(Node {
            position_type: PositionType::Absolute,
            top: Val::Px(28.0),
            left: Val::Px(28.0),
            flex_direction: FlexDirection::Column,
            align_items: AlignItems::FlexStart,
            row_gap: Val::Px(8.0),
            ..default()
        })
        .id();

    let top_right = commands
        .spawn(Node {
            position_type: PositionType::Absolute,
            top: Val::Px(28.0),
            right: Val::Px(28.0),
            flex_direction: FlexDirection::Column,
            row_gap: Val::Px(8.0),
            align_items: AlignItems::FlexEnd,
            ..default()
        })
        .id();

    let bottom = commands
        .spawn(Node {
            position_type: PositionType::Absolute,
            bottom: Val::Px(0.0),
            left: Val::Px(0.0),
            right: Val::Px(0.0),
            flex_direction: FlexDirection::Row,
            ..default()
        })
        .id();

    commands.insert_resource(UiAnchors {
        top_left,
        top_right,
        bottom,
    });
}
