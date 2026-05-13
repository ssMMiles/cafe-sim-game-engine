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
use rand::seq::SliceRandom;
use std::collections::HashSet;

use crate::cafe::CafeOpen;
use crate::drinks::DRINKS;
use crate::grid::{CAFE_DEPTH, CAFE_WIDTH, FLOOR_Y, HALF_D, cell_center};
use crate::pause_menu::GamePaused;
use crate::placement::{CoffeeMachine, Countertop, GridOccupancy, PlacedCell};
use crate::spawn::{SpawnOwnedCommands, WithChildOwned};

const MAX_CUSTOMERS: usize = 3;
const SPAWN_COOLDOWN_SECS: f32 = 3.0;
const WALK_SPEED: f32 = 1.5;
const ARRIVE_DISTANCE: f32 = 0.1;
pub const CUSTOMER_FLOAT_Y: f32 = FLOOR_Y + 0.6;
const CUSTOMER_SIZE: f32 = 0.4;
const POPUP_OFFSET_Y: f32 = 0.9;
const POPUP_FONT_SIZE: f32 = 16.0;

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
    /// Drink delivered — popup hidden, holding for `ServedTimer` before leaving.
    Served,
    /// Walking back toward the +Z exit; despawned on arrival.
    Leaving,
}

/// Countdown attached when a customer transitions from Waiting → Served.
/// When it hits zero, the customer's target is rerouted to the cafe exit and
/// their state flips to Leaving (handled in `delivery::tick_served_timer`).
#[derive(Component)]
pub struct ServedTimer {
    pub remaining: f32,
}

/// Marker attached to a customer who was served the wrong drink. The popup
/// flips to ">:(" during their Served state and they walk out without paying.
#[derive(Component)]
pub struct AngryFace;

/// Marker attached to a customer who was served their correct drink. The popup
/// flips to ":)" during their Served state.
#[derive(Component)]
pub struct HappyFace;

#[derive(Component)]
pub struct Customer {
    pub state: CustomerState,
    /// Cell of the counter this customer is being served at. Reserved so two
    /// customers don't queue at the same counter.
    pub counter_cell: (i32, i32),
    /// Cell the customer stands in while waiting (one of the four orthogonal
    /// neighbors of the counter). Reserved so two customers don't try to stand
    /// in the same spot — e.g. between two adjacent counters.
    pub queue_cell: (i32, i32),
    pub target: Vec3,
    pub order: &'static str,
}

fn cell_in_bounds(cell: (i32, i32)) -> bool {
    (0..CAFE_WIDTH).contains(&cell.0) && (0..CAFE_DEPTH).contains(&cell.1)
}

