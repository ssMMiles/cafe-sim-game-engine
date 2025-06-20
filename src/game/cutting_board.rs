use super::icons::SvgTextureCache;
use super::interaction::{HoveredOver, Interactable, InteractableType};
use super::{
    GameEntity,
    simulation::{ActiveCooking, CuttingBoardMarker, TableMarker},
};
use bevy::{log, prelude::*};

#[derive(Component)]
pub struct CuttingBoardMenu;

#[derive(Component)]
pub struct RecipeCard {
    pub recipe_name: String,
}

#[derive(Component)]
pub struct MenuOwner(pub Entity);

#[derive(Component)]
pub struct CardShadow(pub Entity); // Links shadow to its card

#[derive(Component)]
pub struct HoverAnimation {
    pub target_z: f32,
    pub current_z: f32,
    pub speed: f32,
}

#[derive(Component)]
pub struct DistanceCloseable {
    pub max_distance: f32,
    pub owner_entity: Entity,
}

#[derive(Resource, Default)]
pub struct ActiveCuttingBoardMenu(pub Option<Entity>);

pub fn animate_card_hover(
    mut card_query: Query<
        (&mut Transform, &mut HoverAnimation, Option<&HoveredOver>),
        With<RecipeCard>,
    >,
    time: Res<Time>,
) {
    for (mut transform, mut animation, hovered) in card_query.iter_mut() {
        // Set target based on hover state (updated for new base position)
        animation.target_z = if hovered.is_some() { 0.3 } else { 0.2 };

        // Smoothly animate towards target
        let direction = animation.target_z - animation.current_z;
        if direction.abs() > 0.001 {
            let move_amount = direction * animation.speed * time.delta_secs();
            animation.current_z += move_amount;

            // Update the actual transform
            transform.translation.z = animation.current_z;
        } else {
            // Snap to target when very close
            animation.current_z = animation.target_z;
            transform.translation.z = animation.current_z;
        }
    }
}

pub fn update_card_shadows(
    card_query: Query<(Entity, &Transform, Option<&HoveredOver>), With<RecipeCard>>,
    mut shadow_query: Query<(&CardShadow, &mut Transform), (Without<RecipeCard>, With<CardShadow>)>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    material_query: Query<&MeshMaterial3d<StandardMaterial>, With<CardShadow>>,
) {
    for (shadow_component, mut shadow_transform) in shadow_query.iter_mut() {
        if let Ok((card_entity, card_transform, card_hovered)) = card_query.get(shadow_component.0)
        {
            // Calculate shadow offset based on card elevation (updated for new positions)
            let card_z = card_transform.translation.z;
            let elevation_factor = (card_z - 0.2) / (0.3 - 0.2); // 0.0 to 1.0
            let elevation_factor = elevation_factor.clamp(0.0, 1.0);

            // Update shadow position with more offset when elevated
            let base_x = if card_transform.translation.x < 0.0 {
                -0.4
            } else {
                0.4
            };
            let shadow_offset_x = 0.01 + elevation_factor * 0.02; // Increases with elevation
            let shadow_offset_y = -0.01 - elevation_factor * 0.01; // Increases with elevation

            shadow_transform.translation.x = base_x + shadow_offset_x;
            shadow_transform.translation.y = shadow_offset_y;
            shadow_transform.translation.z = 0.05; // Keep shadow behind cards but in front of menu

            // Update shadow opacity - more transparent when elevated
            let base_alpha = 0.3;
            let hovered_alpha = 0.15;
            let target_alpha = if card_hovered.is_some() {
                hovered_alpha
            } else {
                base_alpha
            };

            // Find and update the shadow's material
            if let Ok(material_handle) = material_query.get(card_entity) {
                if let Some(material) = materials.get_mut(&material_handle.0) {
                    material.base_color = Color::srgba(0.0, 0.0, 0.0, target_alpha);
                }
            }
        }
    }
}

