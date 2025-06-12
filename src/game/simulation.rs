use bevy::{
    diagnostic::{DiagnosticsStore, FrameTimeDiagnosticsPlugin},
    log,
    pbr::NotShadowCaster,
    picking::mesh_picking::ray_cast::MeshRayCast,
    prelude::*,
    scene::SceneInstanceReady,
};
use bevy_mod_outline::OutlineVolume;

use super::{GameEntity, SimulationConfig};
use crate::settings::{PlacementObjectType, UserSettings};

#[derive(Component, Clone)]
pub struct Interactable {
    pub name: String,
    pub distance: f32,
    pub angle_threshold: Option<f32>, // If None, use raycasting to mesh
}

#[derive(Component)]
pub enum InteractableType {
    Cooker,
    Fridge,
    Table,
    CuttingBoard,
}

#[derive(Component)]
pub struct BeingInteractedWith;

#[derive(Component)]
pub struct InteractionText;

#[derive(Component)]
pub struct Crosshair;

#[derive(Component)]
pub struct KeybindsDisplay;

#[derive(Component)]
pub struct FpsDisplay;

#[derive(Component)]
pub struct InteractionInfoDisplay;

#[derive(Component)]
pub struct PositionDisplay;

#[derive(Component)]
pub struct TableMarker;

#[derive(Component)]
pub struct CookerMarker;

#[derive(Component)]
pub struct FridgeMarker;

#[derive(Component)]
pub struct CuttingBoardMarker;

#[derive(Component)]
pub struct PlacementPreview;

#[derive(Resource, Default)]
pub struct PlacementPreviewEntity(pub Option<Entity>);

#[derive(Component)]
pub struct InteractableParent(pub Entity);

#[derive(Component, Clone)]
pub struct PendingMaterial {
    pub base_color: Color,
    pub metallic: f32,
    pub perceptual_roughness: f32,
    pub alpha_mode: AlphaMode,
}

#[derive(Component)]
pub struct CuttingBoardMenu;

#[derive(Component)]
pub struct RecipeCard {
    pub recipe_name: String,
}

#[derive(Component)]
pub struct MenuOwner(pub Entity);

#[derive(Resource, Default)]
pub struct ActiveCuttingBoardMenu(pub Option<Entity>);

pub fn setup(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut windows: Query<&mut Window>,
    asset_server: Res<AssetServer>,
) {
    log::info!("Setting up the simulation environment");

    // Lock cursor to window for FPS-style controls
    if let Ok(mut window) = windows.single_mut() {
        window.cursor_options.grab_mode = bevy::window::CursorGrabMode::Locked;
        window.cursor_options.visible = false;
    }

    super::camera::setup_camera(&mut commands);
    commands.insert_resource(SimulationConfig::default());

    let sun_position = Vec3::new(25.0, 50.0, -150.0);
    // get rotation from pointing at 0,0 from sun_position

    let target = Vec3::new(0.0, 0.0, 0.0);
    let direction = (target - sun_position).normalize();
    let sun_rotation = Quat::from_rotation_arc(Vec3::NEG_Z, direction);

    // Add lighting
    let sun_transform = Transform {
        translation: sun_position,
        rotation: sun_rotation,
        ..default()
    };

    commands.spawn((
        GameEntity,
        DirectionalLight {
            color: Color::srgb(1.0, 1.0, 0.9),
            illuminance: 10000.0,
            shadows_enabled: true,
            ..default()
        },
        sun_transform,
    ));

    // Debug sun position with yellow cone
    commands.spawn((
        GameEntity,
        Mesh3d(meshes.add(Cone::new(1.0, 3.0))),
        MeshMaterial3d(materials.add(StandardMaterial {
            base_color: Color::srgb(1.0, 1.0, 0.0),      // Bright yellow
            emissive: Color::srgb(1.0, 1.0, 0.0).into(), // Fully emissive so it glows and doesn't cast shadows
            cull_mode: None,                             // Show both sides
            ..Default::default()
        })),
        NotShadowCaster, // Prevent casting shadows
        Transform {
            translation: sun_transform.translation,
            rotation: sun_transform.rotation * Quat::from_rotation_x(-std::f32::consts::PI / 2.0), // Point the cone downwards
            scale: Vec3::splat(3.0), // Make it bigger
        },
    ));

    // Add ambient light for diffusion
    commands.insert_resource(AmbientLight {
        color: Color::srgb(0.6, 0.6, 0.7), // Slightly brighter and warmer
        brightness: 800.0,                 // Much higher for more diffusion
        affects_lightmapped_meshes: true,
    });

    let sphere = meshes.add(Sphere::default());

    // spawn a sphere at 0, 0, 0
    commands.spawn((
        GameEntity,
        Interactable {
            name: "Red Sphere".to_string(),
            distance: 10.0,
            angle_threshold: None, // Use mesh raycasting
        },
        Mesh3d(sphere),
        MeshMaterial3d(materials.add(StandardMaterial {
            base_color: Color::srgb(1.0, 0.1, 0.1),
            ..Default::default()
        })),
        Transform::from_translation(Vec3::new(0.0, 1.0, 0.0)),
        GlobalTransform::default(),
    ));

    // spawn a plane at 0, 0, 0
    commands.spawn((
        GameEntity,
        Mesh3d(meshes.add(Plane3d {
            normal: Dir3::Y,
            half_size: Vec2 { x: 100.0, y: 100.0 },
            ..Default::default()
        })),
        MeshMaterial3d(materials.add(StandardMaterial {
            base_color: Color::srgb(0.2, 0.7, 0.2),
            ..Default::default()
        })),
        Transform::from_translation(Vec3::new(0.0, 0.0, 0.0)),
        GlobalTransform::default(),
    ));

    spawn_cafe_building(&mut commands, &mut meshes, &mut materials);
    spawn_table(&mut commands, &asset_server);
}

