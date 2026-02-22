use bevy::prelude::*;

use crate::game::GameEntity;

#[derive(Component)]
pub struct DynamicBackground {
    pub padding: f32,
    pub min_size: Option<Vec2>,
    pub material: StandardMaterial,
}

pub struct DisplayBuilder {
    position: DisplayPosition,
    background_color: Color,
    padding: f32,
}

pub enum DisplayPosition {
    TopLeft { top: f32, left: f32 },
    TopRight { top: f32, right: f32 },
}

impl DisplayBuilder {
    pub fn new() -> Self {
        Self {
            position: DisplayPosition::TopRight {
                top: 20.0,
                right: 20.0,
            },
            background_color: Color::srgba(0.0, 0.0, 0.0, 0.7),
            padding: 12.0,
        }
    }

    pub fn position(mut self, position: DisplayPosition) -> Self {
        self.position = position;
        self
    }

    pub fn background_color(mut self, color: Color) -> Self {
        self.background_color = color;
        self
    }

    pub fn padding(mut self, padding: f32) -> Self {
        self.padding = padding;
        self
    }

    pub fn build<M: Component>(
        self,
        commands: &mut Commands,
        marker: M,
        content_builder: impl FnOnce(&mut ChildSpawnerCommands),
    ) -> Entity {
        let mut node = Node {
            position_type: PositionType::Absolute,
            flex_direction: FlexDirection::Column,
            padding: UiRect::all(Val::Px(self.padding)),
            ..default()
        };

        match self.position {
            DisplayPosition::TopLeft { top, left } => {
                node.top = Val::Px(top);
                node.left = Val::Px(left);
            }
            DisplayPosition::TopRight { top, right } => {
                node.top = Val::Px(top);
                node.right = Val::Px(right);
            }
        }

        commands
            .spawn((
                GameEntity,
                marker,
                node,
                BackgroundColor(self.background_color),
            ))
            .with_children(content_builder)
            .id()
    }
}
