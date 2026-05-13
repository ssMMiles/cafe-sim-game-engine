//! Drink pickup → carry → counter delivery → leave-out pipeline.
//!
//! Bridges the brewing system (`coffee_menu`) and the customer AI (`customer`):
//! when the player clicks a machine whose `Brewing` is `ready()`, the drink
//! becomes a yellow cube attached to the player camera. Clicking a countertop
//! whose waiting customer ordered that same drink deposits the cube on the
//! counter, awards the drink's price, and starts a 5s `ServedTimer`. When the
//! timer elapses the counter cube despawns and the customer walks out (+Z).

use bevy::prelude::*;
use engine_api::PlayerCamera;

use crate::balance::{Balance, spawn_balance_popup};
use crate::coffee_menu::Brewing;
use crate::customer::{
    AngryFace, CUSTOMER_FLOAT_Y, Customer, CustomerState, HappyFace, ServedTimer,
};
use crate::drinks::price_for;
use crate::grid::{FLOOR_Y, HALF_D, cell_center};
use crate::menus::ActiveMenu;
use crate::pause_menu::GamePaused;
use crate::placement::{CoffeeMachine, Countertop, GarbageCan, PlacedCell, PlacementSelection};
use crate::spawn::SpawnOwnedCommands;

const HAND_OFFSET: Vec3 = Vec3::new(0.25, -0.2, -0.5);
const HAND_SCALE: f32 = 0.1;
const COUNTER_DRINK_Y_OFFSET: f32 = 1.0;
const COUNTER_DRINK_SCALE: f32 = 0.15;
const MACHINE_DRINK_Y_OFFSET: f32 = 1.0;
const SERVED_HOLD_SECS: f32 = 5.0;
const SERVED_HOLD_SECS_WRONG: f32 = 1.0;

#[derive(Resource)]
pub struct DeliveryAssets {
    pub mesh: Handle<Mesh>,
    pub material: Handle<StandardMaterial>,
}

#[derive(Resource, Default)]
pub struct CarriedDrink {
    pub drink: Option<&'static str>,
    pub visual: Option<Entity>,
}

/// A drink cube sitting on a counter waiting for its customer's timer to elapse.
#[derive(Component)]
pub struct DeliveredDrink {
    pub customer: Entity,
}

/// A drink cube sitting on top of a coffee machine that's finished brewing.
/// Spawned when `Brewing.ready()` flips true, despawned when the machine loses
/// its `Brewing` component (pickup) or the machine itself disappears.
#[derive(Component)]
pub struct ReadyDrink {
    pub machine: Entity,
}

pub fn init_delivery_assets(world: &mut World) {
    let mesh = world
        .resource_mut::<Assets<Mesh>>()
        .add(Cuboid::from_length(1.0));
    let material = world
        .resource_mut::<Assets<StandardMaterial>>()
        .add(StandardMaterial {
            base_color: Color::srgb(1.0, 0.85, 0.2),
            unlit: true,
            ..default()
        });
    world.insert_resource(DeliveryAssets { mesh, material });
}

/// Pickup: raycast the camera ray to a `CoffeeMachine` ancestor whose `Brewing`
/// is ready, swap the brewing component out for a carried-drink resource entry.
pub fn handle_pickup_click(
    mut commands: Commands,
    paused: Res<GamePaused>,
    active: Res<ActiveMenu>,
    selection: Res<PlacementSelection>,
    mut mouse: ResMut<ButtonInput<MouseButton>>,
    mut carried: ResMut<CarriedDrink>,
    cam_q: Query<&GlobalTransform, With<PlayerCamera>>,
    machine_q: Query<&Brewing, With<CoffeeMachine>>,
    parent_q: Query<&ChildOf>,
    mut ray_cast: MeshRayCast,
) {
    if paused.0 || active.is_open() || selection.piece.is_some() || carried.drink.is_some() {
        return;
    }
    if !mouse.just_pressed(MouseButton::Left) {
        return;
    }
    let Ok(cam) = cam_q.single() else {
        return;
    };
    let ray = Ray3d::new(cam.translation(), cam.forward());
    let hits = ray_cast.cast_ray(ray, &MeshRayCastSettings::default());
    let Some(&(hit_entity, _)) = hits.first() else {
        return;
    };

    let mut entity = hit_entity;
    let (machine, brewing) = loop {
        if let Ok(brewing) = machine_q.get(entity) {
            break (entity, brewing);
        }
        match parent_q.get(entity) {
            Ok(child_of) => entity = child_of.parent(),
            Err(_) => return,
        }
    };
    if !brewing.ready() {
        return;
    }

    carried.drink = Some(brewing.drink);
    commands.entity(machine).remove::<Brewing>();
    mouse.clear_just_pressed(MouseButton::Left);
}

