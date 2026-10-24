use bevy::prelude::*;
use engine_api::{
    DynamicPlugin, LevelDataHooks, Player, PropDef, PropDefault, ScriptBinding, ScriptOwned,
    ScriptRegistry, ScriptSchema, add,
};

#[derive(Default)]
pub struct ExamplePlugin;

/// Plugin-defined component. Lives entirely inside this dylib — its TypeId
/// changes on every rebuild, so any entity holding it must be despawned in
/// teardown. The host's auto-despawn (driven by `ScriptOwned`) handles that.
#[derive(Component, Default, Debug)]
struct ScriptCounter {
    ticks: u32,
}

/// Attached to placed level entities by the `spinner` script below.
///
/// Note this is *not* `ScriptOwned`: the entity belongs to the level, not to
/// the plugin, so it must survive unload. The host instead records that this
/// component was added and removes exactly it on unload/hot-reload — which is
/// what makes a dylib-private component safe to put on a level entity.
#[derive(Component, Debug)]
struct Spinner {
    radians_per_sec: f32,
}

/// Props the editor seeds when a `spinner` binding is attached.
static SPINNER_PROPS: &[PropDef] = &[PropDef::new("speed", PropDefault::Float(1.0))];

/// Level-scoped state owned by this plugin, persisted inside `world.meta`.
#[derive(Resource, Default, Debug)]
struct ExampleLevelData {
    load_count: u32,
}

impl DynamicPlugin for ExamplePlugin {
    /// Registration only — runs in editor mode too, so the scripts panel can
    /// list `spinner` and attach it while the plugin's systems are stopped.
    fn register(&self, _world: &mut World, registry: &mut ScriptRegistry) {
        registry.register_script(
            PLUGIN_NAME,
            ScriptSchema::new("spinner").with_props(SPINNER_PROPS),
            hydrate_spinner,
        );

        registry.register_level_hooks(
            PLUGIN_NAME,
            LevelDataHooks {
                save: save_level_data,
                load: load_level_data,
            },
        );
    }

    fn setup(&self, world: &mut World, schedule: &mut Schedule) {
        let entity = world
            .spawn((ScriptCounter::default(), ScriptOwned::new(PLUGIN_NAME)))
            .id();
        info!("[{PLUGIN_NAME}] spawned counter entity {entity:?}");

        world.init_resource::<ExampleLevelData>();

        schedule.add_systems((typeid_sanity_check, hello_tick, tick_counter, spin));
    }
}

/// Turn a `spinner` binding into a live component.
///
/// Factories should *insert* onto the entity rather than spawn: the host
/// tracks inserts via an archetype diff, but anything spawned here would need
/// its own `ScriptOwned` tag to get cleaned up.
fn hydrate_spinner(entity: &mut bevy::ecs::world::EntityWorldMut, binding: &ScriptBinding) {
    entity.insert(Spinner {
        radians_per_sec: binding.float_or("speed", 1.0),
    });
}

fn spin(time: Res<Time>, mut q: Query<(&Spinner, &mut Transform)>) {
    for (spinner, mut transform) in &mut q {
        transform.rotate_y(spinner.radians_per_sec * time.delta_secs());
    }
}

/// Serialize this plugin's level state. Returning `None` would mean "store
/// nothing"; here we always persist the counter.
fn save_level_data(world: &mut World) -> Option<Vec<u8>> {
    let data = world.get_resource::<ExampleLevelData>()?;
    info!(
        "[{PLUGIN_NAME}] saving level data (load_count={})",
        data.load_count
    );
    Some(data.load_count.to_le_bytes().to_vec())
}

/// Restore it. Called once per world load and again after a hot-reload.
fn load_level_data(world: &mut World, bytes: &[u8]) {
    let Ok(raw) = <[u8; 4]>::try_from(bytes) else {
        warn!(
            "[{PLUGIN_NAME}] level data is {} byte(s), expected 4 — ignoring",
            bytes.len()
        );
        return;
    };
    let load_count = u32::from_le_bytes(raw).saturating_add(1);
    world.insert_resource(ExampleLevelData { load_count });
    info!("[{PLUGIN_NAME}] restored level data (load_count now {load_count})");
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