/// Pick a free cell adjacent to `counter` for the customer to stand in.
/// Preference order: front (+Z, the entrance side), then sides, then back.
/// Returns `None` if all four neighbors are out-of-bounds, occupied by another
/// placed piece, or already reserved by another customer.
fn pick_queue_cell(
    counter: (i32, i32),
    occupancy: &GridOccupancy,
    reserved: &HashSet<(i32, i32)>,
) -> Option<(i32, i32)> {
    let candidates = [
        (counter.0, counter.1 + 1), // +Z (front)
        (counter.0 - 1, counter.1), // -X
        (counter.0 + 1, counter.1), // +X
        (counter.0, counter.1 - 1), // -Z (back)
    ];
    candidates
        .into_iter()
        .find(|c| cell_in_bounds(*c) && !occupancy.0.contains_key(c) && !reserved.contains(c))
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

/// Despawn all customers the moment the cafe transitions from open to closed
/// (whether from the Close Early button or the auto-close at closing time).
/// `manage_customer_popups` cleans up the orphaned popup nodes the next frame.
pub fn clear_customers_when_closed(
    mut commands: Commands,
    cafe_open: Res<CafeOpen>,
    customers_q: Query<Entity, With<Customer>>,
    mut was_open: Local<bool>,
    mut timer: ResMut<CustomerSpawnTimer>,
) {
    if *was_open && !cafe_open.0 {
        for entity in &customers_q {
            commands.entity(entity).despawn();
        }
        timer.remaining = 0.0;
    }
    *was_open = cafe_open.0;
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
    occupancy: Res<GridOccupancy>,
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

    // Customers in Leaving state are walking out — their counter and queue cell
    // are immediately reusable so a fresh customer can spawn into them.
    let active = |c: &&Customer| !matches!(c.state, CustomerState::Leaving);
    let reserved_counters: HashSet<(i32, i32)> = customers_q
        .iter()
        .filter(active)
        .map(|c| c.counter_cell)
        .collect();
    let reserved_queues: HashSet<(i32, i32)> = customers_q
        .iter()
        .filter(active)
        .map(|c| c.queue_cell)
        .collect();

    // Pair each unreserved counter with a usable queue cell on one of its sides.
    // Counters whose every neighbor is walled-in or already taken are skipped.
    let candidates: Vec<((i32, i32), (i32, i32))> = counters_q
        .iter()
        .copied()
        .filter(|c| !reserved_counters.contains(&(c.0, c.1)))
        .filter_map(|c| {
            let counter = (c.0, c.1);
            pick_queue_cell(counter, &occupancy, &reserved_queues).map(|q| (counter, q))
        })
        .collect();
    if candidates.is_empty() {
        return;
    }

    let mut rng = rand::thread_rng();
    let (counter_cell, queue_cell) = *candidates.choose(&mut rng).expect("candidates non-empty");
    let queue_world = cell_center(queue_cell.0, queue_cell.1);
    let target = Vec3::new(queue_world.x, CUSTOMER_FLOAT_Y, queue_world.z);
    let order = DRINKS.choose(&mut rng).map(|d| d.name).unwrap_or("Coffee");

    // Spawn just outside the front edge, X-aligned with the queue cell so the
    // straight-line walk is least likely to clip other furniture.
    let spawn_pos = Vec3::new(target.x, CUSTOMER_FLOAT_Y, HALF_D as f32 + 1.0);

    commands.spawn_owned((
        Customer {
            state: CustomerState::Walking,
            counter_cell,
            queue_cell,
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
    mut commands: Commands,
    time: Res<Time>,
    paused: Res<GamePaused>,
    mut q: Query<(Entity, &mut Customer, &mut Transform)>,
) {
    if paused.0 {
        return;
    }
    let dt = time.delta_secs();
    for (entity, mut customer, mut transform) in &mut q {
        if !matches!(
            customer.state,
            CustomerState::Walking | CustomerState::Leaving
        ) {
            continue;
        }
        let to_target = customer.target - transform.translation;
        let flat = Vec3::new(to_target.x, 0.0, to_target.z);
        let dist = flat.length();
        if dist <= ARRIVE_DISTANCE {
            match customer.state {
                CustomerState::Walking => customer.state = CustomerState::Waiting,
                CustomerState::Leaving => {
                    commands.entity(entity).despawn();
                }
                _ => {}
            }
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
    angry_q: Query<(), With<AngryFace>>,
    happy_q: Query<(), With<HappyFace>>,
    cam_q: Query<(&Camera, &GlobalTransform), With<PlayerCamera>>,
    window_q: Query<&Window, With<PrimaryWindow>>,
    mut popups_q: Query<(Entity, &CustomerPopup, &mut Node, &mut Visibility)>,
    children_q: Query<&Children>,
    mut text_q: Query<&mut Text>,
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

    for (popup_entity, popup, mut node, mut visibility) in popups_q.iter_mut() {
        let Ok((customer, customer_transform)) = customers_q.get(popup.customer) else {
            *visibility = Visibility::Hidden;
            continue;
        };
        let angry = angry_q.contains(popup.customer);
        let happy = happy_q.contains(popup.customer);
        let (show, label): (bool, &str) = match customer.state {
            CustomerState::Waiting => (true, customer.order),
            CustomerState::Served if angry => (true, ">:("),
            CustomerState::Served if happy => (true, ":)"),
            _ => (false, customer.order),
        };
        if !show {
            *visibility = Visibility::Hidden;
            continue;
        }

        // Sync the popup's single Text child to the current label.
        if let Ok(children) = children_q.get(popup_entity) {
            for child in children.iter() {
                if let Ok(mut text) = text_q.get_mut(child)
                    && text.as_str() != label
                {
                    **text = label.to_string();
                }
            }
        }

        let head = customer_transform.translation() + Vec3::Y * POPUP_OFFSET_Y;
        let Ok(viewport_pos) = cam.world_to_viewport(cam_transform, head) else {
            *visibility = Visibility::Hidden;
            continue;
        };
        let screen = viewport_pos * scale;
        node.left = Val::Px(screen.x - 40.0);
        node.top = Val::Px(screen.y - 30.0);
        *visibility = Visibility::Visible;
    }
}