/// Delivery: with a carried drink, raycast onto a `Countertop` and try to find
/// a waiting customer at that cell whose order matches. On match, deposit the
/// cube on the counter, credit the player, and arm the served timer.
pub fn handle_delivery_click(
    mut commands: Commands,
    paused: Res<GamePaused>,
    active: Res<ActiveMenu>,
    selection: Res<PlacementSelection>,
    mut mouse: ResMut<ButtonInput<MouseButton>>,
    mut carried: ResMut<CarriedDrink>,
    mut balance: ResMut<Balance>,
    assets: Res<DeliveryAssets>,
    cam_q: Query<&GlobalTransform, With<PlayerCamera>>,
    counter_q: Query<&PlacedCell, With<Countertop>>,
    parent_q: Query<&ChildOf>,
    mut customers_q: Query<(Entity, &mut Customer)>,
    mut ray_cast: MeshRayCast,
) {
    if paused.0 || active.is_open() || selection.piece.is_some() {
        return;
    }
    let Some(drink) = carried.drink else {
        return;
    };
    if !mouse.just_pressed(MouseButton::Left) {
        return;
    }
    let Ok(cam) = cam_q.single() else {
        return;
    };
    let ray = Ray3d::new(cam.translation(), cam.forward());
    let hits = ray_cast.cast_ray(ray, &MeshRayCastSettings::default());
    let Some(&(hit_entity, _)) = hits.first() else {
        return;
    };

    let mut entity = hit_entity;
    let cell = loop {
        if let Ok(&PlacedCell(i, j)) = counter_q.get(entity) {
            break (i, j);
        }
        match parent_q.get(entity) {
            Ok(child_of) => entity = child_of.parent(),
            Err(_) => return,
        }
    };

    let Some((customer_entity, mut customer)) = customers_q
        .iter_mut()
        .find(|(_, c)| c.counter_cell == cell && c.state == CustomerState::Waiting)
    else {
        // No waiting customer at this counter — swallow the click so it doesn't
        // also trigger placement or re-open the brew menu.
        mouse.clear_just_pressed(MouseButton::Left);
        return;
    };

    let correct = customer.order == drink;
    let hold_secs = if correct {
        SERVED_HOLD_SECS
    } else {
        SERVED_HOLD_SECS_WRONG
    };

    if correct && let Some(price) = price_for(drink) {
        balance.0 += price;
        spawn_balance_popup(&mut commands, price);
    }

    let center = cell_center(cell.0, cell.1);
    commands.spawn_owned((
        DeliveredDrink {
            customer: customer_entity,
        },
        Mesh3d(assets.mesh.clone()),
        MeshMaterial3d(assets.material.clone()),
        Transform::from_xyz(center.x, FLOOR_Y + COUNTER_DRINK_Y_OFFSET, center.z)
            .with_scale(Vec3::splat(COUNTER_DRINK_SCALE)),
    ));

    customer.state = CustomerState::Served;
    let mut customer_cmds = commands.entity(customer_entity);
    customer_cmds.insert(ServedTimer {
        remaining: hold_secs,
    });
    if correct {
        customer_cmds.insert(HappyFace);
    } else {
        customer_cmds.insert(AngryFace);
    }

    if let Some(visual) = carried.visual.take() {
        commands.entity(visual).despawn();
    }
    carried.drink = None;

    mouse.clear_just_pressed(MouseButton::Left);
}

/// Discard: with a carried drink, raycast onto a `GarbageCan` to throw the
/// drink away — clears `CarriedDrink` and despawns the hand cube. No money,
/// no customer interaction.
pub fn handle_discard_click(
    mut commands: Commands,
    paused: Res<GamePaused>,
    active: Res<ActiveMenu>,
    selection: Res<PlacementSelection>,
    mut mouse: ResMut<ButtonInput<MouseButton>>,
    mut carried: ResMut<CarriedDrink>,
    cam_q: Query<&GlobalTransform, With<PlayerCamera>>,
    bin_q: Query<(), With<GarbageCan>>,
    parent_q: Query<&ChildOf>,
    mut ray_cast: MeshRayCast,
) {
    if paused.0 || active.is_open() || selection.piece.is_some() || carried.drink.is_none() {
        return;
    }
    if !mouse.just_pressed(MouseButton::Left) {
        return;
    }
    let Ok(cam) = cam_q.single() else {
        return;
    };
    let ray = Ray3d::new(cam.translation(), cam.forward());
    let hits = ray_cast.cast_ray(ray, &MeshRayCastSettings::default());
    let Some(&(hit_entity, _)) = hits.first() else {
        return;
    };

    let mut entity = hit_entity;
    loop {
        if bin_q.contains(entity) {
            break;
        }
        match parent_q.get(entity) {
            Ok(child_of) => entity = child_of.parent(),
            Err(_) => return,
        }
    }

    if let Some(visual) = carried.visual.take() {
        commands.entity(visual).despawn();
    }
    carried.drink = None;
    mouse.clear_just_pressed(MouseButton::Left);
}