fn spawn_cafe_building(
    commands: &mut Commands,
    meshes: &mut ResMut<Assets<Mesh>>,
    materials: &mut ResMut<Assets<StandardMaterial>>,
) {
    // Create a 4x bigger cafe building with door opening (2x shorter height)
    let cafe_roof = meshes.add(Cuboid::new(28.0, 1.0, 20.0));

    let brown_material = materials.add(StandardMaterial {
        base_color: Color::srgb(0.6, 0.4, 0.2),
        cull_mode: None,
        ..Default::default()
    });

    let red_material = materials.add(StandardMaterial {
        base_color: Color::srgb(0.8, 0.2, 0.2),
        cull_mode: None,
        ..Default::default()
    });

    // Cafe walls with door opening (front wall split into three parts) - 4x bigger, 1.5x taller
    // Left front wall piece (8 units wide, 1.5x taller)
    let left_wall = meshes.add(Cuboid::new(8.0, 9.0, 0.8));
    commands.spawn((
        GameEntity,
        Mesh3d(left_wall),
        MeshMaterial3d(brown_material.clone()),
        Transform::from_translation(Vec3::new(0.0, 4.5, -8.0)),
        GlobalTransform::default(),
    ));

    // Right front wall piece (8 units wide, 1.5x taller)
    let right_wall = meshes.add(Cuboid::new(8.0, 9.0, 0.8));
    commands.spawn((
        GameEntity,
        Mesh3d(right_wall),
        MeshMaterial3d(brown_material.clone()),
        Transform::from_translation(Vec3::new(16.0, 4.5, -8.0)),
        GlobalTransform::default(),
    ));

    // Top piece above door (8 units wide, 1.5x taller)
    let top_wall = meshes.add(Cuboid::new(8.0, 2.4, 0.8));
    commands.spawn((
        GameEntity,
        Mesh3d(top_wall),
        MeshMaterial3d(brown_material.clone()),
        Transform::from_translation(Vec3::new(8.0, 7.8, -8.0)),
        GlobalTransform::default(),
    ));

    // Back wall (1.5x taller)
    let back_wall = meshes.add(Cuboid::new(24.0, 9.0, 0.8));
    commands.spawn((
        GameEntity,
        Mesh3d(back_wall),
        MeshMaterial3d(brown_material.clone()),
        Transform::from_translation(Vec3::new(8.0, 4.5, 8.0)),
        GlobalTransform::default(),
    ));

    // Left side wall (1.5x taller)
    let left_side_wall = meshes.add(Cuboid::new(0.8, 9.0, 16.0));
    commands.spawn((
        GameEntity,
        Mesh3d(left_side_wall),
        MeshMaterial3d(brown_material.clone()),
        Transform::from_translation(Vec3::new(-4.0, 4.5, 0.0)),
        GlobalTransform::default(),
    ));

    // Right side wall (1.5x taller)
    let right_side_wall = meshes.add(Cuboid::new(0.8, 9.0, 16.0));
    commands.spawn((
        GameEntity,
        Mesh3d(right_side_wall),
        MeshMaterial3d(brown_material.clone()),
        Transform::from_translation(Vec3::new(20.0, 4.5, 0.0)),
        GlobalTransform::default(),
    ));

    // Cafe roof (1.5x taller)
    commands.spawn((
        GameEntity,
        Mesh3d(cafe_roof),
        MeshMaterial3d(red_material),
        Transform::from_translation(Vec3::new(8.0, 9.75, 0.0)),
        GlobalTransform::default(),
    ));
}

// Observer function that applies pending materials when scene is ready
fn apply_pending_material(
    trigger: Trigger<SceneInstanceReady>,
    query: Query<(&PendingMaterial, &Children)>,
    mesh_query: Query<Entity, With<Mesh3d>>,
    children_query: Query<&Children>,
    mut commands: Commands,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    let entity = trigger.target();
    
    // Get the pending material from the specific entity that triggered
    if let Ok((pending_material, children)) = query.get(entity) {
        // Create the material
        let material = materials.add(StandardMaterial {
            base_color: pending_material.base_color,
            metallic: pending_material.metallic,
            perceptual_roughness: pending_material.perceptual_roughness,
            alpha_mode: pending_material.alpha_mode,
            ..default()
        });
        
        // Apply to all mesh children
        apply_material_to_meshes(&mut commands, children, &material, &mesh_query, &children_query);
        
        // Remove the pending material component
        commands.entity(entity).remove::<PendingMaterial>();
        
        log::info!("Applied material to entity {:?} via SceneInstanceReady", entity);
    }
}

fn apply_material_to_meshes(
    commands: &mut Commands,
    children: &Children,
    material: &Handle<StandardMaterial>,
    mesh_query: &Query<Entity, With<Mesh3d>>,
    children_query: &Query<&Children>,
) {
    for child in children.iter() {
        if mesh_query.contains(child) {
            commands.entity(child).insert(MeshMaterial3d(material.clone()));
        }
        
        // Recursively apply to grandchildren
        if let Ok(grandchildren) = children_query.get(child) {
            apply_material_to_meshes(commands, grandchildren, material, mesh_query, children_query);
        }
    }
}

