use bevy::{log, prelude::*};

use super::icons::{SvgTextureCache, load_svg_icons};
use super::interaction::{HoveredOver, Interactable, InteractableType};
use super::materials::create_arc_mesh;
use super::{GameEntity, SimulationConfig};
use crate::settings::UserSettings;

#[derive(Component)]
pub struct InteractionText;

#[derive(Component)]
pub struct Crosshair;

#[derive(Component)]
pub struct TableMarker;

#[derive(Component)]
pub struct CookerMarker;

#[derive(Component)]
pub struct FridgeMarker;

#[derive(Component)]
pub struct CuttingBoardMarker;

#[derive(Component, Debug)]
pub struct ActiveCooking {
    pub recipe_name: String,
    pub total_time: f32,
    pub remaining_time: f32,
}

#[derive(Component)]
pub struct CookingProgressOverlay;

#[derive(Component)]
pub struct CookingDisplay {
    pub table_entity: Entity,
}

#[derive(Component)]
pub struct ProgressArc;

pub fn setup(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    images: ResMut<Assets<Image>>,
    mut windows: Query<&mut Window>,
    asset_server: Res<AssetServer>,
    svg_cache: ResMut<SvgTextureCache>,
) {
    log::info!("Setting up the simulation environment");

    // Lock cursor to window for FPS-style controls
    if let Ok(mut window) = windows.single_mut() {
        window.cursor_options.grab_mode = bevy::window::CursorGrabMode::Locked;
        window.cursor_options.visible = false;
    }

    super::camera::setup_camera(&mut commands);
    commands.insert_resource(SimulationConfig::default());

    // Load SVG icons
    load_svg_icons(images, svg_cache);

    // Setup the game world
    super::world::setup(&mut commands, &mut meshes, &mut materials, &asset_server);
}

pub fn show_interaction_text(
    mut commands: Commands,
    sphere_query: Query<(Entity, &Transform), (With<Interactable>, With<HoveredOver>)>,
    text_query: Query<Entity, With<InteractionText>>,
) {
    // Remove existing interaction text
    for entity in text_query.iter() {
        commands.entity(entity).despawn();
    }

    // Add text for any sphere being interacted with
    for (_, sphere_transform) in sphere_query.iter() {
        let text_position = sphere_transform.translation + Vec3::new(0.0, 2.0, 0.0); // Hover above sphere

        commands.spawn((
            GameEntity,
            InteractionText,
            Text2d::new("Boo!"),
            TextFont {
                font_size: 60.0,
                ..default()
            },
            TextColor(Color::srgb(1.0, 1.0, 1.0)), // White text
            Transform::from_translation(text_position),
            GlobalTransform::default(),
        ));
    }
}

pub fn toggle_crosshair(mut settings: ResMut<UserSettings>, input: Res<ButtonInput<KeyCode>>) {
    if input.just_pressed(KeyCode::KeyT)
        && (input.pressed(KeyCode::ShiftLeft) || input.pressed(KeyCode::ShiftRight))
    {
        settings.toggle_crosshair();
    }
}

pub fn update_crosshair_display(
    mut commands: Commands,
    settings: Res<UserSettings>,
    crosshair_query: Query<Entity, With<Crosshair>>,
) {
    if settings.is_changed() {
        // Remove existing crosshair
        for entity in crosshair_query.iter() {
            commands.entity(entity).despawn();
        }

        // Add crosshair if visible
        if settings.crosshair_visible {
            // Create simple square crosshair at center of screen
            commands.spawn((
                GameEntity,
                Crosshair,
                Node {
                    position_type: PositionType::Absolute,
                    left: Val::Percent(50.0),
                    top: Val::Percent(50.0),
                    width: Val::Px(4.0),
                    height: Val::Px(4.0),
                    ..default()
                },
                BackgroundColor(Color::srgb(1.0, 1.0, 1.0)), // White square
            ));
        }
    }
}

