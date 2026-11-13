//! The plugin lifecycle steps the host runs, independent of how the plugin
//! arrived. The `add!` FFI shims call these for a dylib; the host calls them
//! directly for a statically linked plugin — one definition, so the two paths
//! can't drift.

use bevy::prelude::*;

use crate::{DynamicPlugin, ScriptOwned, ScriptRegistry};

/// Run `register` with the [`ScriptRegistry`] lifted out of the world, so the
/// plugin can take both without aliasing.
///
/// # Panics
/// If the world has no `ScriptRegistry` — callers check first (for a dylib,
/// its absence is the TypeId-mismatch symptom).
pub fn register(plugin: &dyn DynamicPlugin, world: &mut World) {
    world.resource_scope(|world, mut registry: Mut<ScriptRegistry>| {
        plugin.register(world, &mut registry);
    });
}

/// Run the plugin's own `teardown`, then despawn every entity it tagged
/// [`ScriptOwned`]. Compares by string content: a dylib's static strings live
/// at different addresses from the host's.
pub fn teardown(plugin: &dyn DynamicPlugin, plugin_name: &str, world: &mut World) {
    plugin.teardown(world);

    let owned: Vec<Entity> = world
        .query::<(Entity, &ScriptOwned)>()
        .iter(world)
        .filter(|(_, owned)| owned.plugin == plugin_name)
        .map(|(entity, _)| entity)
        .collect();
    for entity in owned {
        world.despawn(entity);
    }
}
