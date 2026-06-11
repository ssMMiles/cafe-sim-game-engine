//! Staff agents — the in-world behavior layer for hired staff.
//!
//! Each hired [`crate::staff::StaffMember`] spawns as a green floating cube at
//! the cafe door when the day opens (the `CafeOpen` false→true edge). Staff with
//! no job wander the floor; staff assigned **Barista** run the same make-and-serve
//! loop the player does by hand: walk to an idle coffee machine, brew the waiting
//! customer's order (sped up / slowed by their Drink-Time stat), carry it to the
//! counter, and serve — crediting the drink's price plus a friendliness-scaled tip.
//!
//! At end of day (`CafeOpen` true→false) staff walk out the door and draw their
//! salary from [`Balance`], paid in roster order; anyone the player can't afford
//! quits (dropped from the roster) and is named in a next-morning toast.
//!
//! Movement, float height, and exit mirror [`crate::customer`]; the serve effect
//! mirrors [`crate::delivery::handle_delivery_click`].

use bevy::prelude::*;
use rand::Rng;
use std::collections::HashSet;

use crate::balance::{Balance, spawn_balance_loss_popup, spawn_balance_popup};
use crate::cafe::CafeOpen;
use crate::coffee_menu::Brewing;
use crate::customer::{Customer, CustomerState, HappyFace, ServedTimer};
use crate::delivery::{DeliveredDrink, DeliveryAssets};
use crate::drinks::{brew_secs_for, price_for};
use crate::grid::{CAFE_DEPTH, CAFE_WIDTH, FLOOR_Y, HALF_D, cell_center};
use crate::pause_menu::GamePaused;
use crate::placement::CoffeeMachine;
use crate::spawn::{SpawnOwnedCommands, WithChildOwned};
use crate::staff::{Job, Staff};

const WALK_SPEED: f32 = 1.5;
const ARRIVE_DISTANCE: f32 = 0.1;
const STAFF_FLOAT_Y: f32 = FLOOR_Y + 0.6;
const STAFF_SIZE: f32 = 0.4;
const DOOR_Z: f32 = HALF_D as f32 + 1.0;
const SPREAD_X: f32 = 0.6;
/// How long a wandering staffer heads toward one spot before re-rolling a target.
const WANDER_REPATH_SECS: f32 = 2.5;
/// Tip = price * friendliness * this. Friendliness 1.0 → +50% of the price.
const TIP_FRACTION: f32 = 0.5;
const SERVED_HOLD_SECS: f32 = 5.0;
const CARRY_SCALE: f32 = 0.15;
const CARRY_OFFSET: Vec3 = Vec3::new(0.0, 0.5, 0.0);
const NOTICE_LIFETIME_SECS: f32 = 6.0;

// ----------------------------------------------------------------------------
// Resources / components
// ----------------------------------------------------------------------------

#[derive(Resource)]
pub struct StaffAssets {
    pub mesh: Handle<Mesh>,
    pub material: Handle<StandardMaterial>,
}

/// Names of staff who quit at the last close for lack of pay. Drained into a
/// toast the next morning. In-memory only — the roster shrink is what persists.
#[derive(Resource, Default)]
pub struct PayrollNotice {
    pub names: Vec<String>,
}

/// Lifetime marker for the morning "couldn't make payroll" toast.
#[derive(Component)]
pub struct NoticeToast {
    pub elapsed: f32,
}

#[derive(Clone, Copy)]
enum AgentState {
    Wandering,
    ToMachine {
        customer: Entity,
        counter: (i32, i32),
        machine: Entity,
        drink: &'static str,
    },
    WaitBrew {
        customer: Entity,
        counter: (i32, i32),
        machine: Entity,
        drink: &'static str,
    },
    ToCustomer {
        customer: Entity,
        counter: (i32, i32),
        drink: &'static str,
    },
    Leaving,
}