pub fn update_cooking_timers(
    mut commands: Commands,
    mut cooking_query: Query<(Entity, &mut ActiveCooking)>,
    time: Res<Time>,
    cooking_display_query: Query<(Entity, &CookingDisplay)>,
) {
    for (entity, mut cooking) in cooking_query.iter_mut() {
        // Count down the timer
        cooking.remaining_time -= time.delta_secs();

        // Check if cooking is complete
        if cooking.remaining_time <= 0.0 {
            log::info!("Finished cooking {} on table", cooking.recipe_name);

            // Remove the cooking component
            commands.entity(entity).remove::<ActiveCooking>();

            // Remove any cooking displays associated with this table
            for (display_entity, display) in cooking_display_query.iter() {
                if display.table_entity == entity {
                    commands.entity(display_entity).despawn();
                }
            }

            // TODO: Spawn finished product
        }
    }
}

pub fn display_cooking_progress(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    _asset_server: Res<AssetServer>,
    svg_cache: Res<SvgTextureCache>,
    tables_query: Query<
        (Entity, &Transform, &ActiveCooking),
        Or<(With<TableMarker>, With<CuttingBoardMarker>)>,
    >,
    cooking_display_query: Query<(Entity, &CookingDisplay)>,
) {
    // Create displays for tables that are cooking but don't have a display
    for (table_entity, table_transform, cooking) in tables_query.iter() {
        // Check if this table already has a cooking display
        let has_display = cooking_display_query
            .iter()
            .any(|(_, display)| display.table_entity == table_entity);

        if has_display {
            continue;
        }
        log::info!(
            "Creating cooking display for table with recipe: {}",
            cooking.recipe_name
        );

        // Get the appropriate texture
        let texture_handle = match cooking.recipe_name.as_str() {
            "Cake Slice" => svg_cache.cake_slice.clone(),
            "Cupcake" => svg_cache.cupcake.clone(),
            _ => None,
        };

        // Calculate world position for the display (2 units above table)
        let display_position = table_transform.translation + Vec3::Y * 2.0;

        // Calculate rotation to face camera (will be updated each frame)
        let initial_rotation = Quat::from_rotation_arc(Vec3::Z, Vec3::X); // Default forward direction

        // Create the display entity with fixed size background
        let _display_entity = commands
            .spawn((
                GameEntity,
                CookingDisplay { table_entity },
                Mesh3d(meshes.add(Plane3d::new(Vec3::Z, Vec2::new(0.3, 0.7)))),
                MeshMaterial3d(materials.add(StandardMaterial {
                    base_color: Color::srgba(0.9, 0.7, 0.4, 0.9), // Same beige as cutting board menu
                    unlit: true,
                    alpha_mode: bevy::render::alpha::AlphaMode::Blend,
                    double_sided: true,
                    ..default()
                })),
                Transform::from_translation(display_position).with_rotation(initial_rotation),
                Visibility::Inherited,
            ))
            .with_children(|parent| {
                // Progress circle background (grey outline) - quarter size
                parent.spawn((
                    Mesh3d(meshes.add(Circle::new(0.0925))),
                    MeshMaterial3d(materials.add(StandardMaterial {
                        base_color: Color::srgba(0.4, 0.4, 0.4, 0.8),
                        unlit: true,
                        double_sided: true,
                        ..default()
                    })),
                    Transform::from_translation(Vec3::new(0.0, 0.1, 0.01)),
                    Visibility::Inherited,
                ));

                // Progress arc (green fill that grows with progress) - quarter size
                let progress = 1.0 - (cooking.remaining_time / cooking.total_time); // Invert so it fills up
                let arc_radius = 0.0875;
                // Adjust Y position to compensate for arc mesh offset
                let arc_y = 0.1 - (arc_radius * 0.5);
                parent.spawn((
                    ProgressArc,
                    Mesh3d(meshes.add(create_arc_mesh(arc_radius, progress))),
                    MeshMaterial3d(materials.add(StandardMaterial {
                        base_color: Color::srgba(0.2, 0.8, 0.2, 1.0),
                        unlit: true,
                        double_sided: true,
                        ..default()
                    })),
                    Transform::from_translation(Vec3::new(0.0, arc_y, 0.02)),
                    Visibility::Inherited,
                ));

                // Recipe icon - positioned below the smaller progress circle
                if let Some(texture) = texture_handle {
                    parent.spawn((
                        Mesh3d(meshes.add(Plane3d::new(Vec3::Z, Vec2::new(0.2, 0.2)))),
                        MeshMaterial3d(materials.add(StandardMaterial {
                            base_color_texture: Some(texture),
                            unlit: true,
                            alpha_mode: bevy::render::alpha::AlphaMode::Blend,
                            double_sided: true,
                            ..default()
                        })),
                        Transform::from_translation(Vec3::new(0.0, -0.35, 0.03)),
                        Visibility::Inherited,
                    ));
                }
            })
            .id();
    }
}

