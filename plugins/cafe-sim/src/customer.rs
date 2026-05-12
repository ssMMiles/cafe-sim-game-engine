//! NPC customers — initial vertical slice.
//!
//! Spawns floating red cubes at the front edge of the cafe when it's open and
//! has at least one [`Countertop`] and one [`CoffeeMachine`] placed. Each
//! customer walks straight-line to a counter cell (cells are reserved so two
//! customers don't pile onto the same one), then displays its order as a
//! screen-space popup positioned above its head. No serve/complete logic yet.

use bevy::prelude::*;
use bevy::window::PrimaryWindow;
use engine_api::PlayerCamera;
use rand::Rng;
use rand::seq::SliceRandom;
use std::collections::HashSet;

use crate::cafe::CafeOpen;
use crate::grid::{FLOOR_Y, HALF_D, HALF_W, cell_center};
use crate::pause_menu::GamePaused;
use crate::placement::{CoffeeMachine, Countertop, PlacedCell};
use crate::spawn::{SpawnOwnedCommands, WithChildOwned};

const MAX_CUSTOMERS: usize = 3;
const SPAWN_COOLDOWN_SECS: f32 = 3.0;
const WALK_SPEED: f32 = 1.5;
const ARRIVE_DISTANCE: f32 = 0.1;
const CUSTOMER_FLOAT_Y: f32 = FLOOR_Y + 0.6;
const CUSTOMER_SIZE: f32 = 0.4;
/// How far in front of the counter (in the +Z customer-approach direction) a
/// customer stands while waiting. Slightly more than half a cell + customer
/// radius so the cube doesn't clip the counter mesh.
const COUNTER_STAND_OFFSET: f32 = 0.7;
const POPUP_OFFSET_Y: f32 = 0.9;
const POPUP_FONT_SIZE: f32 = 16.0;

const MENU: &[&str] = &[
    "Coffee",
    "Latte",
    "Tea",
    "Espresso",
    "Cappuccino",
    "Hot Chocolate",
];

#[derive(Resource)]
pub struct CustomerAssets {
    pub mesh: Handle<Mesh>,
    pub material: Handle<StandardMaterial>,
}