/// Mirror `CarriedDrink.drink` to a child of the player camera so the drink
/// appears "in hand". Change-detects on the resource — only fires when the
/// resource actually flips, not every frame.
pub fn manage_carried_visual(
    mut commands: Commands,
    mut carried: ResMut<CarriedDrink>,
    assets: Res<DeliveryAssets>,
    cam_q: Query<Entity, With<PlayerCamera>>,
) {
    if !carried.is_changed() {
        return;
    }

    match (carried.drink, carried.visual) {
        (Some(_), None) => {
            let Ok(camera) = cam_q.single() else {
                return;
            };
            let cube = commands
                .spawn_owned((
                    Mesh3d(assets.mesh.clone()),
                    MeshMaterial3d(assets.material.clone()),
                    Transform::from_translation(HAND_OFFSET).with_scale(Vec3::splat(HAND_SCALE)),
                ))
                .id();
            commands.entity(camera).add_child(cube);
            carried.visual = Some(cube);
        }
        (None, Some(visual)) => {
            commands.entity(visual).despawn();
            carried.visual = None;
        }
        _ => {}
    }
}

/// Tick the 5s post-delivery hold. When it elapses, despawn the counter cube,
/// reroute the customer toward the +Z exit, and flip their state to Leaving so
/// `walk_customers` carries them out.
pub fn tick_served_timer(
    mut commands: Commands,
    time: Res<Time>,
    paused: Res<GamePaused>,
    mut customers_q: Query<(Entity, &mut Customer, &mut ServedTimer)>,
    drinks_q: Query<(Entity, &DeliveredDrink)>,
) {
    if paused.0 {
        return;
    }
    let dt = time.delta_secs();
    for (entity, mut customer, mut timer) in customers_q.iter_mut() {
        timer.remaining -= dt;
        if timer.remaining > 0.0 {
            continue;
        }

        for (drink_entity, drink) in drinks_q.iter() {
            if drink.customer == entity {
                commands.entity(drink_entity).despawn();
            }
        }

        customer.target = Vec3::new(customer.target.x, CUSTOMER_FLOAT_Y, HALF_D as f32 + 1.0);
        customer.state = CustomerState::Leaving;
        commands.entity(entity).remove::<ServedTimer>();
    }
}

/// Spawn a small yellow cube on top of any machine whose `Brewing` is `ready()`
/// and doesn't already have one, and despawn cubes whose machine has dropped
/// its `Brewing` (i.e. pickup happened) or vanished entirely. Mirrors the
/// `manage_brew_popups` pattern in `coffee_menu`.
pub fn manage_ready_drinks(
    mut commands: Commands,
    assets: Res<DeliveryAssets>,
    machines_q: Query<(Entity, &Brewing, &GlobalTransform), With<CoffeeMachine>>,
    ready_q: Query<(Entity, &ReadyDrink)>,
) {
    use std::collections::HashSet;

    let ready_machines: HashSet<Entity> = machines_q
        .iter()
        .filter(|(_, b, _)| b.ready())
        .map(|(e, _, _)| e)
        .collect();
    let already_has_cube: HashSet<Entity> = ready_q.iter().map(|(_, r)| r.machine).collect();

    for (cube, ready_drink) in ready_q.iter() {
        if !ready_machines.contains(&ready_drink.machine) {
            commands.entity(cube).despawn();
        }
    }

    for (machine, _brewing, gt) in machines_q.iter() {
        if !ready_machines.contains(&machine) || already_has_cube.contains(&machine) {
            continue;
        }
        let pos = gt.translation();
        commands.spawn_owned((
            ReadyDrink { machine },
            Mesh3d(assets.mesh.clone()),
            MeshMaterial3d(assets.material.clone()),
            Transform::from_xyz(pos.x, FLOOR_Y + MACHINE_DRINK_Y_OFFSET, pos.z)
                .with_scale(Vec3::splat(COUNTER_DRINK_SCALE)),
        ));
    }
}

/// Despawn delivered-drink cubes whose customer entity is gone (covers the
/// cafe-close path where `clear_customers_when_closed` despawns customers
/// mid-served). Mirrors the orphan-popup sweep in `manage_customer_popups`.
pub fn cleanup_orphan_drinks(
    mut commands: Commands,
    drinks_q: Query<(Entity, &DeliveredDrink)>,
    customers_q: Query<(), With<Customer>>,
) {
    for (drink_entity, drink) in drinks_q.iter() {
        if customers_q.get(drink.customer).is_err() {
            commands.entity(drink_entity).despawn();
        }
    }
}