pub fn update_cooking_displays(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    _materials: ResMut<Assets<StandardMaterial>>,
    cooking_query: Query<
        (Entity, &ActiveCooking),
        Or<(With<TableMarker>, With<CuttingBoardMarker>)>,
    >,
    display_query: Query<(Entity, &CookingDisplay)>,
    children_query: Query<&Children>,
    progress_arc_query: Query<Entity, With<ProgressArc>>,
) {
    // For each cooking display
    for (display_entity, display) in display_query.iter() {
        // Get the cooking info from the associated table
        if let Ok((_, cooking)) = cooking_query.get(display.table_entity) {
            // Find and update the progress arc
            if let Ok(display_children) = children_query.get(display_entity) {
                for child in display_children.iter() {
                    if progress_arc_query.get(child).is_ok() {
                        // Update the progress arc mesh
                        let progress = 1.0 - (cooking.remaining_time.max(0.0) / cooking.total_time);
                        commands
                            .entity(child)
                            .insert(Mesh3d(meshes.add(create_arc_mesh(0.0875, progress))));
                        break;
                    }
                }
            }
        }
    }
}

pub fn update_cooking_display_rotation(
    camera_query: Query<&Transform, With<super::camera::PlayerCamera>>,
    mut display_query: Query<
        &mut Transform,
        (With<CookingDisplay>, Without<super::camera::PlayerCamera>),
    >,
) {
    if let Ok(camera_transform) = camera_query.single() {
        for mut display_transform in display_query.iter_mut() {
            // Keep the display facing the camera
            let to_camera =
                (camera_transform.translation - display_transform.translation).normalize();
            // Keep the display facing the camera horizontally
            let forward = Vec3::new(to_camera.x, 0.0, to_camera.z).normalize();
            let new_rotation = Quat::from_rotation_arc(Vec3::Z, forward);
            display_transform.rotation = new_rotation;
        }
    }
}

pub fn exit(
    mut commands: Commands,
    mut windows: Query<&mut Window>,
    game_entities: Query<Entity, With<GameEntity>>,
) {
    // Use world::cleanup to despawn all game entities
    super::world::cleanup(commands.reborrow(), game_entities);

    // Cleanup the simulation environment
    commands.remove_resource::<SimulationConfig>();

    // Unlock cursor when exiting game
    if let Ok(mut window) = windows.single_mut() {
        window.cursor_options.grab_mode = bevy::window::CursorGrabMode::None;
        window.cursor_options.visible = true;
    }
}

// Main handler for all interactable clicks
pub fn handle_interactable_clicks(
    mut commands: Commands,
    mut click_events: EventReader<super::interaction::InteractableClicked>,
    camera_query: Query<&Transform, With<super::camera::PlayerCamera>>,
    transform_query: Query<&Transform>,
    mut active_menu: ResMut<super::cutting_board::ActiveCuttingBoardMenu>,
    menu_owner_query: Query<
        &super::cutting_board::MenuOwner,
        With<super::cutting_board::CuttingBoardMenu>,
    >,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    svg_cache: Res<SvgTextureCache>,
    mut table_query: Query<
        (Entity, Option<&ActiveCooking>),
        Or<(With<TableMarker>, With<CuttingBoardMarker>)>,
    >,
) {
    for event in click_events.read() {
        let entity = event.entity;

        match &event.interactable.interactable_type {
            InteractableType::CuttingBoard => {
                super::cutting_board::handle_cutting_board_click(
                    &mut commands,
                    entity,
                    &transform_query,
                    &camera_query,
                    &mut active_menu,
                    &menu_owner_query,
                    &mut meshes,
                    &mut materials,
                    &svg_cache,
                    &table_query,
                );
            }
            InteractableType::RecipeCard(recipe_name) => {
                super::cutting_board::handle_recipe_card_click(
                    &mut commands,
                    recipe_name,
                    &mut active_menu,
                    &menu_owner_query,
                    &mut table_query,
                );
            }
            _ => {
                log::warn!(
                    "Clicked on interactable with no action registered: {:?}: {}",
                    event.interactable.interactable_type,
                    event.interactable.name
                );
            }
        }
    }
}