fn spawn_table(commands: &mut Commands, asset_server: &Res<AssetServer>) {
    // Load the scene directly, ignoring texture issues
    let table_scene: Handle<Scene> = asset_server.load("meshes/table/result.gltf#Scene0");
    log::info!("Loading scene from: meshes/table/result.gltf#Scene0");

    commands.spawn((
        GameEntity,
        Interactable {
            name: "Wooden Table".to_string(),
            distance: 10.0,
            angle_threshold: None, // Use mesh raycasting
        },
        InteractableType::Table, // Specify the type of interactable
        SceneRoot(table_scene),
        Transform::from_translation(Vec3::new(3.0, 0.0, 0.0)).with_scale(Vec3::splat(0.02)), // Scale down to 1/50th size
        GlobalTransform::default(),
        TableMarker, // Mark this as a table for material override
        PendingMaterial {
            base_color: Color::srgb(0.6, 0.4, 0.2), // Brown wood color
            metallic: 0.0,
            perceptual_roughness: 0.9,
            alpha_mode: AlphaMode::Opaque,
        },
    ))
    .observe(apply_pending_material);

    log::info!("Table entity spawned at position (3, 0, 0)");
}

pub fn update(_commands: Commands, _simulation_state: ResMut<SimulationConfig>, _time: Res<Time>) {
    // Update the simulation state based on the elapsed time
    // simulation_state.update(time.delta_seconds());
}