pub fn check_menu_distances(
    mut commands: Commands,
    player_query: Query<&Transform, With<super::camera::PlayerCamera>>,
    owner_query: Query<&Transform, (With<Interactable>, Without<super::camera::PlayerCamera>)>,
    menu_query: Query<(Entity, &DistanceCloseable)>,
    mut active_cutting_board_menu: ResMut<ActiveCuttingBoardMenu>,
) {
    if let Ok(player_transform) = player_query.single() {
        let player_pos = player_transform.translation;

        // Check each menu with distance-based closing
        for (menu_entity, distance_config) in menu_query.iter() {
            // Get the owner's position
            if let Ok(owner_transform) = owner_query.get(distance_config.owner_entity) {
                let owner_pos = owner_transform.translation;

                // Calculate 2D floor distance (ignore Y axis)
                let distance = (Vec2::new(player_pos.x, player_pos.z)
                    - Vec2::new(owner_pos.x, owner_pos.z))
                .length();

                // Close menu if player is too far away
                if distance > distance_config.max_distance {
                    log::info!(
                        "Auto-closing menu due to distance: {:.1} > {:.1}",
                        distance,
                        distance_config.max_distance
                    );

                    // Remove the menu entity
                    commands.entity(menu_entity).despawn();

                    // Update the active menu resource if this was the active cutting board menu
                    if active_cutting_board_menu.0 == Some(menu_entity) {
                        active_cutting_board_menu.0 = None;
                    }
                }
            }
        }
    }
}

pub fn update_cutting_board_menu_rotation(
    camera_query: Query<&Transform, With<super::camera::PlayerCamera>>,
    mut menu_query: Query<
        &mut Transform,
        (With<CuttingBoardMenu>, Without<super::camera::PlayerCamera>),
    >,
) {
    if let Ok(camera_transform) = camera_query.single() {
        for mut menu_transform in menu_query.iter_mut() {
            // Keep the menu facing the camera
            let to_camera = (camera_transform.translation - menu_transform.translation).normalize();
            // Keep the menu facing the camera horizontally
            let forward = Vec3::new(to_camera.x, 0.0, to_camera.z).normalize();
            let new_rotation = Quat::from_rotation_arc(Vec3::Z, forward);
            menu_transform.rotation = new_rotation;
        }
    }
}

pub fn handle_recipe_card_click(
    commands: &mut Commands,
    recipe_name: &str,
    active_menu: &mut ResMut<ActiveCuttingBoardMenu>,
    menu_owner_query: &Query<&MenuOwner, With<CuttingBoardMenu>>,
    table_query: &mut Query<
        (Entity, Option<&ActiveCooking>),
        Or<(With<TableMarker>, With<CuttingBoardMarker>)>,
    >,
) {
    log::info!("Clicked on recipe card: {}", recipe_name);

    // Use the active menu to find the table
    if let Some(menu_entity) = active_menu.0 {
        log::info!("Found active menu entity: {:?}", menu_entity);

        if let Ok(menu_owner) = menu_owner_query.get(menu_entity) {
            let table_entity = menu_owner.0;
            log::info!("Found table entity: {:?}", table_entity);

            // Check if table is already cooking
            if let Ok((_, cooking_status)) = table_query.get_mut(table_entity) {
                if cooking_status.is_none() {
                    // Start cooking this recipe
                    let cooking_time = match recipe_name {
                        "Cake Slice" => 30.0,
                        "Cupcake" => 120.0,
                        _ => 60.0, // Default
                    };

                    commands.entity(table_entity).insert(ActiveCooking {
                        recipe_name: recipe_name.to_string(),
                        total_time: cooking_time,
                        remaining_time: cooking_time,
                    });

                    // Close the menu after starting to cook
                    if let Some(menu_entity) = active_menu.0 {
                        commands.entity(menu_entity).despawn();
                        active_menu.0 = None;
                        log::info!("Closed cutting board menu after starting cooking");
                    }

                    log::info!(
                        "Started cooking {} on table ({}s)",
                        recipe_name,
                        cooking_time
                    );
                } else {
                    log::info!("Table already cooking: {:?}", cooking_status);
                }
            } else {
                log::warn!("Could not find table entity in table_query");
            }
        } else {
            log::warn!("Could not get menu owner from menu entity");
        }
    } else {
        log::warn!("No active menu found when clicking recipe card");
    }
}