#[derive(Component)]
pub struct StaffAgent {
    /// Index into [`Staff::members`] — read for job + stats while working.
    roster_index: usize,
    /// Base walk speed, `WALK_SPEED * (1 + move_speed_mod)`, cached at spawn.
    speed: f32,
    state: AgentState,
    target: Vec3,
    wander_cooldown: f32,
    /// Yellow cube child shown while carrying a finished drink.
    carried_visual: Option<Entity>,
}

// ----------------------------------------------------------------------------
// Assets
// ----------------------------------------------------------------------------

/// Cache the green cube mesh + material once at plugin setup, mirroring
/// `init_customer_assets`.
pub fn init_staff_assets(world: &mut World) {
    let mesh = world
        .resource_mut::<Assets<Mesh>>()
        .add(Cuboid::from_length(STAFF_SIZE));
    let material = world
        .resource_mut::<Assets<StandardMaterial>>()
        .add(StandardMaterial {
            base_color: Color::srgb(0.2, 0.8, 0.3),
            unlit: true,
            ..default()
        });
    world.insert_resource(StaffAssets { mesh, material });
}

// ----------------------------------------------------------------------------
// Day lifecycle: spawn at open, payroll + walk-out at close
// ----------------------------------------------------------------------------

pub fn staff_day_lifecycle(
    mut commands: Commands,
    cafe_open: Res<CafeOpen>,
    mut staff: ResMut<Staff>,
    mut balance: ResMut<Balance>,
    mut notice: ResMut<PayrollNotice>,
    assets: Res<StaffAssets>,
    mut agents_q: Query<(Entity, &Transform, &mut StaffAgent)>,
    mut was_open: Local<bool>,
) {
    let open = cafe_open.0;

    if open && !*was_open {
        // Open edge: clear any stragglers still walking out, spawn a fresh cube
        // per roster member, and surface last night's payroll message.
        for (e, _, _) in agents_q.iter() {
            commands.entity(e).despawn();
        }
        let total = staff.members.len();
        for (i, member) in staff.members.iter().enumerate() {
            spawn_agent(&mut commands, &assets, i, member.move_speed_mod, total);
        }
        if !notice.names.is_empty() {
            spawn_notice_toast(&mut commands, &notice.names);
            notice.names.clear();
        }
    } else if !open && *was_open {
        // Close edge: run payroll (pay who we can, in order) then send everyone home.
        let mut kept = Vec::with_capacity(staff.members.len());
        let mut total_paid = 0i64;
        for member in std::mem::take(&mut staff.members) {
            if balance.0 >= member.pay {
                balance.0 -= member.pay;
                total_paid += member.pay;
                kept.push(member);
            } else {
                notice.names.push(member.name.clone());
            }
        }
        staff.members = kept;
        if total_paid > 0 {
            spawn_balance_loss_popup(&mut commands, total_paid);
        }
        for (_, transform, mut agent) in agents_q.iter_mut() {
            agent.state = AgentState::Leaving;
            agent.target = Vec3::new(transform.translation.x, STAFF_FLOAT_Y, DOOR_Z);
        }
    } else if open {
        // Steady state: cover mid-day hires that have no agent yet.
        let have: HashSet<usize> = agents_q.iter().map(|(_, _, a)| a.roster_index).collect();
        let total = staff.members.len();
        for (i, member) in staff.members.iter().enumerate() {
            if !have.contains(&i) {
                spawn_agent(&mut commands, &assets, i, member.move_speed_mod, total);
            }
        }
    }

    *was_open = open;
}

fn spawn_agent(
    commands: &mut Commands,
    assets: &StaffAssets,
    index: usize,
    move_speed_mod: f32,
    total: usize,
) {
    let spread = (index as f32 - (total as f32 - 1.0) * 0.5) * SPREAD_X;
    let pos = Vec3::new(spread, STAFF_FLOAT_Y, DOOR_Z);
    let speed = WALK_SPEED * (1.0 + move_speed_mod).max(0.2);
    commands.spawn_owned((
        StaffAgent {
            roster_index: index,
            speed,
            state: AgentState::Wandering,
            target: random_interior_target(),
            wander_cooldown: 0.0,
            carried_visual: None,
        },
        Mesh3d(assets.mesh.clone()),
        MeshMaterial3d(assets.material.clone()),
        Transform::from_translation(pos),
    ));
}