pub fn show_interaction_text(
    mut commands: Commands,
    sphere_query: Query<(Entity, &Transform), (With<Interactable>, With<BeingInteractedWith>)>,
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

pub fn update_outline(
    mut commands: Commands,
    interactable_query: Query<
        (Entity, Option<&BeingInteractedWith>, Option<&Children>),
        (With<Interactable>, Changed<BeingInteractedWith>),
    >,
    all_interactables: Query<(Entity, Option<&Children>), With<Interactable>>,
    mut removed_interactions: RemovedComponents<BeingInteractedWith>,
    mesh_query: Query<Entity, With<Mesh3d>>,
    children_query: Query<&Children>,
) {
    // Handle entities that are being interacted with (add white outline)
    for (entity, interaction, children) in interactable_query.iter() {
        if interaction.is_some() {
            // Add outline to the main entity
            commands.entity(entity).insert(OutlineVolume {
                visible: true,
                width: 3.0,
                colour: Color::srgb(1.0, 1.0, 1.0), // White outline
            });

            // If entity has children (like GLTF scenes), propagate outline to mesh children
            if let Some(children) = children {
                propagate_outline_to_children(
                    &mut commands,
                    children,
                    true,
                    &mesh_query,
                    &children_query,
                );
            }
        }
    }

    // Handle entities that are no longer being interacted with (remove outline)
    for entity in removed_interactions.read() {
        commands.entity(entity).remove::<OutlineVolume>();

        // Also remove outlines from children if this is a scene
        if let Ok((_, children)) = all_interactables.get(entity) {
            if let Some(children) = children {
                propagate_outline_to_children(
                    &mut commands,
                    children,
                    false,
                    &mesh_query,
                    &children_query,
                );
            }
        }
    }
}

fn propagate_outline_to_children(
    commands: &mut Commands,
    children: &Children,
    add_outline: bool,
    mesh_query: &Query<Entity, With<Mesh3d>>,
    children_query: &Query<&Children>,
) {
    for child in children.iter() {
        // If this child is a mesh, add or remove outline
        if mesh_query.contains(child) {
            if add_outline {
                commands.entity(child).insert(OutlineVolume {
                    visible: true,
                    width: 3.0,
                    colour: Color::srgb(1.0, 1.0, 1.0),
                });
            } else {
                commands.entity(child).remove::<OutlineVolume>();
            }
        }

        // Recursively check grandchildren
        if let Ok(grandchildren) = children_query.get(child) {
            propagate_outline_to_children(
                commands,
                grandchildren,
                add_outline,
                mesh_query,
                children_query,
            );
        }
    }
}

pub fn toggle_crosshair(mut settings: ResMut<UserSettings>, input: Res<ButtonInput<KeyCode>>) {
    if input.just_pressed(KeyCode::KeyT)
        && (input.pressed(KeyCode::ShiftLeft) || input.pressed(KeyCode::ShiftRight))
    {
        settings.toggle_crosshair();
    }
}

pub fn toggle_edit_mode(mut settings: ResMut<UserSettings>, input: Res<ButtonInput<KeyCode>>) {
    if input.just_pressed(KeyCode::KeyE)
        && (input.pressed(KeyCode::ShiftLeft) || input.pressed(KeyCode::ShiftRight))
    {
        settings.toggle_edit_mode();
    }
}

pub fn toggle_placement_object(
    mut settings: ResMut<UserSettings>,
    input: Res<ButtonInput<KeyCode>>,
    mut preview_query: Query<(Entity, Option<&mut PendingMaterial>, Option<&Children>), With<PlacementPreview>>,
    children_query: Query<&Children>,
    mut mesh_material_query: Query<&mut MeshMaterial3d<StandardMaterial>, With<Mesh3d>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    if input.just_pressed(KeyCode::KeyR)
        && (input.pressed(KeyCode::ShiftLeft) || input.pressed(KeyCode::ShiftRight))
    {
        settings.cycle_placement_object();
        
        // Update preview material color
        let new_color = match settings.current_placement_object {
            PlacementObjectType::Table => Color::srgba(0.6, 0.4, 0.2, 0.5), // Brown
            PlacementObjectType::Cooker => Color::srgba(0.9, 0.2, 0.2, 0.5), // Red
            PlacementObjectType::Fridge => Color::srgba(0.2, 0.4, 0.8, 0.5), // Blue
            PlacementObjectType::CuttingBoard => Color::srgba(0.2, 0.8, 0.3, 0.5), // Green
        };
        
        // Update any preview entities
        for (_entity, pending_material, children) in preview_query.iter_mut() {
            if let Some(mut pending) = pending_material {
                // Update the pending material component
                pending.base_color = new_color;
            } else if let Some(children) = children {
                // Update existing materials on mesh children
                update_mesh_materials_color(
                    children,
                    new_color,
                    &children_query,
                    &mut mesh_material_query,
                    &mut materials,
                );
            }
        }
    }
}

fn update_mesh_materials_color(
    children: &Children,
    new_color: Color,
    children_query: &Query<&Children>,
    mesh_material_query: &mut Query<&mut MeshMaterial3d<StandardMaterial>, With<Mesh3d>>,
    materials: &mut ResMut<Assets<StandardMaterial>>,
) {
    for child in children.iter() {
        if let Ok(mesh_material) = mesh_material_query.get_mut(child) {
            // Update the material's color
            if let Some(material) = materials.get_mut(&mesh_material.0) {
                material.base_color = new_color;
            }
        }
        
        // Recursively update grandchildren
        if let Ok(grandchildren) = children_query.get(child) {
            update_mesh_materials_color(
                grandchildren,
                new_color,
                children_query,
                mesh_material_query,
                materials,
            );
        }
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

// pub fn check_interactions(
//     mut commands: Commands,
//     camera_query: Query<&Transform, With<PlayerCamera>>,
//     interactable_query: Query<(Entity, &Transform, &Interactable), Without<PlayerCamera>>,
//     marked_query: Query<Entity, With<BeingInteractedWith>>,
//     mut raycast: Raycast,
// ) {
//     // Clear all existing markers first
//     for entity in marked_query.iter() {
//         commands.entity(entity).remove::<BeingInteractedWith>();
//     }

//     if let Ok(camera_transform) = camera_query.single() {
//         let camera_pos = camera_transform.translation;
//         let camera_forward = camera_transform.forward().as_vec3();

//         // Cast ray from camera
//         let ray = Ray3d::new(camera_pos, camera_forward.into());

//         let mut closest_interaction: Option<(Entity, f32, &str)> = None;

//         // Use Bevy's raycast system to check for intersections
//         if let Some(hits) = raycast.cast_ray(ray, &RaycastSettings::default()) {
//             for (entity, hit) in hits.iter() {
//                 // Check if this entity is interactable
//                 if let Ok((_, object_transform, interactable)) = interactable_query.get(*entity) {
//                     let object_pos = object_transform.translation;

//                     // Check distance constraint
//                     let floor_distance = (Vec2::new(camera_pos.x, camera_pos.z) - Vec2::new(object_pos.x, object_pos.z)).length();

//                     if floor_distance <= interactable.distance && hit.distance <= interactable.distance {
//                         match closest_interaction {
//                             None => closest_interaction = Some((*entity, hit.distance, &interactable.name)),
//                             Some((_, current_closest_distance, _)) => {
//                                 if hit.distance < current_closest_distance {
//                                     closest_interaction = Some((*entity, hit.distance, &interactable.name));
//                                 }
//                             }
//                         }
//                     }
//                 }
//             }
//         }

//         // Mark the closest interactable object
//         if let Some((entity, _, name)) = closest_interaction {
//             commands.entity(entity).insert(BeingInteractedWith);
//             info!("Player is interacting with: {}", name);
//         }
//     }
// }

pub fn exit(
    mut commands: Commands,
    mut windows: Query<&mut Window>,
    game_entities: Query<Entity, With<GameEntity>>,
) {
    // Cleanup all game entities
    for entity in game_entities.iter() {
        commands.entity(entity).despawn();
    }

    // Cleanup the simulation environment
    commands.remove_resource::<SimulationConfig>();

    // Unlock cursor when exiting game
    if let Ok(mut window) = windows.single_mut() {
        window.cursor_options.grab_mode = bevy::window::CursorGrabMode::None;
        window.cursor_options.visible = true;
    }
}

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

    // Create main container positioned in top right
    commands
        .spawn((
            GameEntity,
            KeybindsDisplay,
            Node {
                position_type: PositionType::Absolute,
                top: Val::Px(20.0),
                right: Val::Px(20.0),
                flex_direction: FlexDirection::Column,
                row_gap: Val::Px(8.0),
                padding: UiRect::all(Val::Px(12.0)),
                ..default()
            },
            BackgroundColor(Color::srgba(0.0, 0.0, 0.0, 0.7)), // Semi-transparent black background
        ))
        .with_children(|parent| {
            // Title
            parent.spawn((
                Text::new("Controls"),
                TextFont {
                    font_size: 16.0,
                    ..default()
                },
                TextColor(Color::srgb(1.0, 1.0, 1.0)),
            ));

            // Add each setting
            for setting in settings_info {
                parent
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
                                font_size: 12.0,
                                ..default()
                            },
                            TextColor(Color::srgb(0.8, 0.8, 0.8)),
                        ));

                        // Current value
                        row.spawn((
                            Text::new(setting.current_value),
                            TextFont {
                                font_size: 12.0,
                                ..default()
                            },
                            TextColor(Color::srgb(0.4, 1.0, 0.4)), // Green for values
                        ));
                    });
            }
        });
}