pub fn handle_cutting_board_click(
    commands: &mut Commands,
    entity: Entity,
    transform_query: &Query<&Transform>,
    camera_query: &Query<&Transform, With<super::camera::PlayerCamera>>,
    active_menu: &mut ResMut<ActiveCuttingBoardMenu>,
    menu_owner_query: &Query<&MenuOwner, With<CuttingBoardMenu>>,
    meshes: &mut ResMut<Assets<Mesh>>,
    materials: &mut ResMut<Assets<StandardMaterial>>,
    svg_cache: &Res<SvgTextureCache>,
    table_query: &Query<
        (Entity, Option<&ActiveCooking>),
        Or<(With<TableMarker>, With<CuttingBoardMarker>)>,
    >,
) {
    let cutting_board_transform = transform_query.get(entity).unwrap();

    // Check if this cutting board already has the active menu
    let is_same_cutting_board = if let Some(menu_entity) = active_menu.0 {
        if let Ok(menu_owner) = menu_owner_query.get(menu_entity) {
            menu_owner.0 == entity
        } else {
            false
        }
    } else {
        false
    };

    // If same cutting board clicked, just close the menu (toggle behavior)
    if is_same_cutting_board {
        if let Some(menu_entity) = active_menu.0.take() {
            commands.entity(menu_entity).despawn();
        }
        return; // Early return - don't create new menu
    }

    // Close any existing menu (different cutting board or no menu open)
    if let Some(menu_entity) = active_menu.0 {
        commands.entity(menu_entity).despawn();
    }

    // Check if this cutting board is currently cooking
    if let Ok((_, cooking_status)) = table_query.get(entity) {
        if cooking_status.is_some() {
            log::info!("Cannot open menu - cutting board is currently cooking");
            active_menu.0 = None;
            return; // Don't open menu if cooking
        }
    }

    // Get camera transform to face the menu towards the player
    if let Ok(camera_transform) = camera_query.single() {
        let menu_position = cutting_board_transform.translation + Vec3::Y * 2.0; // 2 units above cutting board

        // Calculate rotation to face the camera
        let to_camera = (camera_transform.translation - menu_position).normalize();
        // Calculate rotation to face camera horizontally
        let forward = Vec3::new(to_camera.x, 0.0, to_camera.z).normalize();
        let menu_rotation = Quat::from_rotation_arc(Vec3::Z, forward);

        // Create menu background
        let menu_entity = commands
            .spawn((
                GameEntity,
                CuttingBoardMenu,
                MenuOwner(entity),
                DistanceCloseable {
                    max_distance: 20.0, // Close when player is 8 units away
                    owner_entity: entity,
                },
                Mesh3d(meshes.add(Plane3d::new(Vec3::Z, Vec2::new(4.0 / 3.0, 2.0 / 3.0)))), // Z normal faces forward
                MeshMaterial3d(materials.add(StandardMaterial {
                    base_color: Color::srgba(0.9, 0.7, 0.4, 0.9), // Beige like cake card
                    alpha_mode: bevy::render::alpha::AlphaMode::Blend,
                    double_sided: true,
                    ..default()
                })),
                Transform::from_translation(menu_position).with_rotation(menu_rotation),
                GlobalTransform::default(),
            ))
            .with_children(|parent| {
                // Check if this table is cooking
                let is_cooking = if let Ok((_, cooking)) = table_query.get(entity) {
                    cooking.map(|c| c.recipe_name.clone())
                } else {
                    None
                };

                // Cake Slice Card
                let cake_card = parent
                    .spawn((
                        RecipeCard {
                            recipe_name: "Cake Slice".to_string(),
                        },
                        Interactable {
                            name: "Cake Slice Recipe".to_string(),
                            distance: 15.0, // Reasonable interaction distance for UI elements
                            interactable_type: InteractableType::RecipeCard(
                                "Cake Slice".to_string(),
                            ),
                        },
                        Mesh3d(meshes.add(Plane3d::new(Vec3::Z, Vec2::new(0.233, 0.233)))), // Z normal faces forward - 2/3rd size
                        MeshMaterial3d(materials.add(StandardMaterial {
                            base_color_texture: svg_cache.cake_slice.clone(),
                            base_color: Color::WHITE, // Tint color
                            double_sided: true,
                            ..default()
                        })),
                        Transform::from_translation(Vec3::new(-0.4, 0.0, 0.2)),
                        GlobalTransform::default(),
                        HoverAnimation {
                            target_z: 0.2,
                            current_z: 0.2,
                            speed: 5.0,
                        },
                    ))
                    .with_children(|card_parent| {
                        // Add progress overlay if this recipe is currently cooking
                        if let Some(ref cooking_recipe) = is_cooking {
                            if cooking_recipe == "Cake Slice" {
                                // Progress background circle
                                card_parent.spawn((
                                    super::simulation::CookingProgressOverlay,
                                    Mesh3d(meshes.add(Circle::new(0.15))),
                                    MeshMaterial3d(materials.add(StandardMaterial {
                                        base_color: Color::srgba(0.0, 0.0, 0.0, 0.7),
                                        alpha_mode: bevy::render::alpha::AlphaMode::Blend,
                                        double_sided: true,
                                        ..default()
                                    })),
                                    Transform::from_translation(Vec3::new(0.0, 0.0, 0.02)), // Above card
                                ));

                                // TODO: Add progress ring/arc here based on cooking progress
                            }
                        }
                    })
                    .id();

                // Spawn shadow for cake card
                parent.spawn((
                    CardShadow(cake_card),
                    Mesh3d(meshes.add(Plane3d::new(Vec3::Z, Vec2::new(0.247, 0.247)))), // Slightly larger shadow - 2/3rd size
                    MeshMaterial3d(materials.add(StandardMaterial {
                        base_color: Color::srgba(0.0, 0.0, 0.0, 0.3), // Semi-transparent black
                        alpha_mode: bevy::render::alpha::AlphaMode::Blend,
                        double_sided: true,
                        ..default()
                    })),
                    Transform::from_translation(Vec3::new(-0.4 + 0.01, -0.01, 0.05)), // Slightly offset shadow
                ));

                // Add text to cake card separately
                parent.spawn((
                    Text2d::new("Cake\nSlice"),
                    TextFont {
                        font_size: 40.0 / 3.0,
                        ..default()
                    },
                    TextColor(Color::srgb(0.2, 0.1, 0.1)),
                    Transform::from_translation(Vec3::new(-0.4, 0.0, 0.21)), // Small offset from card
                ));

                // Cupcake Card
                let cupcake_card = parent
                    .spawn((
                        RecipeCard {
                            recipe_name: "Cupcake".to_string(),
                        },
                        Interactable {
                            name: "Cupcake Recipe".to_string(),
                            distance: 15.0, // Reasonable interaction distance for UI elements
                            interactable_type: InteractableType::RecipeCard("Cupcake".to_string()),
                        },
                        Mesh3d(meshes.add(Plane3d::new(Vec3::Z, Vec2::new(0.233, 0.233)))), // Z normal faces forward - 2/3rd size
                        MeshMaterial3d(materials.add(StandardMaterial {
                            base_color_texture: svg_cache.cupcake.clone(),
                            base_color: Color::WHITE, // Tint color
                            double_sided: true,
                            ..default()
                        })),
                        Transform::from_translation(Vec3::new(0.4, 0.0, 0.2)),
                        GlobalTransform::default(),
                        HoverAnimation {
                            target_z: 0.2,
                            current_z: 0.2,
                            speed: 5.0,
                        },
                    ))
                    .with_children(|card_parent| {
                        // Add progress overlay if this recipe is currently cooking
                        if let Some(ref cooking_recipe) = is_cooking {
                            if cooking_recipe == "Cupcake" {
                                // Progress background circle
                                card_parent.spawn((
                                    super::simulation::CookingProgressOverlay,
                                    Mesh3d(meshes.add(Circle::new(0.15))),
                                    MeshMaterial3d(materials.add(StandardMaterial {
                                        base_color: Color::srgba(0.0, 0.0, 0.0, 0.7),
                                        alpha_mode: bevy::render::alpha::AlphaMode::Blend,
                                        double_sided: true,
                                        ..default()
                                    })),
                                    Transform::from_translation(Vec3::new(0.0, 0.0, 0.02)), // Above card
                                ));

                                // TODO: Add progress ring/arc here based on cooking progress
                            }
                        }
                    })
                    .id();

                // Spawn shadow for cupcake card
                parent.spawn((
                    CardShadow(cupcake_card),
                    Mesh3d(meshes.add(Plane3d::new(Vec3::Z, Vec2::new(0.247, 0.247)))), // Slightly larger shadow - 2/3rd size
                    MeshMaterial3d(materials.add(StandardMaterial {
                        base_color: Color::srgba(0.0, 0.0, 0.0, 0.3), // Semi-transparent black
                        alpha_mode: bevy::render::alpha::AlphaMode::Blend,
                        double_sided: true,
                        ..default()
                    })),
                    Transform::from_translation(Vec3::new(0.4 + 0.01, -0.01, 0.05)), // Slightly offset shadow
                ));

                // Add text to cupcake card separately
                parent.spawn((
                    Text2d::new("Cupcake"),
                    TextFont {
                        font_size: 40.0 / 3.0,
                        ..default()
                    },
                    TextColor(Color::srgb(0.2, 0.1, 0.1)),
                    Transform::from_translation(Vec3::new(0.4, 0.0, 0.21)), // Small offset from card
                ));
            })
            .id();

        // Store the active menu
        active_menu.0 = Some(menu_entity);
    }
}
