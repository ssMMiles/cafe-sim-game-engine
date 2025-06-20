use bevy::{
    diagnostic::{DiagnosticsStore, FrameTimeDiagnosticsPlugin},
    log,
    prelude::*,
    render::primitives::Aabb,
};

use super::interaction::{HoveredOver, Interactable};
use super::{GameEntity, camera::PlayerCamera};
use crate::settings::UserSettings;

// Display components
#[derive(Component)]
pub struct FpsDisplay;

#[derive(Component)]
pub struct KeybindsDisplay;

#[derive(Component)]
pub struct InteractionInfoDisplay;

#[derive(Component)]
pub struct PositionDisplay;

#[derive(Component)]
pub struct DynamicBackground {
    pub padding: f32,
    pub min_size: Option<Vec2>,
    pub material: StandardMaterial,
}

// Generic display builder
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

// FPS Display
pub fn setup_fps_display(mut commands: Commands) {
    create_fps_display(&mut commands, "...");
}

pub fn update_fps_display(
    mut commands: Commands,
    diagnostics: Res<DiagnosticsStore>,
    fps_query: Query<Entity, With<FpsDisplay>>,
) {
    // Remove existing FPS display
    for entity in fps_query.iter() {
        commands.entity(entity).despawn();
    }

    // Get FPS from diagnostics
    let fps = if let Some(fps_diagnostic) = diagnostics.get(&FrameTimeDiagnosticsPlugin::FPS) {
        if let Some(fps_smoothed) = fps_diagnostic.smoothed() {
            format!("{}", fps_smoothed.floor() as u32)
        } else {
            "...".to_string()
        }
    } else {
        "...".to_string()
    };

    // Recreate FPS display
    create_fps_display(&mut commands, &fps);
}

fn create_fps_display(commands: &mut Commands, fps: &str) {
    DisplayBuilder::new()
        .position(DisplayPosition::TopLeft {
            top: 20.0,
            left: 20.0,
        })
        .build(commands, FpsDisplay, |parent| {
            parent.spawn((
                Text::new(format!("FPS: {}", fps)),
                TextFont {
                    font_size: 16.0,
                    ..default()
                },
                TextColor(Color::srgb(1.0, 1.0, 1.0)),
            ));
        });
}

// Keybinds Display
pub fn setup_keybinds_display(mut commands: Commands, settings: Res<UserSettings>) {
    create_keybinds_display(&mut commands, &settings);
}

pub fn update_keybinds_display(
    mut commands: Commands,
    settings: Res<UserSettings>,
    keybinds_query: Query<Entity, With<KeybindsDisplay>>,
) {
    if settings.is_changed() {
        // Remove existing keybinds display
        for entity in keybinds_query.iter() {
            commands.entity(entity).despawn();
        }

        // Recreate keybinds display with updated values
        create_keybinds_display(&mut commands, &settings);
    }
}

fn create_keybinds_display(commands: &mut Commands, settings: &UserSettings) {
    let settings_info = settings.get_settings_info();

    DisplayBuilder::new()
        .position(DisplayPosition::TopRight {
            top: 20.0,
            right: 20.0,
        })
        .build(commands, KeybindsDisplay, |parent| {
            // Title
            parent.spawn((
                Text::new("Controls"),
                TextFont {
                    font_size: 16.0,
                    ..default()
                },
                TextColor(Color::srgb(1.0, 1.0, 1.0)),
            ));

            parent
                .spawn((Node {
                    flex_direction: FlexDirection::Column,
                    row_gap: Val::Px(8.0),
                    ..default()
                },))
                .with_children(|settings_parent| {
                    // Add each setting
                    for setting in settings_info {
                        settings_parent
                            .spawn((Node {
                                flex_direction: FlexDirection::Row,
                                justify_content: JustifyContent::SpaceBetween,
                                width: Val::Px(200.0),
                                ..default()
                            },))
                            .with_children(|row| {
                                // Setting name + keybind
                                row.spawn((
                                    Text::new(format!("{} [{}]:", setting.name, setting.keybind)),
                                    TextFont {
                                        font_size: 14.0,
                                        ..default()
                                    },
                                    TextColor(Color::srgb(0.8, 0.8, 0.8)),
                                ));

                                // Current value
                                row.spawn((
                                    Text::new(&setting.current_value),
                                    TextFont {
                                        font_size: 14.0,
                                        ..default()
                                    },
                                    TextColor(Color::srgb(0.6, 0.8, 1.0)),
                                ));
                            });
                    }
                });
        });
}

// Interaction Info Display
pub fn setup_interaction_info_display(mut commands: Commands) {
    create_interaction_info_display(&mut commands, None);
}