pub fn check_interactions(
    mut commands: Commands,
    camera_query: Query<&Transform, With<super::camera::PlayerCamera>>,
    interactable_query: Query<
        (
            Entity,
            &Transform,
            &Interactable,
            Option<&InteractableParent>,
        ),
        Without<super::camera::PlayerCamera>,
    >,
    marked_query: Query<Entity, With<BeingInteractedWith>>,
    mut raycast: MeshRayCast,
) {
    let mut current_interaction: Option<Entity> = None;

    if let Ok(camera_transform) = camera_query.single() {
        let camera_pos = camera_transform.translation;
        let camera_forward = camera_transform.forward().as_vec3();

        // Create ray from camera
        let ray = Ray3d::new(camera_pos, Dir3::new(camera_forward).unwrap());

        // Cast ray using Bevy's proper mesh raycasting
        let hits = raycast.cast_ray(ray, &Default::default());

        let mut closest_hit: Option<(Entity, f32, String)> = None;

        // Log hit count for debugging
        if !hits.is_empty() {
            log::trace!("Ray hit {} entities", hits.len());
        }

        // Check if any of the hits are interactable entities
        for (entity, hit_data) in hits.iter() {
            // First check if this entity has Interactable
            if let Ok((_, object_transform, interactable, parent_ref)) =
                interactable_query.get(*entity)
            {
                // Use parent entity if this is a child mesh, otherwise use the entity itself
                let target_entity = parent_ref.map(|p| p.0).unwrap_or(*entity);
                
                // For distance calculation, use parent position if available, otherwise child position
                let reference_pos = if let Some(parent) = parent_ref {
                    // Get parent's position for consistent distance calculation
                    if let Ok((_, parent_transform, _, _)) = interactable_query.get(parent.0) {
                        parent_transform.translation
                    } else {
                        object_transform.translation
                    }
                } else {
                    object_transform.translation
                };

                // Check distance constraint using the reference position
                let floor_distance = (Vec2::new(camera_pos.x, camera_pos.z)
                    - Vec2::new(reference_pos.x, reference_pos.z))
                .length();

                if floor_distance <= interactable.distance
                    && hit_data.distance <= interactable.distance
                {
                    let name = interactable.name.clone();

                    match closest_hit {
                        None => {
                            closest_hit = Some((target_entity, hit_data.distance, name.clone()));
                            log::trace!("Found interactable: {} at distance {}", name, hit_data.distance);
                        }
                        Some((_, current_distance, _)) => {
                            if hit_data.distance < current_distance {
                                closest_hit = Some((target_entity, hit_data.distance, name.clone()));
                                log::trace!("Found closer interactable: {} at distance {}", name, hit_data.distance);
                            }
                        }
                    }
                }
            } else {
                log::trace!("Hit entity {:?} but it has no Interactable component", entity);
            }
        }

        // Store the entity we're currently interacting with
        if let Some((entity, _, name)) = closest_hit {
            current_interaction = Some(entity);
            log::trace!("Current interaction target: {} (entity {:?})", name, entity);
        }
    }

    // Remove BeingInteractedWith from entities that are no longer being looked at
    for entity in marked_query.iter() {
        if Some(entity) != current_interaction {
            commands.entity(entity).remove::<BeingInteractedWith>();
        }
    }

    // Add BeingInteractedWith to the new interaction target (if any)
    if let Some(entity) = current_interaction {
        // Only insert if it doesn't already have it
        if !marked_query.contains(entity) {
            commands.entity(entity).insert(BeingInteractedWith);
        }
    }
}

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
    // Create main container positioned in top left
    commands
        .spawn((
            GameEntity,
            FpsDisplay,
            Node {
                position_type: PositionType::Absolute,
                top: Val::Px(20.0),
                left: Val::Px(20.0),
                flex_direction: FlexDirection::Column,
                padding: UiRect::all(Val::Px(12.0)),
                ..default()
            },
            BackgroundColor(Color::srgba(0.0, 0.0, 0.0, 0.7)), // Semi-transparent black background
        ))
        .with_children(|parent| {
            // FPS text
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

pub fn propagate_interactable_to_children(
    mut commands: Commands,
    interactable_scene_query: Query<
        (Entity, &Children, &Interactable),
        With<SceneRoot>,
    >,
    mesh_query: Query<Entity, (With<Mesh3d>, Without<Interactable>)>,
    children_query: Query<&Children>,
) {
    for (scene_entity, children, interactable) in interactable_scene_query.iter() {
        // Check if this scene has any mesh children yet
        let has_meshes = check_has_mesh_children(children, &mesh_query, &children_query);
        
        if has_meshes {
            // Recursively find all mesh children and add Interactable component
            propagate_to_mesh_children(
                &mut commands,
                children,
                interactable,
                scene_entity,
                &mesh_query,
                &children_query,
            );
            
            // Mark this scene as processed so we don't try again
            // No longer need ProcessedScene marker - using observer pattern
            
            log::info!("Processed scene entity {:?} - added Interactable to mesh children", scene_entity);
        }
    }
}

fn check_has_mesh_children(
    children: &Children,
    mesh_query: &Query<Entity, (With<Mesh3d>, Without<Interactable>)>,
    children_query: &Query<&Children>,
) -> bool {
    for child in children.iter() {
        if mesh_query.contains(child) {
            return true;
        }
        
        // Recursively check grandchildren
        if let Ok(grandchildren) = children_query.get(child) {
            if check_has_mesh_children(grandchildren, mesh_query, children_query) {
                return true;
            }
        }
    }
    false
}

fn propagate_to_mesh_children(
    commands: &mut Commands,
    children: &Children,
    interactable: &Interactable,
    parent_entity: Entity,
    mesh_query: &Query<Entity, (With<Mesh3d>, Without<Interactable>)>,
    children_query: &Query<&Children>,
) {
    for child in children.iter() {
        // If this child is a mesh without Interactable, add it and mark parent
        if mesh_query.contains(child) {
            commands
                .entity(child)
                .insert((interactable.clone(), InteractableParent(parent_entity)));
        }

        // Recursively check grandchildren
        if let Ok(grandchildren) = children_query.get(child) {
            propagate_to_mesh_children(
                commands,
                grandchildren,
                interactable,
                parent_entity,
                mesh_query,
                children_query,
            );
        }
    }
}

pub fn setup_interaction_info_display(mut commands: Commands) {
    create_interaction_info_display(&mut commands, None);
}

pub fn update_interaction_info_display(
    mut commands: Commands,
    interaction_info_query: Query<Entity, With<InteractionInfoDisplay>>,
    interactable_query: Query<&Interactable, With<BeingInteractedWith>>,
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

    // Create main container positioned in top right, below the controls
    commands
        .spawn((
            GameEntity,
            InteractionInfoDisplay,
            Node {
                position_type: PositionType::Absolute,
                top: Val::Px(180.0), // Position below the controls box
                right: Val::Px(20.0),
                flex_direction: FlexDirection::Column,
                padding: UiRect::all(Val::Px(12.0)),
                ..default()
            },
            BackgroundColor(Color::srgba(0.0, 0.0, 0.0, 0.7)), // Semi-transparent black background
        ))
        .with_children(|parent| {
            // Interaction text
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

pub fn setup_position_display(mut commands: Commands) {
    create_position_display(&mut commands, Vec3::ZERO);
}

pub fn update_position_display(
    mut commands: Commands,
    position_query: Query<Entity, With<PositionDisplay>>,
    camera_query: Query<&Transform, With<super::camera::PlayerCamera>>,
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
    // Create main container positioned in top right, below the interaction info
    commands
        .spawn((
            GameEntity,
            PositionDisplay,
            Node {
                position_type: PositionType::Absolute,
                top: Val::Px(240.0), // Position below the interaction info box
                right: Val::Px(20.0),
                flex_direction: FlexDirection::Column,
                row_gap: Val::Px(4.0),
                padding: UiRect::all(Val::Px(12.0)),
                ..default()
            },
            BackgroundColor(Color::srgba(0.0, 0.0, 0.0, 0.7)), // Semi-transparent black background
        ))
        .with_children(|parent| {
            // Title
            parent.spawn((
                Text::new("Position"),
                TextFont {
                    font_size: 14.0,
                    ..default()
                },
                TextColor(Color::srgb(1.0, 1.0, 1.0)),
            ));

            // Position coordinates
            parent.spawn((
                Text::new(format!("X: {:.1}", position.x)),
                TextFont {
                    font_size: 12.0,
                    ..default()
                },
                TextColor(Color::srgb(0.8, 0.8, 0.8)),
            ));

            parent.spawn((
                Text::new(format!("Y: {:.1}", position.y)),
                TextFont {
                    font_size: 12.0,
                    ..default()
                },
                TextColor(Color::srgb(0.8, 0.8, 0.8)),
            ));

            parent.spawn((
                Text::new(format!("Z: {:.1}", position.z)),
                TextFont {
                    font_size: 12.0,
                    ..default()
                },
                TextColor(Color::srgb(0.8, 0.8, 0.8)),
            ));
        });
}

// Removed override_object_materials - replaced by apply_pending_material observer

pub fn handle_edit_mode_click(
    mut commands: Commands,
    settings: Res<UserSettings>,
    mouse_input: Res<ButtonInput<MouseButton>>,
    preview_entity: Res<PlacementPreviewEntity>,
    preview_query: Query<&Transform, With<PlacementPreview>>,
    camera_query: Query<&Transform, With<super::camera::PlayerCamera>>,
    mut raycast: MeshRayCast,
    asset_server: Res<AssetServer>,
) {
    // Only proceed if edit mode is active and left mouse button was just clicked
    if !settings.edit_mode || !mouse_input.just_pressed(MouseButton::Left) {
        return;
    }

    // Try to get position from preview entity first
    let mut position = None;
    if let Some(entity) = preview_entity.0 {
        if let Ok(preview_transform) = preview_query.get(entity) {
            position = Some(preview_transform.translation);
        }
    }

    // Fall back to raycast if no preview position
    if position.is_none() {
        if let Ok(camera_transform) = camera_query.single() {
            let camera_pos = camera_transform.translation;
            let camera_forward = camera_transform.forward().as_vec3();

            // Create ray from camera
            let ray = Ray3d::new(camera_pos, Dir3::new(camera_forward).unwrap());

            // Cast ray to find ground intersection
            let hits = raycast.cast_ray(ray, &Default::default());

            // Find the ground plane hit (y = 0)
            for (_, hit_data) in hits.iter() {
                if hit_data.point.y < 1.0 {
                    position = Some(hit_data.point);
                    break;
                }
            }

            // If no mesh hit, calculate ground intersection manually
            if position.is_none() && camera_forward.y < 0.0 {
                let t = -camera_pos.y / camera_forward.y;
                if t > 0.0 {
                    let intersection = camera_pos + camera_forward * t;
                    position = Some(intersection);
                }
            }
        }
    }

    // Spawn object at determined location
    if let Some(pos) = position {
        let table_scene: Handle<Scene> = asset_server.load("meshes/table/result.gltf#Scene0");

        // Determine object properties based on current placement type
        let name = match settings.current_placement_object {
            PlacementObjectType::Table => "Wooden Table",
            PlacementObjectType::Cooker => "Cooker",
            PlacementObjectType::Fridge => "Fridge",
            PlacementObjectType::CuttingBoard => "Cutting Board",
        };

        let mut entity_commands = commands.spawn((
            GameEntity,
            Interactable {
                name: name.to_string(),
                distance: 10.0,
                angle_threshold: None,
            },
            SceneRoot(table_scene),
            Transform::from_translation(Vec3::new(pos.x, 0.0, pos.z))
                .with_scale(Vec3::splat(0.02)),
            GlobalTransform::default(),
        ));

        // Add the appropriate marker components and material
        match settings.current_placement_object {
            PlacementObjectType::Table => {
                entity_commands.insert((TableMarker, InteractableType::Table));
                entity_commands.insert(PendingMaterial {
                    base_color: Color::srgb(0.6, 0.4, 0.2),
                    metallic: 0.0,
                    perceptual_roughness: 0.9,
                    alpha_mode: AlphaMode::Opaque,
                });
            },
            PlacementObjectType::Cooker => {
                entity_commands.insert((CookerMarker, InteractableType::Cooker));
                entity_commands.insert(PendingMaterial {
                    base_color: Color::srgb(0.9, 0.2, 0.2),
                    metallic: 0.3,
                    perceptual_roughness: 0.5,
                    alpha_mode: AlphaMode::Opaque,
                });
            },
            PlacementObjectType::Fridge => {
                entity_commands.insert((FridgeMarker, InteractableType::Fridge));
                entity_commands.insert(PendingMaterial {
                    base_color: Color::srgb(0.2, 0.4, 0.8),
                    metallic: 0.5,
                    perceptual_roughness: 0.3,
                    alpha_mode: AlphaMode::Opaque,
                });
            },
            PlacementObjectType::CuttingBoard => {
                entity_commands.insert((CuttingBoardMarker, InteractableType::CuttingBoard));
                entity_commands.insert(PendingMaterial {
                    base_color: Color::srgb(0.2, 0.8, 0.3),
                    metallic: 0.0,
                    perceptual_roughness: 0.8,
                    alpha_mode: AlphaMode::Opaque,
                });
            },
        };
        
        // Add the material observer
        entity_commands.observe(apply_pending_material);

        log::info!(
            "Spawned {} at position ({:.1}, 0.0, {:.1})",
            name,
            pos.x,
            pos.z
        );
    }
}

pub fn setup_placement_preview(
    mut commands: Commands,
    mut preview_entity: ResMut<PlacementPreviewEntity>,
    asset_server: Res<AssetServer>,
    settings: Res<UserSettings>,
) {
    // Only setup preview if edit mode just turned on and we don't already have one
    if !settings.is_changed() || !settings.edit_mode || preview_entity.0.is_some() {
        return;
    }

    // Load the table mesh (same as actual placement)
    let table_scene: Handle<Scene> = asset_server.load("meshes/table/result.gltf#Scene0");

    // Get the color for the current placement type
    let base_color = match settings.current_placement_object {
        PlacementObjectType::Table => Color::srgba(0.6, 0.4, 0.2, 0.5), // Brown
        PlacementObjectType::Cooker => Color::srgba(0.9, 0.2, 0.2, 0.5), // Red
        PlacementObjectType::Fridge => Color::srgba(0.2, 0.4, 0.8, 0.5), // Blue
        PlacementObjectType::CuttingBoard => Color::srgba(0.2, 0.8, 0.3, 0.5), // Green
    };

    // Spawn preview entity
    let entity = commands.spawn((
        GameEntity,
        PlacementPreview,
        SceneRoot(table_scene),
        Transform::from_translation(Vec3::new(0.0, 0.05, 0.0)) // Slightly elevated to prevent z-fighting
            .with_scale(Vec3::splat(0.02)),
        GlobalTransform::default(),
        Visibility::Hidden, // Start hidden until we have a valid position
        PendingMaterial {
            base_color,
            metallic: 0.0,
            perceptual_roughness: 0.9,
            alpha_mode: AlphaMode::Blend,
        },
    ))
    .observe(apply_pending_material)
    .id();

    // Store the entity for later reference
    preview_entity.0 = Some(entity);

    log::info!("Placement preview entity created");
}

pub fn cleanup_placement_preview(
    mut commands: Commands,
    mut preview_entity: ResMut<PlacementPreviewEntity>,
    settings: Res<UserSettings>,
) {
    // Only cleanup if edit mode just turned off and we have a preview
    if !settings.is_changed() || settings.edit_mode || preview_entity.0.is_none() {
        return;
    }

    if let Some(entity) = preview_entity.0.take() {
        commands.entity(entity).despawn();
        log::info!("Placement preview entity removed");
    }
}

pub fn update_placement_preview(
    mut preview_query: Query<(&mut Transform, &mut Visibility), With<PlacementPreview>>,
    camera_query: Query<&Transform, (With<super::camera::PlayerCamera>, Without<PlacementPreview>)>,
    mut raycast: MeshRayCast,
    preview_entity: Res<PlacementPreviewEntity>,
    settings: Res<UserSettings>,
) {
    // Only update if we're in edit mode and have a preview entity
    if !settings.edit_mode || preview_entity.0.is_none() {
        return;
    }

    let entity = preview_entity.0.unwrap();
    
    if let Ok((mut preview_transform, mut visibility)) = preview_query.get_mut(entity) {
        if let Ok(camera_transform) = camera_query.single() {
            let camera_pos = camera_transform.translation;
            let camera_forward = camera_transform.forward().as_vec3();

            // Create ray from camera
            let ray = Ray3d::new(camera_pos, Dir3::new(camera_forward).unwrap());

            // Cast ray to find ground intersection
            let hits = raycast.cast_ray(ray, &Default::default());

            // Find the ground plane hit (y = 0)
            let mut ground_hit: Option<Vec3> = None;

            for (_, hit_data) in hits.iter() {
                // Check if this is roughly at ground level
                if hit_data.point.y < 1.0 {
                    ground_hit = Some(hit_data.point);
                    break;
                }
            }

            // If no mesh hit, calculate ground intersection manually
            if ground_hit.is_none() && camera_forward.y < 0.0 {
                // Calculate where ray intersects y=0 plane
                let t = -camera_pos.y / camera_forward.y;
                if t > 0.0 {
                    let intersection = camera_pos + camera_forward * t;
                    ground_hit = Some(intersection);
                }
            }

            // Update preview position
            if let Some(position) = ground_hit {
                preview_transform.translation = Vec3::new(position.x, 0.05, position.z); // Slightly elevated
                *visibility = Visibility::Visible;
            } else {
                *visibility = Visibility::Hidden;
            }
        }
    }
}

// Removed apply_preview_material system - now handled by apply_pending_material observer

pub fn handle_cutting_board_interaction(
    mut commands: Commands,
    mouse_input: Res<ButtonInput<MouseButton>>,
    camera_query: Query<&Transform, With<super::camera::PlayerCamera>>,
    cutting_board_query: Query<(Entity, &Transform, &InteractableType), With<BeingInteractedWith>>,
    mut active_menu: ResMut<ActiveCuttingBoardMenu>,
    menu_owner_query: Query<&MenuOwner, With<CuttingBoardMenu>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    _asset_server: Res<AssetServer>,
) {
    // Only proceed on left click
    if !mouse_input.just_pressed(MouseButton::Left) {
        return;
    }
    
    // Check if we're interacting with a cutting board
    for (entity, cutting_board_transform, interactable_type) in cutting_board_query.iter() {
        if matches!(interactable_type, InteractableType::CuttingBoard) {
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
            
            // Get camera transform to face the menu towards the player
            if let Ok(camera_transform) = camera_query.single() {
                let menu_position = cutting_board_transform.translation + Vec3::Y * 2.0; // 2 units above cutting board
                
                // Calculate rotation to face the camera
                let to_camera = (camera_transform.translation - menu_position).normalize();
                // Calculate rotation to face camera horizontally
                let forward = Vec3::new(to_camera.x, 0.0, to_camera.z).normalize();
                let menu_rotation = Quat::from_rotation_arc(Vec3::Z, forward);
                
                // Create menu background
                let menu_entity = commands.spawn((
                    GameEntity,
                    CuttingBoardMenu,
                    MenuOwner(entity),
                    Mesh3d(meshes.add(Plane3d::new(Vec3::Z, Vec2::new(4.0, 2.0)))), // Z normal faces forward
                    MeshMaterial3d(materials.add(StandardMaterial {
                        base_color: Color::srgba(0.1, 0.1, 0.1, 0.9),
                        alpha_mode: AlphaMode::Blend,
                        double_sided: true,
                        ..default()
                    })),
                    Transform::from_translation(menu_position)
                        .with_rotation(menu_rotation),
                    GlobalTransform::default(),
                ))
                .with_children(|parent| {
                    // Cake Slice Card
                    parent.spawn((
                        RecipeCard { recipe_name: "Cake Slice".to_string() },
                        Mesh3d(meshes.add(Plane3d::new(Vec3::Z, Vec2::new(1.5, 1.5)))), // Z normal faces forward
                        MeshMaterial3d(materials.add(StandardMaterial {
                            base_color: Color::srgb(0.9, 0.7, 0.4), // Light brown for cake
                            double_sided: true,
                            ..default()
                        })),
                        Transform::from_translation(Vec3::new(-1.0, 0.0, 0.1)),
                    ))
                    .with_children(|card| {
                        // Cake slice text (using Text2d for 3D world)
                        card.spawn((
                            Text2d::new("Cake\nSlice"),
                            TextFont {
                                font_size: 40.0,
                                ..default()
                            },
                            TextColor(Color::srgb(0.2, 0.1, 0.1)),
                            Transform::from_translation(Vec3::new(0.0, 0.0, 0.01)), // Small offset from card
                        ));
                    });
                    
                    // Cupcake Card
                    parent.spawn((
                        RecipeCard { recipe_name: "Cupcake".to_string() },
                        Mesh3d(meshes.add(Plane3d::new(Vec3::Z, Vec2::new(1.5, 1.5)))), // Z normal faces forward
                        MeshMaterial3d(materials.add(StandardMaterial {
                            base_color: Color::srgb(0.9, 0.4, 0.7), // Pink for cupcake
                            double_sided: true,
                            ..default()
                        })),
                        Transform::from_translation(Vec3::new(1.0, 0.0, 0.1)),
                    ))
                    .with_children(|card| {
                        // Cupcake text (using Text2d for 3D world)
                        card.spawn((
                            Text2d::new("Cupcake"),
                            TextFont {
                                font_size: 40.0,
                                ..default()
                            },
                            TextColor(Color::srgb(0.2, 0.1, 0.1)),
                            Transform::from_translation(Vec3::new(0.0, 0.0, 0.01)), // Small offset from card
                        ));
                    });
                })
                .id();
                
                // Store the active menu
                active_menu.0 = Some(menu_entity);
            }
            
            break; // Only handle one cutting board at a time
        }
    }
}


pub fn update_cutting_board_menu_rotation(
    camera_query: Query<&Transform, With<super::camera::PlayerCamera>>,
    mut menu_query: Query<&mut Transform, (With<CuttingBoardMenu>, Without<super::camera::PlayerCamera>)>,
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