#[derive(Resource, Default)]
pub struct CustomerSpawnTimer {
    pub remaining: f32,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum CustomerState {
    Walking,
    Waiting,
}

#[derive(Component)]
pub struct Customer {
    pub state: CustomerState,
    pub target_cell: (i32, i32),
    pub target: Vec3,
    pub order: &'static str,
}

/// Screen-space UI label that follows a specific customer's head.
#[derive(Component)]
pub struct CustomerPopup {
    pub customer: Entity,
}

/// Build the cached cube mesh + red unlit material once at plugin setup so the
/// spawn system doesn't re-add identical assets every customer.
pub fn init_customer_assets(world: &mut World) {
    let mesh = world
        .resource_mut::<Assets<Mesh>>()
        .add(Cuboid::from_length(CUSTOMER_SIZE));
    let material = world
        .resource_mut::<Assets<StandardMaterial>>()
        .add(StandardMaterial {
            base_color: Color::srgb(1.0, 0.2, 0.2),
            unlit: true,
            ..default()
        });
    world.insert_resource(CustomerAssets { mesh, material });
}

pub fn tick_spawn_timer(
    time: Res<Time>,
    paused: Res<GamePaused>,
    mut timer: ResMut<CustomerSpawnTimer>,
) {
    if paused.0 {
        return;
    }
    if timer.remaining > 0.0 {
        timer.remaining = (timer.remaining - time.delta_secs()).max(0.0);
    }
}

pub fn spawn_customers(
    mut commands: Commands,
    paused: Res<GamePaused>,
    cafe_open: Res<CafeOpen>,
    assets: Res<CustomerAssets>,
    mut timer: ResMut<CustomerSpawnTimer>,
    customers_q: Query<&Customer>,
    counters_q: Query<&PlacedCell, With<Countertop>>,
    machines_q: Query<(), With<CoffeeMachine>>,
) {
    if paused.0 || !cafe_open.0 || timer.remaining > 0.0 {
        return;
    }
    if customers_q.iter().count() >= MAX_CUSTOMERS {
        return;
    }
    if machines_q.is_empty() {
        return;
    }

    let reserved: HashSet<(i32, i32)> = customers_q.iter().map(|c| c.target_cell).collect();
    let available: Vec<PlacedCell> = counters_q
        .iter()
        .copied()
        .filter(|c| !reserved.contains(&(c.0, c.1)))
        .collect();
    if available.is_empty() {
        return;
    }

    let mut rng = rand::thread_rng();
    let target_cell = *available.choose(&mut rng).expect("available non-empty");
    let counter_center = cell_center(target_cell.0, target_cell.1);
    let target = Vec3::new(
        counter_center.x,
        CUSTOMER_FLOAT_Y,
        counter_center.z + COUNTER_STAND_OFFSET,
    );
    let order = *MENU.choose(&mut rng).unwrap_or(&"Coffee");

    let half_w = HALF_W as f32;
    let spawn_x = rng.gen_range(-half_w + 1.0..half_w - 1.0);
    let spawn_z = HALF_D as f32 + 1.0;
    let spawn_pos = Vec3::new(spawn_x, CUSTOMER_FLOAT_Y, spawn_z);

    commands.spawn_owned((
        Customer {
            state: CustomerState::Walking,
            target_cell: (target_cell.0, target_cell.1),
            target,
            order,
        },
        Mesh3d(assets.mesh.clone()),
        MeshMaterial3d(assets.material.clone()),
        Transform::from_translation(spawn_pos),
    ));

    timer.remaining = SPAWN_COOLDOWN_SECS;
}

pub fn walk_customers(
    time: Res<Time>,
    paused: Res<GamePaused>,
    mut q: Query<(&mut Customer, &mut Transform)>,
) {
    if paused.0 {
        return;
    }
    let dt = time.delta_secs();
    for (mut customer, mut transform) in &mut q {
        if customer.state != CustomerState::Walking {
            continue;
        }
        let to_target = customer.target - transform.translation;
        let flat = Vec3::new(to_target.x, 0.0, to_target.z);
        let dist = flat.length();
        if dist <= ARRIVE_DISTANCE {
            customer.state = CustomerState::Waiting;
            continue;
        }
        let step = (flat / dist) * WALK_SPEED * dt;
        transform.translation.x += step.x;
        transform.translation.z += step.z;
    }
}

/// Spawn a popup `Node` for any customer that doesn't have one yet, and
/// despawn popups whose customer entity is gone. Popup positioning happens in
/// `position_customer_popups`.
pub fn manage_customer_popups(
    mut commands: Commands,
    customers_q: Query<(Entity, &Customer)>,
    popups_q: Query<(Entity, &CustomerPopup)>,
) {
    let already_has_popup: HashSet<Entity> = popups_q.iter().map(|(_, p)| p.customer).collect();
    let live_customers: HashSet<Entity> = customers_q.iter().map(|(e, _)| e).collect();

    for (popup_entity, popup) in popups_q.iter() {
        if !live_customers.contains(&popup.customer) {
            commands.entity(popup_entity).despawn();
        }
    }

    for (customer_entity, customer) in customers_q.iter() {
        if already_has_popup.contains(&customer_entity) {
            continue;
        }
        commands
            .spawn_owned((
                CustomerPopup {
                    customer: customer_entity,
                },
                Node {
                    position_type: PositionType::Absolute,
                    padding: UiRect::axes(Val::Px(8.0), Val::Px(4.0)),
                    border_radius: BorderRadius::all(Val::Px(4.0)),
                    ..default()
                },
                BackgroundColor(Color::srgba(0.0, 0.0, 0.0, 0.7)),
                Visibility::Hidden,
                GlobalZIndex(85),
            ))
            .with_child_owned((
                Text::new(customer.order),
                TextFont {
                    font_size: POPUP_FONT_SIZE,
                    ..default()
                },
                TextColor(Color::srgb(1.0, 1.0, 1.0)),
            ));
    }
}

/// Each frame, project each waiting customer's head position through the
/// in-game camera and write it back to the corresponding popup Node's `top`/
/// `left`. Walking customers (or customers behind the camera) get hidden.
pub fn position_customer_popups(
    customers_q: Query<(&Customer, &GlobalTransform)>,
    cam_q: Query<(&Camera, &GlobalTransform), With<PlayerCamera>>,
    window_q: Query<&Window, With<PrimaryWindow>>,
    mut popups_q: Query<(&CustomerPopup, &mut Node, &mut Visibility)>,
) {
    let Ok((cam, cam_transform)) = cam_q.single() else {
        return;
    };
    let Ok(window) = window_q.single() else {
        return;
    };
    let Some(viewport_size) = cam.logical_viewport_size() else {
        return;
    };
    if viewport_size.x <= 0.0 || viewport_size.y <= 0.0 {
        return;
    }
    let scale = Vec2::new(window.width(), window.height()) / viewport_size;

    for (popup, mut node, mut visibility) in popups_q.iter_mut() {
        let Ok((customer, customer_transform)) = customers_q.get(popup.customer) else {
            *visibility = Visibility::Hidden;
            continue;
        };
        if customer.state != CustomerState::Waiting {
            *visibility = Visibility::Hidden;
            continue;
        }
        let head = customer_transform.translation() + Vec3::Y * POPUP_OFFSET_Y;
        let Ok(viewport_pos) = cam.world_to_viewport(cam_transform, head) else {
            *visibility = Visibility::Hidden;
            continue;
        };
        let screen = viewport_pos * scale;
        // Center horizontally on the head; sit just above it.
        node.left = Val::Px(screen.x - 40.0);
        node.top = Val::Px(screen.y - 30.0);
        *visibility = Visibility::Visible;
    }
}