// ----------------------------------------------------------------------------
// Per-frame movement + barista AI
// ----------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
pub fn tick_staff(
    mut commands: Commands,
    time: Res<Time>,
    paused: Res<GamePaused>,
    cafe_open: Res<CafeOpen>,
    staff: Res<Staff>,
    delivery_assets: Res<DeliveryAssets>,
    mut balance: ResMut<Balance>,
    mut agents_q: Query<(Entity, &mut Transform, &mut StaffAgent)>,
    mut customers_q: Query<(Entity, &mut Customer)>,
    machines_q: Query<(Entity, &GlobalTransform, Has<Brewing>), With<CoffeeMachine>>,
    brewing_q: Query<&Brewing>,
) {
    if paused.0 {
        return;
    }
    let dt = time.delta_secs();

    // --- Frame-start snapshots (read-only) ---

    // Customers a barista is already committed to, and machines already in use
    // by a barista (heading to or brewing at). Prevents double-booking.
    let mut claimed_customers: HashSet<Entity> = HashSet::new();
    let mut claimed_machines: HashSet<Entity> = HashSet::new();
    for (_, _, agent) in agents_q.iter() {
        match agent.state {
            AgentState::ToMachine {
                customer, machine, ..
            }
            | AgentState::WaitBrew {
                customer, machine, ..
            } => {
                claimed_customers.insert(customer);
                claimed_machines.insert(machine);
            }
            AgentState::ToCustomer { customer, .. } => {
                claimed_customers.insert(customer);
            }
            _ => {}
        }
    }

    // Customer state lookup, and the pool of unclaimed waiting customers.
    let mut waiting: Vec<(Entity, (i32, i32), &'static str)> = Vec::new();
    for (entity, customer) in customers_q.iter() {
        if customer.state == CustomerState::Waiting && !claimed_customers.contains(&entity) {
            waiting.push((entity, customer.counter_cell, customer.order));
        }
    }

    // Idle machines (no Brewing, unclaimed) and a set for arrival re-checks.
    let mut idle_machines: Vec<(Entity, Vec3)> = Vec::new();
    let idle_machine_set: HashSet<Entity> = machines_q
        .iter()
        .filter(|(_, _, has_brew)| !*has_brew)
        .map(|(e, _, _)| e)
        .collect();
    for (entity, gt, has_brew) in machines_q.iter() {
        if !has_brew && !claimed_machines.contains(&entity) {
            idle_machines.push((entity, gt.translation()));
        }
    }

    // --- Per-agent update ---
    for (agent_entity, mut transform, mut agent) in agents_q.iter_mut() {
        let speed = agent.speed;
        match agent.state {
            AgentState::Wandering => {
                let job = staff.members.get(agent.roster_index).and_then(|m| m.job);
                let can_work = cafe_open.0 && job == Some(Job::Barista);
                if can_work
                    && !waiting.is_empty()
                    && !idle_machines.is_empty()
                    && brew_secs_for(waiting.last().unwrap().2).is_some()
                {
                    let (customer, counter, drink) = waiting.pop().unwrap();
                    let (machine, mpos) = idle_machines.pop().unwrap();
                    agent.state = AgentState::ToMachine {
                        customer,
                        counter,
                        machine,
                        drink,
                    };
                    agent.target = Vec3::new(mpos.x, STAFF_FLOAT_Y, mpos.z);
                    continue;
                }

                agent.wander_cooldown -= dt;
                let arrived = step_toward(&mut transform, agent.target, speed, dt);
                if arrived || agent.wander_cooldown <= 0.0 {
                    agent.target = random_interior_target();
                    agent.wander_cooldown = WANDER_REPATH_SECS;
                }
            }

            AgentState::ToMachine {
                customer,
                counter,
                machine,
                drink,
            } => {
                if !customer_waiting(&customers_q, customer) {
                    agent.state = AgentState::Wandering;
                    continue;
                }
                if step_toward(&mut transform, agent.target, speed, dt) {
                    if !idle_machine_set.contains(&machine) {
                        // Machine got taken before we arrived — try again later.
                        agent.state = AgentState::Wandering;
                        continue;
                    }
                    let dmod = staff
                        .members
                        .get(agent.roster_index)
                        .map(|m| m.drink_time_mod)
                        .unwrap_or(0.0);
                    let base = brew_secs_for(drink).unwrap_or(4.0);
                    commands.entity(machine).insert(Brewing {
                        drink,
                        total_secs: (base * (1.0 + dmod)).max(0.5),
                        elapsed: 0.0,
                    });
                    agent.state = AgentState::WaitBrew {
                        customer,
                        counter,
                        machine,
                        drink,
                    };
                }
            }

            AgentState::WaitBrew {
                customer,
                counter,
                machine,
                drink,
            } => {
                if !customer_waiting(&customers_q, customer) {
                    // Abandon — leave the brew for the player to pick up.
                    agent.state = AgentState::Wandering;
                    continue;
                }
                let ready = brewing_q.get(machine).map(|b| b.ready());
                match ready {
                    Ok(true) => {
                        commands.entity(machine).remove::<Brewing>();
                        let cube = commands
                            .spawn_owned((
                                Mesh3d(delivery_assets.mesh.clone()),
                                MeshMaterial3d(delivery_assets.material.clone()),
                                Transform::from_translation(CARRY_OFFSET)
                                    .with_scale(Vec3::splat(CARRY_SCALE)),
                            ))
                            .id();
                        commands.entity(agent_entity).add_child(cube);
                        agent.carried_visual = Some(cube);
                        agent.target = carry_target(counter);
                        agent.state = AgentState::ToCustomer {
                            customer,
                            counter,
                            drink,
                        };
                    }
                    Ok(false) => {} // still brewing — hold position
                    Err(_) => {
                        // Brewing vanished (e.g. player picked it up) — give up.
                        agent.state = AgentState::Wandering;
                    }
                }
            }

            AgentState::ToCustomer {
                customer,
                counter,
                drink,
            } => {
                if step_toward(&mut transform, agent.target, speed, dt) {
                    serve(
                        &mut commands,
                        &delivery_assets,
                        &mut balance,
                        &staff,
                        agent.roster_index,
                        &mut customers_q,
                        customer,
                        counter,
                        drink,
                    );
                    if let Some(cube) = agent.carried_visual.take() {
                        commands.entity(cube).despawn();
                    }
                    agent.state = AgentState::Wandering;
                }
            }

            AgentState::Leaving => {
                if step_toward(&mut transform, agent.target, speed, dt) {
                    commands.entity(agent_entity).despawn();
                }
            }
        }
    }
}

/// Replicate the correct-serve half of `handle_delivery_click`: deposit a drink
/// cube on the counter, credit price + a friendliness tip, and arm the customer's
/// served timer. No-op if the customer is no longer waiting at `counter`.
#[allow(clippy::too_many_arguments)]
fn serve(
    commands: &mut Commands,
    assets: &DeliveryAssets,
    balance: &mut Balance,
    staff: &Staff,
    roster_index: usize,
    customers_q: &mut Query<(Entity, &mut Customer)>,
    customer: Entity,
    counter: (i32, i32),
    drink: &'static str,
) {
    let Ok((_, mut cust)) = customers_q.get_mut(customer) else {
        return;
    };
    if cust.state != CustomerState::Waiting || cust.counter_cell != counter {
        return;
    }

    if let Some(price) = price_for(drink) {
        let friendliness = staff
            .members
            .get(roster_index)
            .map(|m| m.friendliness)
            .unwrap_or(0.0);
        let tip = (price as f32 * friendliness * TIP_FRACTION).round() as i64;
        balance.0 += price + tip;
        spawn_balance_popup(commands, price + tip);
    }

    let center = cell_center(counter.0, counter.1);
    commands.spawn_owned((
        DeliveredDrink { customer },
        Mesh3d(assets.mesh.clone()),
        MeshMaterial3d(assets.material.clone()),
        Transform::from_xyz(center.x, FLOOR_Y + 1.0, center.z).with_scale(Vec3::splat(0.15)),
    ));

    cust.state = CustomerState::Served;
    commands
        .entity(customer)
        .insert((ServedTimer { remaining: SERVED_HOLD_SECS }, HappyFace));
}

// ----------------------------------------------------------------------------
// Morning toast
// ----------------------------------------------------------------------------

fn spawn_notice_toast(commands: &mut Commands, names: &[String]) {
    let message = format!(
        "Couldn't make payroll — {} staff quit: {}",
        names.len(),
        names.join(", ")
    );
    commands
        .spawn_owned((
            Node {
                position_type: PositionType::Absolute,
                top: Val::Px(90.0),
                left: Val::Px(0.0),
                right: Val::Px(0.0),
                justify_content: JustifyContent::Center,
                ..default()
            },
            GlobalZIndex(120),
        ))
        .with_children(|root| {
            root.spawn_owned((
                NoticeToast { elapsed: 0.0 },
                Node {
                    padding: UiRect::axes(Val::Px(18.0), Val::Px(10.0)),
                    border_radius: BorderRadius::all(Val::Px(6.0)),
                    ..default()
                },
                BackgroundColor(Color::srgba(0.20, 0.05, 0.07, 0.92)),
            ))
            .with_child_owned((
                Text::new(message),
                TextFont {
                    font_size: 18.0,
                    ..default()
                },
                TextColor(Color::srgb(1.0, 0.55, 0.55)),
            ));
        });
}

pub fn tick_notice_toast(
    mut commands: Commands,
    time: Res<Time>,
    paused: Res<GamePaused>,
    mut toasts_q: Query<(Entity, &mut NoticeToast, &Children)>,
    mut text_color_q: Query<&mut TextColor>,
) {
    if paused.0 {
        return;
    }
    let dt = time.delta_secs();
    for (entity, mut toast, children) in toasts_q.iter_mut() {
        toast.elapsed += dt;
        let t = (toast.elapsed / NOTICE_LIFETIME_SECS).clamp(0.0, 1.0);
        if t >= 1.0 {
            commands.entity(entity).despawn();
            continue;
        }
        // Fade the text in the last quarter of the lifetime.
        let alpha = ((1.0 - t) * 4.0).min(1.0);
        for child in children.iter() {
            if let Ok(mut color) = text_color_q.get_mut(child) {
                let c = color.0.to_srgba();
                *color = TextColor(Color::srgba(c.red, c.green, c.blue, alpha));
            }
        }
    }
}

// ----------------------------------------------------------------------------
// Helpers
// ----------------------------------------------------------------------------

fn step_toward(transform: &mut Transform, target: Vec3, speed: f32, dt: f32) -> bool {
    let to_target = target - transform.translation;
    let flat = Vec3::new(to_target.x, 0.0, to_target.z);
    let dist = flat.length();
    if dist <= ARRIVE_DISTANCE {
        return true;
    }
    let step = (flat / dist) * speed * dt;
    transform.translation.x += step.x;
    transform.translation.z += step.z;
    false
}

fn random_interior_target() -> Vec3 {
    let mut rng = rand::thread_rng();
    let i = rng.gen_range(0..CAFE_WIDTH);
    let j = rng.gen_range(0..CAFE_DEPTH);
    let c = cell_center(i, j);
    Vec3::new(c.x, STAFF_FLOAT_Y, c.z)
}

fn carry_target(counter: (i32, i32)) -> Vec3 {
    let c = cell_center(counter.0, counter.1);
    Vec3::new(c.x, STAFF_FLOAT_Y, c.z)
}

fn customer_waiting(customers_q: &Query<(Entity, &mut Customer)>, customer: Entity) -> bool {
    customers_q
        .get(customer)
        .map(|(_, c)| c.state == CustomerState::Waiting)
        .unwrap_or(false)
}