pub fn update_interaction_info_display(
    mut commands: Commands,
    interaction_info_query: Query<Entity, With<InteractionInfoDisplay>>,
    interactable_query: Query<&Interactable, With<HoveredOver>>,
) {
    // Remove existing interaction info display
    for entity in interaction_info_query.iter() {
        commands.entity(entity).despawn();
    }

    // Get current interaction
    let current_interaction = interactable_query.iter().next();

    // Recreate interaction info display
    create_interaction_info_display(&mut commands, current_interaction);
}

fn create_interaction_info_display(commands: &mut Commands, interaction: Option<&Interactable>) {
    let interaction_text = match interaction {
        Some(interactable) => format!("Interacting: {}", interactable.name),
        None => "No interaction".to_string(),
    };

    DisplayBuilder::new()
        .position(DisplayPosition::TopRight {
            top: 180.0,
            right: 20.0,
        })
        .build(commands, InteractionInfoDisplay, |parent| {
            parent.spawn((
                Text::new(interaction_text),
                TextFont {
                    font_size: 14.0,
                    ..default()
                },
                TextColor(if interaction.is_some() {
                    Color::srgb(0.4, 1.0, 0.4) // Green when interacting
                } else {
                    Color::srgb(0.8, 0.8, 0.8) // Gray when not interacting
                }),
            ));
        });
}

// Position Display
pub fn setup_position_display(mut commands: Commands) {
    create_position_display(&mut commands, Vec3::ZERO);
}

pub fn update_position_display(
    mut commands: Commands,
    position_query: Query<Entity, With<PositionDisplay>>,
    camera_query: Query<&Transform, With<PlayerCamera>>,
) {
    // Remove existing position display
    for entity in position_query.iter() {
        commands.entity(entity).despawn();
    }

    // Get current camera position
    let camera_position = if let Ok(transform) = camera_query.single() {
        transform.translation
    } else {
        Vec3::ZERO
    };

    // Recreate position display
    create_position_display(&mut commands, camera_position);
}

fn create_position_display(commands: &mut Commands, position: Vec3) {
    DisplayBuilder::new()
        .position(DisplayPosition::TopRight {
            top: 240.0,
            right: 20.0,
        })
        .build(commands, PositionDisplay, |parent| {
            // Title
            parent.spawn((
                Text::new("Position"),
                TextFont {
                    font_size: 14.0,
                    ..default()
                },
                TextColor(Color::srgb(1.0, 1.0, 1.0)),
            ));

            parent
                .spawn((Node {
                    flex_direction: FlexDirection::Column,
                    row_gap: Val::Px(4.0),
                    ..default()
                },))
                .with_children(|coords_parent| {
                    // Position coordinates
                    coords_parent.spawn((
                        Text::new(format!("X: {:.1}", position.x)),
                        TextFont {
                            font_size: 12.0,
                            ..default()
                        },
                        TextColor(Color::srgb(0.8, 0.8, 0.8)),
                    ));

                    coords_parent.spawn((
                        Text::new(format!("Y: {:.1}", position.y)),
                        TextFont {
                            font_size: 12.0,
                            ..default()
                        },
                        TextColor(Color::srgb(0.8, 0.8, 0.8)),
                    ));

                    coords_parent.spawn((
                        Text::new(format!("Z: {:.1}", position.z)),
                        TextFont {
                            font_size: 12.0,
                            ..default()
                        },
                        TextColor(Color::srgb(0.8, 0.8, 0.8)),
                    ));
                });
        });
}

