use bevy::prelude::*;
use engine_api::{DynamicPlugin, Player, ScriptOwned, add};

#[derive(Default)]
pub struct ExamplePlugin;

/// Plugin-defined component. Lives entirely inside this dylib — its TypeId
/// changes on every rebuild, so any entity holding it must be despawned in
/// teardown. The host's auto-despawn (driven by `ScriptOwned`) handles that.
#[derive(Component, Default, Debug)]
struct ScriptCounter {
    ticks: u32,
}

impl DynamicPlugin for ExamplePlugin {
    fn setup(&self, world: &mut World, schedule: &mut Schedule) {
        let entity = world
            .spawn((ScriptCounter::default(), ScriptOwned::new(PLUGIN_NAME)))
            .id();
        info!("[{PLUGIN_NAME}] spawned counter entity {entity:?}");

        schedule.add_systems((typeid_sanity_check, hello_tick, tick_counter));
    }
}

fn typeid_sanity_check(mut ran: Local<bool>) {
    if *ran {
        return;
    }
    *ran = true;
    info!(
        "[{PLUGIN_NAME}] Time TypeId          = {:?}",
        std::any::TypeId::of::<bevy::time::Time>()
    );
    info!(
        "[{PLUGIN_NAME}] Player TypeId        = {:?}",
        std::any::TypeId::of::<Player>()
    );
    info!(
        "[{PLUGIN_NAME}] ScriptCounter TypeId = {:?} (changes every rebuild)",
        std::any::TypeId::of::<ScriptCounter>()
    );
}

fn hello_tick(
    time: Res<Time>,
    player: Query<&Transform, With<Player>>,
    mut last_bucket: Local<u32>,
) {
    let Ok(t) = player.single() else {
        return;
    };
    let bucket = time.elapsed_secs() as u32;
    if bucket == *last_bucket {
        return;
    }
    *last_bucket = bucket;
    let p = t.translation;
    info!(
        "[{PLUGIN_NAME}] elapsed={:.3} pos=({:.1}, {:.1}, {:.1})",
        time.elapsed_secs(),
        p.x,
        p.y,
        p.z
    );
}

/// Reads/writes a plugin-defined component. Proves that components declared
/// inside the dylib work for queries within a single load session.
fn tick_counter(time: Res<Time>, mut q: Query<&mut ScriptCounter>, mut last_bucket: Local<u32>) {
    for mut c in &mut q {
        c.ticks += 1;
    }
    let bucket = time.elapsed_secs() as u32;
    if bucket == *last_bucket {
        return;
    }
    *last_bucket = bucket;
    if let Ok(c) = q.single() {
        info!("[{PLUGIN_NAME}] counter ticks={}", c.ticks);
    }
}

add!(ExamplePlugin, "example");