// Update dynamic backgrounds system
pub fn update_dynamic_backgrounds(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    dynamic_bg_query: Query<(Entity, &DynamicBackground, &Children)>,
    text_query: Query<(&Node, &ComputedNode, &Text)>,
    mesh_query: Query<(&Transform, &Aabb), With<Mesh3d>>,
) {
    for (background_entity, dynamic_bg, children) in dynamic_bg_query.iter() {
        let mut min_bounds = Vec2::new(f32::INFINITY, f32::INFINITY);
        let mut max_bounds = Vec2::new(f32::NEG_INFINITY, f32::NEG_INFINITY);
        let mut has_content = false;

        log::debug!(
            "Processing DynamicBackground entity {:?} with {} children",
            background_entity,
            children.len()
        );

        // Calculate bounds of all text children
        for child in children.iter() {
            if let Ok((_, computed_node, _)) = text_query.get(child) {
                has_content = true;
                let size = computed_node.size();

                if size.x > 0.0 && size.y > 0.0 {
                    min_bounds = min_bounds.min(Vec2::ZERO);
                    max_bounds = max_bounds.max(size);
                }
            }

            // Also check for mesh children with Aabb
            if let Ok((transform, aabb)) = mesh_query.get(child) {
                has_content = true;

                // Calculate the local bounds from Aabb
                let aabb_min = aabb.center - aabb.half_extents;
                let aabb_max = aabb.center + aabb.half_extents;

                log::debug!("  Child mesh {:?}:", child);
                log::debug!(
                    "    Transform: pos={:?}, rot={:?}, scale={:?}",
                    transform.translation,
                    transform.rotation,
                    transform.scale
                );
                log::debug!(
                    "    Aabb: center={:?}, half_extents={:?}",
                    aabb.center,
                    aabb.half_extents
                );
                log::debug!("    Local bounds: min={:?}, max={:?}", aabb_min, aabb_max);

                // Apply transform to get bounds in parent space
                // For now, we'll use the translation and ignore rotation/scale for simplicity
                let transformed_min = Vec2::new(
                    transform.translation.x + aabb_min.x,
                    transform.translation.y + aabb_min.y,
                );
                let transformed_max = Vec2::new(
                    transform.translation.x + aabb_max.x,
                    transform.translation.y + aabb_max.y,
                );

                log::debug!(
                    "    Transformed bounds: min={:?}, max={:?}",
                    transformed_min,
                    transformed_max
                );

                // Update overall bounds
                min_bounds = min_bounds.min(transformed_min);
                max_bounds = max_bounds.max(transformed_max);
            }
        }

        log::debug!(
            "  Final bounds: min={:?}, max={:?}, has_content={}",
            min_bounds,
            max_bounds,
            has_content
        );

        // Calculate background size with safety checks
        let background_size = if has_content
            && min_bounds.x.is_finite()
            && min_bounds.y.is_finite()
            && max_bounds.x.is_finite()
            && max_bounds.y.is_finite()
        {
            let content_size = max_bounds - min_bounds;
            log::debug!("  Content size: {:?}", content_size);

            // Ensure content size is valid
            if content_size.x > 0.0
                && content_size.y > 0.0
                && content_size.x.is_finite()
                && content_size.y.is_finite()
            {
                let padded_size = content_size + Vec2::splat(dynamic_bg.padding * 2.0);
                log::debug!(
                    "  Padded size (padding={}): {:?}",
                    dynamic_bg.padding,
                    padded_size
                );

                // Apply minimum size if specified, otherwise just ensure positive values
                if let Some(min_size) = dynamic_bg.min_size {
                    let final_size = Vec2::new(
                        padded_size.x.max(min_size.x).max(0.01),
                        padded_size.y.max(min_size.y).max(0.01),
                    );
                    log::debug!(
                        "  Final size with min_size {:?}: {:?}",
                        min_size,
                        final_size
                    );
                    final_size
                } else {
                    let final_size = Vec2::new(padded_size.x.max(0.01), padded_size.y.max(0.01));
                    log::debug!("  Final size (no min_size): {:?}", final_size);
                    final_size
                }
            } else {
                // Invalid content size, use minimum or hide
                dynamic_bg.min_size.unwrap_or(Vec2::ZERO)
            }
        } else {
            // No valid content or invalid bounds, use minimum or hide
            dynamic_bg.min_size.unwrap_or(Vec2::ZERO)
        };

        // Handle mesh creation based on calculated size
        log::debug!("  Background size: {:?}", background_size);

        if background_size == Vec2::ZERO {
            // Hide the background by setting visibility to hidden
            log::debug!("  -> Hiding background (size is zero)");
            commands
                .entity(background_entity)
                .insert(Visibility::Hidden);
        } else if background_size.x > 0.0
            && background_size.y > 0.0
            && background_size.x.is_finite()
            && background_size.y.is_finite()
            && background_size.x < 100.0
            && background_size.y < 100.0
        {
            // Create mesh with calculated size and ensure it's visible
            log::debug!("  -> Creating mesh with size {:?}", background_size);

            // Extra safety check to prevent zero-sized mesh
            if background_size.x <= 0.0 || background_size.y <= 0.0 {
                log::error!(
                    "Attempted to create mesh with invalid size: {:?}",
                    background_size
                );
                continue;
            }

            let new_mesh = meshes.add(Plane3d::new(Vec3::Z, background_size));
            let material_handle = materials.add(dynamic_bg.material.clone());
            commands.entity(background_entity).insert((
                Mesh3d(new_mesh),
                MeshMaterial3d(material_handle),
                Visibility::Inherited,
            ));
        } else {
            log::debug!("  -> Skipping mesh update (invalid size)");
        }
    }
}
