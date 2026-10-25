//! Host-side loader for dynamic plugin dylibs.
//!
//! Scans `<workspace>/plugins/` (resolved via `CARGO_MANIFEST_DIR` baked at
//! compile time) for subdirectories containing a `Cargo.toml`. Each subdir's
//! name is the plugin id; the matching DLL is expected at `<exe-dir>/<id>.dll`
//! (where cargo writes dylib artifacts).
//!
//! Each plugin gets:
//! - its own [`PluginSchedule`] (label = plugin id), populated by the dylib's
//!   `plugin_setup` and replaced wholesale on hot-reload,
//! - independent mtime tracking, so a rebuild of one plugin only reloads that
//!   one,
//! - independent ABI-checked load with rollback if ABI doesn't match.
//!
//! Hot-reload: press **Shift+R** to force-reload everything, or just rebuild a
//! plugin and the loader picks up the mtime change automatically.

#[cfg(not(target_family = "wasm"))]
use std::{
    collections::HashMap,
    ffi::c_void,
    panic::AssertUnwindSafe,
    path::{Path, PathBuf},
    time::SystemTime,
};

use bevy::ecs::schedule::ScheduleLabel;
use bevy::prelude::*;
use engine_api::ScriptRegistry;

#[cfg(not(target_family = "wasm"))]
use crate::projects::ActiveProject;
#[cfg(not(target_family = "wasm"))]
use bevy::ecs::component::ComponentId;
#[cfg(not(target_family = "wasm"))]
use engine_api::{AppState, ScriptBindings, ScriptHydrateFn};
#[cfg(not(target_family = "wasm"))]
use std::collections::HashSet;

// ---------------------------------------------------------------------------
// Schedule
// ---------------------------------------------------------------------------

/// One schedule per loaded plugin, keyed by plugin id (the source directory
/// name under `plugins/`). Replaced wholesale on hot-reload.
#[derive(ScheduleLabel, Debug, Clone, PartialEq, Eq, Hash)]
pub struct PluginSchedule(pub String);

// ---------------------------------------------------------------------------
// Hydration bookkeeping
// ---------------------------------------------------------------------------

/// Marks a placed entity whose [`ScriptBindings`] are not yet fully realised
/// into plugin components.
///
/// Set when the entity spawns, and re-set whenever a plugin is unloaded or
/// hot-reloaded. It is *not* cleared when a binding names a plugin the project
/// does not load — that entity simply stays marked, so adding the plugin later
/// hydrates it on the next frame without a reload.
///
/// Defined unconditionally (not behind `cfg(not(wasm))`) so the spawn path in
/// `world::objects` is platform-independent; on wasm nothing ever consumes it.
#[derive(Component, Default)]
pub struct NeedsHydration;

/// Ids of the plugins whose dylibs are actually loaded right now.
///
/// Distinct from `ActiveProject.plugins`, which is only what the project
/// *declares*. A plugin can be declared but absent here because its dylib was
/// never built (`cargo run` does not build plugin crates), failed its ABI
/// check, or panicked in `setup`. The editor uses the difference between the
/// two lists to explain an empty script list instead of just showing nothing.
#[derive(Resource, Default)]
pub struct LoadedPluginIds(pub Vec<String>);

/// Set by the editor's Plugins ▸ Reload all; equivalent to Shift+R. Cleared
/// when the reload runs.
#[derive(Resource, Default)]
pub struct ReloadRequested(pub bool);

/// Records which components a plugin's hydrate fn actually inserted, so the
/// host can remove exactly those again when the plugin goes away.
#[cfg(not(target_family = "wasm"))]
#[derive(Component, Default)]
struct HydratedScripts(Vec<HydratedScript>);

#[cfg(not(target_family = "wasm"))]
struct HydratedScript {
    plugin: String,
    script: String,
    /// Components added by the factory call, captured as an archetype diff.
    component_ids: Vec<ComponentId>,
}

/// Which plugins have had their level-data `load` hook run for the currently
/// loaded world, so a hook runs once per (world, plugin) rather than per frame.
#[cfg(not(target_family = "wasm"))]
#[derive(Resource, Default)]
struct AppliedPluginData {
    world_id: String,
    plugins: HashSet<String>,
}

/// `(plugin, script)` pairs already warned about, so an unresolvable binding
/// logs once instead of every frame. Entries for a plugin are dropped when it
/// unloads, so a reload that still fails warns again.
#[cfg(not(target_family = "wasm"))]
#[derive(Resource, Default)]
struct ReportedMissingScripts(HashSet<(String, String)>);

// ---------------------------------------------------------------------------
// Plugin
// ---------------------------------------------------------------------------

pub struct ScriptHostPlugin;

impl Plugin for ScriptHostPlugin {
    fn build(&self, app: &mut App) {
        // The registry exists on every platform so engine-side code (and the
        // editor) can query it unconditionally; on wasm it just stays empty.
        app.init_resource::<ScriptRegistry>();
        app.init_resource::<ReloadRequested>();
        app.init_resource::<LoadedPluginIds>();

        #[cfg(not(target_family = "wasm"))]
        {
            app.insert_resource(LoadedPlugins::default());
            app.init_resource::<AppliedPluginData>();
            app.init_resource::<ReportedMissingScripts>();
            app.add_systems(OnEnter(AppState::InGame), load_active_project_plugins);
            app.add_systems(OnExit(AppState::InGame), unload_all_plugins);
            // Plugins run only in gameplay mode. Both are no-ops outside
            // `AppState::InGame`, where nothing is loaded to begin with.
            app.add_systems(
                OnEnter(crate::editor::EditorMode::Gameplay),
                activate_all_plugins,
            );
            app.add_systems(
                OnExit(crate::editor::EditorMode::Gameplay),
                deactivate_all_plugins,
            );
            app.add_systems(Update, run_scripts.run_if(in_state(AppState::InGame)));
            app.add_systems(
                Update,
                apply_plugin_level_data.run_if(in_state(AppState::InGame)),
            );
            // Must observe the save request before the terrain save snapshots
            // `TerrainWorld.metadata`, since that snapshot is what gets written.
            app.add_systems(
                Update,
                collect_plugin_level_data
                    .before(crate::game::world::terrain::save::save_terrain)
                    .run_if(in_state(AppState::InGame)),
            );
        }
        #[cfg(target_family = "wasm")]
        {
            bevy::log::info!("[scripts] dynamic plugin loading disabled on wasm");
        }
    }
}

// ---------------------------------------------------------------------------
// Native implementation
// ---------------------------------------------------------------------------

#[cfg(not(target_family = "wasm"))]
type PluginSetupFn = unsafe extern "C" fn(*mut c_void, *mut c_void) -> u32;

#[cfg(not(target_family = "wasm"))]
type PluginRegisterFn = unsafe extern "C" fn(*mut c_void) -> u32;

#[cfg(not(target_family = "wasm"))]
type PluginTeardownFn = unsafe extern "C" fn(*mut c_void) -> u32;

#[cfg(not(target_family = "wasm"))]
type PluginAbiVersionFn = unsafe extern "C" fn() -> u32;

#[cfg(not(target_family = "wasm"))]
type PluginNameFn = unsafe extern "C" fn() -> *const std::os::raw::c_char;

/// Per-plugin state held for the lifetime of the process (or until hot-reload
/// replaces it).
#[cfg(not(target_family = "wasm"))]
struct LoadedPlugin {
    library: libloading::Library,
    /// Plugin's self-reported name (used for logging only — not the same as
    /// the plugin id, which is the source directory name).
    plugin_name: String,
    /// Path to the original DLL (the one cargo writes to).
    source_path: PathBuf,
    /// mtime of the source DLL when we last loaded it.
    loaded_mtime: Option<SystemTime>,
    /// Counter for generating unique copy filenames per plugin.
    reload_count: u32,
    /// Whether the plugin's `setup` has run and its schedule is installed.
    ///
    /// Loading and activation are independent: a plugin is loaded whenever
    /// the project declares it, but only *active* in `EditorMode::Gameplay`.
    /// While inactive its scripts stay in the registry (so the editor can
    /// still list and attach them) but none of its systems run.
    active: bool,
}

#[cfg(not(target_family = "wasm"))]
#[derive(Resource, Default)]
struct LoadedPlugins {
    plugins: HashMap<String, LoadedPlugin>,
    /// mtime of last *failed* load attempt per plugin id. Used to skip
    /// retrying a busted DLL until its mtime changes.
    failed_mtimes: HashMap<String, Option<SystemTime>>,
    /// Next unused copy index per plugin id. Persists across unload/reload
    /// cycles within a process — unloaded libraries are `mem::forget`'d,
    /// so their copy files stay locked, and reusing index 0 would collide.
    next_copy_index: HashMap<String, u32>,
}

/// Returns the path to a plugin's dylib next to the running exe, or `None` if
/// the exe path can't be resolved.
///
/// Mirrors cargo's artifact-naming rules: dashes in the crate name become
/// underscores, and the platform-appropriate dylib prefix/suffix are applied.
/// So plugin id `cafe-sim` resolves to `cafe_sim.dll` on Windows,
/// `libcafe_sim.so` on Linux, and `libcafe_sim.dylib` on macOS.
#[cfg(not(target_family = "wasm"))]
fn plugin_dll_path(id: &str) -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let artifact = id.replace('-', "_");
    let filename = format!(
        "{}{}{}",
        std::env::consts::DLL_PREFIX,
        artifact,
        std::env::consts::DLL_SUFFIX,
    );
    Some(exe.parent()?.join(filename))
}

#[cfg(not(target_family = "wasm"))]
fn file_mtime(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).ok()?.modified().ok()
}

/// Copy the DLL to a numbered variant so the original stays unlocked for
/// `cargo build`. Returns the path to the copy.
#[cfg(not(target_family = "wasm"))]
fn copy_dll(source: &Path, n: u32) -> std::io::Result<PathBuf> {
    let stem = source.file_stem().unwrap_or_default().to_string_lossy();
    let ext = source.extension().unwrap_or_default().to_string_lossy();
    let dest = source
        .parent()
        .unwrap_or(source)
        .join(format!("{stem}.loaded.{n}.{ext}"));
    std::fs::copy(source, &dest)?;
    Ok(dest)
}

// ---------------------------------------------------------------------------
// Core load / register / reload
// ---------------------------------------------------------------------------

/// A dylib that's been loaded, ABI-checked, and had all its symbols resolved.
/// No world state has been touched yet — call [`run_register`] to commit.
#[cfg(not(target_family = "wasm"))]
struct PreparedLibrary {
    library: libloading::Library,
    plugin_name: String,
    register_fn: PluginRegisterFn,
}

/// Phase 1 of loading: copy the DLL, map it, verify the ABI, and resolve
/// every symbol the host will need. Doesn't touch world state, so failure
/// here is safe to roll back from — the caller can keep the previous
/// version running.
#[cfg(not(target_family = "wasm"))]
fn prepare_library(
    plugin_id: &str,
    source_path: &Path,
    reload_count: u32,
) -> Option<PreparedLibrary> {
    if let Some(dir) = source_path.parent() {
        add_dll_search_dir(dir);
    }

    let copy_path = match copy_dll(source_path, reload_count) {
        Ok(p) => p,
        Err(e) => {
            bevy::log::error!(
                "[scripts] '{plugin_id}' failed to copy {}: {e}",
                source_path.display()
            );
            return None;
        }
    };

    bevy::log::info!(
        "[scripts] '{plugin_id}' loading {} (copy: {})",
        source_path.display(),
        copy_path.display()
    );

    let library = match unsafe { libloading::Library::new(&copy_path) } {
        Ok(lib) => lib,
        Err(e) => {
            bevy::log::error!(
                "[scripts] '{plugin_id}' failed to load {}: {e}",
                copy_path.display()
            );
            return None;
        }
    };

    let dylib_abi = unsafe {
        match library.get::<PluginAbiVersionFn>(b"plugin_abi_version") {
            Ok(sym) => sym(),
            Err(e) => {
                bevy::log::error!("[scripts] '{plugin_id}' missing plugin_abi_version symbol: {e}");
                return None;
            }
        }
    };
    if dylib_abi != engine_api::ENGINE_SCRIPTS_ABI_VERSION {
        bevy::log::error!(
            "[scripts] '{plugin_id}' ABI mismatch: host expects {}, dylib exports {}",
            engine_api::ENGINE_SCRIPTS_ABI_VERSION,
            dylib_abi
        );
        return None;
    }

    let plugin_name = unsafe {
        match library.get::<PluginNameFn>(b"plugin_name") {
            Ok(sym) => {
                let ptr = sym();
                if ptr.is_null() {
                    bevy::log::error!("[scripts] '{plugin_id}' plugin_name returned null");
                    return None;
                }
                std::ffi::CStr::from_ptr(ptr).to_string_lossy().into_owned()
            }
            Err(e) => {
                bevy::log::error!("[scripts] '{plugin_id}' missing plugin_name symbol: {e}");
                return None;
            }
        }
    };

    let register_fn: PluginRegisterFn = unsafe {
        match library.get::<PluginRegisterFn>(b"plugin_register") {
            Ok(sym) => *sym,
            Err(e) => {
                bevy::log::error!("[scripts] '{plugin_id}' missing plugin_register symbol: {e}");
                return None;
            }
        }
    };

    // Resolved up front so a dylib missing either symbol is rejected in this
    // phase, while rolling back is still free.
    if let Err(e) = unsafe { library.get::<PluginSetupFn>(b"plugin_setup") } {
        bevy::log::error!("[scripts] '{plugin_id}' missing plugin_setup symbol: {e}");
        return None;
    }

    if let Err(e) = unsafe { library.get::<PluginTeardownFn>(b"plugin_teardown") } {
        bevy::log::error!("[scripts] '{plugin_id}' missing plugin_teardown symbol: {e}");
        return None;
    }

    Some(PreparedLibrary {
        library,
        plugin_name,
        register_fn,
    })
}

/// Phase 2 of loading: run the plugin's `register`.
///
/// Registration is mode-independent — it only populates the script registry —
/// so this runs on every load and hot-reload, in both editor and gameplay
/// mode. Returns `false` if the dylib reported an error or panicked, in which
/// case the caller must roll the load back.
#[cfg(not(target_family = "wasm"))]
fn run_register(world: &mut World, plugin_id: &str, register_fn: PluginRegisterFn) -> bool {
    // Make sure Time is registered host-side before the dylib touches it.
    let _ = world.register_resource::<bevy::time::Time>();

    let world_ptr = world as *mut World as *mut c_void;
    let result = std::panic::catch_unwind(AssertUnwindSafe(|| unsafe { register_fn(world_ptr) }));

    match result {
        Ok(0) => true,
        Ok(code) => {
            bevy::log::error!("[scripts] '{plugin_id}' plugin_register returned error code {code}");
            false
        }
        Err(_) => {
            bevy::log::error!("[scripts] '{plugin_id}' plugin_register panicked");
            false
        }
    }
}

/// Start a loaded plugin's gameplay systems: run `setup` and install its
/// schedule. No-op if the plugin isn't loaded or is already active.
///
/// The entry is lifted out of `LoadedPlugins` for the duration so the dylib
/// gets an unaliased `&mut World`, and put back either way.
#[cfg(not(target_family = "wasm"))]
fn activate_plugin(world: &mut World, plugin_id: &str) {
    let Some(mut entry) = world
        .resource_mut::<LoadedPlugins>()
        .plugins
        .remove(plugin_id)
    else {
        return;
    };
    if entry.active {
        world
            .resource_mut::<LoadedPlugins>()
            .plugins
            .insert(plugin_id.to_owned(), entry);
        return;
    }

    let setup_fn: PluginSetupFn = unsafe {
        match entry.library.get::<PluginSetupFn>(b"plugin_setup") {
            Ok(sym) => *sym,
            Err(e) => {
                bevy::log::error!("[scripts] '{plugin_id}' missing plugin_setup symbol: {e}");
                world
                    .resource_mut::<LoadedPlugins>()
                    .plugins
                    .insert(plugin_id.to_owned(), entry);
                return;
            }
        }
    };

    let _ = world.register_resource::<bevy::time::Time>();

    let mut new_schedule = Schedule::new(PluginSchedule(plugin_id.to_owned()));
    let schedule_ptr = &mut new_schedule as *mut bevy::ecs::schedule::Schedule as *mut c_void;
    let world_ptr = world as *mut World as *mut c_void;

    let result = std::panic::catch_unwind(AssertUnwindSafe(|| unsafe {
        setup_fn(world_ptr, schedule_ptr)
    }));

    let failed = match result {
        Ok(0) => false,
        Ok(2) => {
            bevy::log::error!(
                "[scripts] '{plugin_id}' TypeId mismatch — the DLL was compiled \
                 against different bevy metadata. Rebuild the full workspace \
                 (cargo build --workspace) and restart."
            );
            true
        }
        Ok(code) => {
            bevy::log::error!("[scripts] '{plugin_id}' plugin_setup returned error code {code}");
            true
        }
        Err(_) => {
            bevy::log::error!("[scripts] '{plugin_id}' plugin_setup panicked");
            true
        }
    };

    if failed {
        // A partial setup may have spawned ScriptOwned entities; sweep them
        // so the next activation starts clean. Registry entries stay — they
        // came from `register`, which succeeded.
        despawn_owned_entities(world, plugin_id);
        world
            .resource_mut::<LoadedPlugins>()
            .plugins
            .insert(plugin_id.to_owned(), entry);
        return;
    }

    // Replaces any prior schedule with the same label.
    world
        .resource_mut::<bevy::ecs::schedule::Schedules>()
        .insert(new_schedule);

    // Level data is applied once per (world, plugin). Setup has just
    // re-inserted this plugin's resources at their defaults, so the saved
    // blob has to be re-applied on top — clearing the record is what makes
    // `apply_plugin_level_data` do that on the next frame. Must happen
    // *after* setup, or the defaults would land on top of the save.
    world
        .resource_mut::<AppliedPluginData>()
        .plugins
        .remove(plugin_id);

    entry.active = true;
    world
        .resource_mut::<LoadedPlugins>()
        .plugins
        .insert(plugin_id.to_owned(), entry);

    bevy::log::info!("[scripts] '{plugin_id}' activated");
}

/// Stop a plugin's gameplay systems: run `teardown` (which sweeps its
/// `ScriptOwned` entities) and drop its schedule. The dylib stays mapped and
/// its registry entries stay put, so the editor keeps working.
///
/// No-op if the plugin isn't loaded or is already inactive.
#[cfg(not(target_family = "wasm"))]
fn deactivate_plugin(world: &mut World, plugin_id: &str) {
    let Some(mut entry) = world
        .resource_mut::<LoadedPlugins>()
        .plugins
        .remove(plugin_id)
    else {
        return;
    };
    if !entry.active {
        world
            .resource_mut::<LoadedPlugins>()
            .plugins
            .insert(plugin_id.to_owned(), entry);
        return;
    }

    run_teardown(world, &entry.library, plugin_id);

    // Dropped while the dylib is still mapped, so the systems' drop glue is
    // safe to run.
    if let Some(mut schedules) = world.get_resource_mut::<bevy::ecs::schedule::Schedules>() {
        schedules.remove(PluginSchedule(plugin_id.to_owned()));
    }

    // Re-applied on the next activation (setup resets resources to defaults).
    world
        .resource_mut::<AppliedPluginData>()
        .plugins
        .remove(plugin_id);

    // A plugin that was holding input when it stopped (an open menu, say)
    // would otherwise leave the editor unable to move the camera. Its holder
    // string points into dylib pages, so release it by id.
    if let Some(mut lock) = world.get_resource_mut::<engine_api::InputLock>() {
        lock.unlock_id(plugin_id);
    }

    entry.active = false;
    world
        .resource_mut::<LoadedPlugins>()
        .plugins
        .insert(plugin_id.to_owned(), entry);

    bevy::log::info!("[scripts] '{plugin_id}' deactivated");
}

/// Despawn every entity tagged `ScriptOwned` for `plugin_id`.
///
/// Normally the dylib's own teardown shim does this; the host needs it for
/// the one case where that shim can't be trusted to have run — a `setup` that
/// failed or panicked partway through.
#[cfg(not(target_family = "wasm"))]
fn despawn_owned_entities(world: &mut World, plugin_id: &str) {
    let orphans: Vec<Entity> = world
        .query::<(Entity, &engine_api::ScriptOwned)>()
        .iter(world)
        .filter(|(_, owned)| owned.plugin == plugin_id)
        .map(|(entity, _)| entity)
        .collect();
    if orphans.is_empty() {
        return;
    }
    bevy::log::warn!(
        "[scripts] '{plugin_id}' left {} entity(ies) behind in its failed setup; despawning",
        orphans.len()
    );
    for entity in orphans {
        world.despawn(entity);
    }
}

/// Clean up after a `plugin_register` that failed or panicked partway through.
///
/// A failed registration may already have mutated the world: registered
/// script factories and level hooks (fn pointers into the dylib) and, if the
/// plugin ignored the contract, spawned entities. Dropping the library here
/// would `dlclose` those pages and turn any later registry call or despawn
/// into a jump into unmapped memory. So, like every other unload path, the
/// library is `mem::forget`'d (pages stay mapped forever), the plugin's
/// registry entries are removed, and its orphaned entities are despawned —
/// safe precisely because the pages stay mapped.
#[cfg(not(target_family = "wasm"))]
fn abort_failed_load(world: &mut World, plugin_id: &str, library: libloading::Library) {
    std::mem::forget(library);

    world
        .resource_mut::<ScriptRegistry>()
        .remove_plugin(plugin_id);

    despawn_owned_entities(world, plugin_id);
}

/// Call `plugin_teardown` on a still-loaded library. Used during hot-reload
/// to give the *old* dylib a chance to despawn its `ScriptOwned` entities
/// before its code pages are leaked via `mem::forget`.
#[cfg(not(target_family = "wasm"))]
fn run_teardown(world: &mut World, library: &libloading::Library, plugin_id: &str) {
    let teardown_fn: PluginTeardownFn = unsafe {
        match library.get::<PluginTeardownFn>(b"plugin_teardown") {
            Ok(sym) => *sym,
            Err(e) => {
                bevy::log::error!(
                    "[scripts] '{plugin_id}' missing plugin_teardown symbol on unload: {e}"
                );
                return;
            }
        }
    };

    let world_ptr = world as *mut World as *mut c_void;
    let result = std::panic::catch_unwind(AssertUnwindSafe(|| unsafe { teardown_fn(world_ptr) }));

    match result {
        Ok(0) => bevy::log::info!("[scripts] '{plugin_id}' torn down"),
        Ok(code) => {
            bevy::log::error!("[scripts] '{plugin_id}' plugin_teardown returned error code {code}")
        }
        Err(_) => bevy::log::error!("[scripts] '{plugin_id}' plugin_teardown panicked"),
    }
}

// ---------------------------------------------------------------------------
// Systems
// ---------------------------------------------------------------------------

/// Exclusive `OnEnter(AppState::InGame)` system: load every plugin declared
/// by the active project.
#[cfg(not(target_family = "wasm"))]
fn load_active_project_plugins(world: &mut World) {
    bevy::log::info!(
        "[HOST] Time TypeId   = {:?}",
        std::any::TypeId::of::<bevy::time::Time>()
    );
    bevy::log::info!(
        "[HOST] Player TypeId = {:?}",
        std::any::TypeId::of::<engine_api::Player>()
    );

    let Some(active) = world.get_resource::<ActiveProject>() else {
        bevy::log::warn!(
            "[scripts] entered InGame without an ActiveProject — no plugins will load"
        );
        return;
    };
    let ids = active.0.plugins.clone();
    if ids.is_empty() {
        bevy::log::info!("[scripts] project '{}' declares no plugins", active.0.id);
        return;
    }
    bevy::log::info!(
        "[scripts] loading {} plugin(s) for project '{}': {:?}",
        ids.len(),
        active.0.id,
        ids
    );

    for id in ids {
        try_load_plugin(world, &id);
    }
}

/// Exclusive `OnExit(AppState::InGame)` system: tear down and drop every
/// currently loaded plugin. Mirrors the teardown pattern used during
/// hot-reload (run teardown → drop schedule → mem::forget library).
#[cfg(not(target_family = "wasm"))]
fn unload_all_plugins(world: &mut World) {
    let ids: Vec<String> = world
        .resource::<LoadedPlugins>()
        .plugins
        .keys()
        .cloned()
        .collect();
    if ids.is_empty() {
        return;
    }
    bevy::log::info!(
        "[scripts] unloading {} plugin(s) on exit to menu",
        ids.len()
    );

    for id in ids {
        unload_one(world, &id);
    }

    // Wipe the failed_mtimes too so a fresh load attempt can proceed next time.
    world.resource_mut::<LoadedPlugins>().failed_mtimes.clear();
}

/// Tear down a single loaded plugin: run teardown, drop its schedule, and
/// `mem::forget` the library so any lingering vtable pointers stay mapped.
/// No-op if the id isn't currently loaded.
#[cfg(not(target_family = "wasm"))]
fn unload_one(world: &mut World, id: &str) {
    if !world.resource::<LoadedPlugins>().plugins.contains_key(id) {
        return;
    }

    // Strip hydrated components first: their drop glue lives in this dylib,
    // which is still mapped here but whose pages we leak below. This also
    // clears the plugin's registry entries, which a plain deactivation
    // deliberately leaves alone.
    dehydrate_plugin(world, id);

    // Runs teardown and drops the schedule, but only if it was active.
    deactivate_plugin(world, id);

    let Some(loaded) = world.resource_mut::<LoadedPlugins>().plugins.remove(id) else {
        return;
    };
    std::mem::forget(loaded.library);
    refresh_loaded_plugin_ids(world);
    bevy::log::info!("[scripts] '{id}' unloaded");
}

/// Public hook for live-loading a plugin while the game is running. Use
/// from `Commands::queue` so it runs as a deferred exclusive command.
/// Idempotent — if the plugin is already loaded, this is a no-op.
#[cfg(not(target_family = "wasm"))]
pub fn load_plugin(world: &mut World, id: &str) {
    if world.resource::<LoadedPlugins>().plugins.contains_key(id) {
        return;
    }
    try_load_plugin(world, id);
}

#[cfg(target_family = "wasm")]
pub fn load_plugin(_world: &mut World, _id: &str) {}

/// Public hook for live-unloading a plugin. Runs the teardown and drops the
/// library — same shape as the on-exit cleanup, but for one id.
#[cfg(not(target_family = "wasm"))]
pub fn unload_plugin(world: &mut World, id: &str) {
    unload_one(world, id);
}

#[cfg(target_family = "wasm")]
pub fn unload_plugin(_world: &mut World, _id: &str) {}

/// Attempt to load a plugin by id. Records mtime on success; records the
/// failed mtime on failure so we don't retry until the DLL is rebuilt.
#[cfg(not(target_family = "wasm"))]
fn try_load_plugin(world: &mut World, plugin_id: &str) {
    let Some(source_path) = plugin_dll_path(plugin_id) else {
        bevy::log::warn!("[scripts] '{plugin_id}' could not resolve exe path");
        return;
    };
    if !source_path.exists() {
        bevy::log::warn!(
            "[scripts] '{plugin_id}' DLL not found at {} (build it with `cargo build -p {plugin_id}`)",
            source_path.display()
        );
        return;
    }

    let mtime = file_mtime(&source_path);

    let copy_index = {
        let mut state = world.resource_mut::<LoadedPlugins>();
        let slot = state
            .next_copy_index
            .entry(plugin_id.to_owned())
            .or_insert(0);
        let n = *slot;
        *slot = n.wrapping_add(1);
        n
    };

    let Some(prepared) = prepare_library(plugin_id, &source_path, copy_index) else {
        let mut state = world.resource_mut::<LoadedPlugins>();
        state.failed_mtimes.insert(plugin_id.to_owned(), mtime);
        return;
    };
    let PreparedLibrary {
        library,
        plugin_name,
        register_fn,
    } = prepared;

    if !run_register(world, plugin_id, register_fn) {
        abort_failed_load(world, plugin_id, library);
        let mut state = world.resource_mut::<LoadedPlugins>();
        state.failed_mtimes.insert(plugin_id.to_owned(), mtime);
        return;
    }

    let mut state = world.resource_mut::<LoadedPlugins>();
    state.failed_mtimes.remove(plugin_id);
    state.plugins.insert(
        plugin_id.to_owned(),
        LoadedPlugin {
            library,
            plugin_name,
            source_path,
            loaded_mtime: mtime,
            reload_count: copy_index,
            active: false,
        },
    );
    refresh_loaded_plugin_ids(world);

    // Gameplay mode wants it running immediately; the editor leaves it
    // registered but idle.
    if in_gameplay(world) {
        activate_plugin(world, plugin_id);
    }

    // Say what the plugin actually contributed — "loaded successfully" alone
    // doesn't tell you whether its scripts reached the registry.
    let script_count = world
        .resource::<ScriptRegistry>()
        .scripts()
        .filter(|s| s.plugin == plugin_id)
        .count();
    let has_hooks = world
        .resource::<ScriptRegistry>()
        .level_hooks(plugin_id)
        .is_some();
    bevy::log::info!(
        "[scripts] '{plugin_id}' registered {script_count} script(s){}",
        if has_hooks {
            " and level-data hooks"
        } else {
            ""
        }
    );
}

/// Ids of every plugin whose systems are currently running.
#[cfg(not(target_family = "wasm"))]
fn active_plugin_ids(world: &World) -> Vec<String> {
    world
        .resource::<LoadedPlugins>()
        .plugins
        .iter()
        .filter(|(_, p)| p.active)
        .map(|(id, _)| id.clone())
        .collect()
}

/// Mirror `LoadedPlugins`' keys into the editor-visible [`LoadedPluginIds`].
#[cfg(not(target_family = "wasm"))]
fn refresh_loaded_plugin_ids(world: &mut World) {
    let mut ids: Vec<String> = world
        .resource::<LoadedPlugins>()
        .plugins
        .keys()
        .cloned()
        .collect();
    ids.sort();
    world.resource_mut::<LoadedPluginIds>().0 = ids;
}

/// Exclusive `Update` system: poll for reload triggers, then run every
/// loaded plugin's schedule.
#[cfg(not(target_family = "wasm"))]
fn run_scripts(world: &mut World) {
    let manual_reload = {
        let key = {
            let input = world.resource::<ButtonInput<KeyCode>>();
            let shift = input.pressed(KeyCode::ShiftLeft) || input.pressed(KeyCode::ShiftRight);
            shift && input.just_pressed(KeyCode::KeyR)
        };
        let requested = std::mem::take(&mut world.resource_mut::<ReloadRequested>().0);
        key || requested
    };

    sync_plugins(world, manual_reload);

    // After `sync_plugins` so a reload in this frame has already dehydrated
    // and re-registered; before the plugin schedules so their systems see
    // fully-hydrated entities on the very first tick after a load.
    hydrate_script_bindings(world);

    // Only active plugins tick. In editor mode every plugin is loaded and
    // registered but inactive, so none of their systems run and they can't
    // fight the editor over keybinds, HUD panels, or the cursor.
    let labels: Vec<PluginSchedule> = world
        .resource::<LoadedPlugins>()
        .plugins
        .iter()
        .filter(|(_, plugin)| plugin.active)
        .map(|(id, _)| PluginSchedule(id.clone()))
        .collect();
    for label in labels {
        let _ = world.try_run_schedule(label);
    }
}

/// Whether plugins should be running right now.
#[cfg(not(target_family = "wasm"))]
fn in_gameplay(world: &World) -> bool {
    world
        .get_resource::<State<crate::editor::EditorMode>>()
        .is_some_and(|state| *state.get() == crate::editor::EditorMode::Gameplay)
}

/// `OnEnter(EditorMode::Gameplay)`: start every loaded plugin.
#[cfg(not(target_family = "wasm"))]
fn activate_all_plugins(world: &mut World) {
    let ids: Vec<String> = world
        .resource::<LoadedPlugins>()
        .plugins
        .iter()
        .filter(|(_, p)| !p.active)
        .map(|(id, _)| id.clone())
        .collect();
    for id in ids {
        activate_plugin(world, &id);
    }
}

/// `OnExit(EditorMode::Gameplay)`: stop every running plugin.
///
/// Every plugin's teardown runs, so this resets the gameplay session —
/// returning to gameplay starts the plugin from scratch.
#[cfg(not(target_family = "wasm"))]
fn deactivate_all_plugins(world: &mut World) {
    let ids: Vec<String> = world
        .resource::<LoadedPlugins>()
        .plugins
        .iter()
        .filter(|(_, p)| p.active)
        .map(|(id, _)| id.clone())
        .collect();
    if ids.is_empty() {
        return;
    }
    for id in ids {
        deactivate_plugin(world, &id);
    }

    // Belt and braces over the per-plugin `unlock_id`: nothing is running now,
    // so nothing has any business holding the player's input.
    if let Some(mut lock) = world.get_resource_mut::<engine_api::InputLock>() {
        lock.clear();
    }
}

/// For each plugin declared by the active project: reload if mtime changed
/// (or if `manual` is set); retry-load if it previously failed and the DLL
/// has been rebuilt since. Plugins outside the active project are ignored,
/// so a sibling plugin's rebuild can't pull it into the running session.
#[cfg(not(target_family = "wasm"))]
fn sync_plugins(world: &mut World, manual: bool) {
    let ids = world
        .get_resource::<ActiveProject>()
        .map(|a| a.0.plugins.clone())
        .unwrap_or_default();

    for id in ids {
        let Some(source_path) = plugin_dll_path(&id) else {
            continue;
        };
        if !source_path.exists() {
            continue;
        }
        let current_mtime = file_mtime(&source_path);

        let state = world.resource::<LoadedPlugins>();
        let is_loaded = state.plugins.contains_key(&id);
        let last_failed_mtime = state.failed_mtimes.get(&id).copied();

        if is_loaded {
            let loaded_mtime = state.plugins.get(&id).and_then(|p| p.loaded_mtime);
            let changed = current_mtime.is_some() && current_mtime != loaded_mtime;
            if manual || changed {
                reload_plugin(world, &id);
            }
        } else {
            // Not loaded. Skip if we already failed at this exact mtime;
            // otherwise try (re)loading. `manual` forces a retry.
            let already_failed_at_this_mtime = matches!(
                last_failed_mtime,
                Some(failed) if failed == current_mtime
            );
            if manual || !already_failed_at_this_mtime {
                try_load_plugin(world, &id);
            }
        }
    }
}

/// Hot-reload a single plugin: prepare new dylib, tear down old, swap schedule,
/// leak old code pages.
#[cfg(not(target_family = "wasm"))]
fn reload_plugin(world: &mut World, plugin_id: &str) {
    // Pull the existing entry out so we can mutate world freely; reinsert at
    // the end (success or failure).
    let mut prev = match world
        .resource_mut::<LoadedPlugins>()
        .plugins
        .remove(plugin_id)
    {
        Some(p) => p,
        None => {
            bevy::log::warn!("[scripts] '{plugin_id}' reload requested but not currently loaded");
            return;
        }
    };

    if !prev.source_path.exists() {
        bevy::log::warn!(
            "[scripts] '{plugin_id}' source DLL {} disappeared, skipping reload",
            prev.source_path.display()
        );
        world
            .resource_mut::<LoadedPlugins>()
            .plugins
            .insert(plugin_id.to_owned(), prev);
        return;
    }

    let new_count = {
        let mut state = world.resource_mut::<LoadedPlugins>();
        let slot = state
            .next_copy_index
            .entry(plugin_id.to_owned())
            .or_insert(0);
        let n = *slot;
        *slot = n.wrapping_add(1);
        n
    };
    let new_mtime = file_mtime(&prev.source_path);

    bevy::log::info!("[scripts] '{plugin_id}' reloading (#{new_count})...");

    // Phase 1: ABI-check the new dylib without touching world state.
    let Some(prepared) = prepare_library(plugin_id, &prev.source_path, new_count) else {
        bevy::log::error!("[scripts] '{plugin_id}' reload failed, keeping previous version");
        // Record the mtime we failed on. Without this the dylib still looks
        // "changed" next frame, so we would retry — and copy the DLL again —
        // every single frame until the user rebuilt it.
        prev.loaded_mtime = new_mtime;
        world
            .resource_mut::<LoadedPlugins>()
            .plugins
            .insert(plugin_id.to_owned(), prev);
        return;
    };

    // Whether to bring the replacement back up at the end. A reload in editor
    // mode re-registers and stops there.
    let was_active = prev.active;

    // Remove the old dylib's hydrated components while its code (and so its
    // drop glue) is still mapped, and clear its registry entries so the new
    // dylib's `register` re-registers into a clean slot. The bindings
    // themselves survive, re-armed with `NeedsHydration`.
    dehydrate_plugin(world, plugin_id);

    // Tear down OLD before NEW spawns its entities — otherwise the old
    // teardown's plugin-name match would catch the new plugin's entities.
    // Put the entry back first so `deactivate_plugin` can find it; it also
    // drops the old schedule while the old library is still mapped, so the
    // systems' drop glue runs safely.
    world
        .resource_mut::<LoadedPlugins>()
        .plugins
        .insert(plugin_id.to_owned(), prev);
    deactivate_plugin(world, plugin_id);
    let mut prev = world
        .resource_mut::<LoadedPlugins>()
        .plugins
        .remove(plugin_id)
        .expect("entry was just re-inserted");

    // Leak old code pages: stray vtable pointers may still reference them.
    let old_library = std::mem::replace(&mut prev.library, prepared.library);
    std::mem::forget(old_library);

    prev.plugin_name = prepared.plugin_name;
    prev.loaded_mtime = new_mtime;
    prev.reload_count = new_count;
    prev.active = false;

    world
        .resource_mut::<LoadedPlugins>()
        .plugins
        .insert(plugin_id.to_owned(), prev);

    // Phase 2: register the new dylib's scripts.
    if !run_register(world, plugin_id, prepared.register_fn) {
        bevy::log::error!(
            "[scripts] '{plugin_id}' new plugin_register failed after old teardown — \
             plugin state is empty until next successful reload"
        );
        refresh_loaded_plugin_ids(world);
        return;
    }

    if was_active {
        activate_plugin(world, plugin_id);
    }

    refresh_loaded_plugin_ids(world);

    let script_count = world
        .resource::<ScriptRegistry>()
        .scripts()
        .filter(|s| s.plugin == plugin_id)
        .count();
    bevy::log::info!(
        "[scripts] '{plugin_id}' reload #{new_count} complete ({script_count} script(s))"
    );
}

// ---------------------------------------------------------------------------
// Script binding hydration
// ---------------------------------------------------------------------------

/// Realise [`ScriptBindings`] into plugin components.
///
/// For each binding not already hydrated, this snapshots the entity's
/// archetype, calls the plugin's factory, and diffs the archetype again to
/// learn exactly which components the factory inserted. Diffing beats asking
/// plugins for a matching "detach" fn: it cannot drift from the factory, and
/// it also catches host components the factory inserted (`AutoCollider`,
/// `Interactable`), which are equally part of what must be undone.
///
/// Entities whose plugin isn't loaded keep [`NeedsHydration`] and are retried
/// each frame — that is how a plugin added via the pause menu picks up
/// already-spawned entities without a reload.
#[cfg(not(target_family = "wasm"))]
fn hydrate_script_bindings(world: &mut World) {
    let pending: Vec<Entity> = world
        .query_filtered::<Entity, (With<ScriptBindings>, With<NeedsHydration>)>()
        .iter(world)
        .collect();

    for entity in pending {
        let Some(bindings) = world.get::<ScriptBindings>(entity).cloned() else {
            continue;
        };

        // (plugin, script) pairs already realised on this entity.
        let done: Vec<(String, String)> = world
            .get::<HydratedScripts>(entity)
            .map(|h| {
                h.0.iter()
                    .map(|s| (s.plugin.clone(), s.script.clone()))
                    .collect()
            })
            .unwrap_or_default();

        let mut newly_hydrated: Vec<HydratedScript> = Vec::new();
        let mut all_resolved = true;

        for binding in bindings.iter() {
            if done
                .iter()
                .any(|(p, s)| p == &binding.plugin && s == &binding.script)
            {
                continue;
            }

            // Copy the fn pointer out so the registry borrow ends before we
            // take a mutable borrow of the world to run it.
            let hydrate: Option<ScriptHydrateFn> = world
                .resource::<ScriptRegistry>()
                .get(&binding.plugin, &binding.script)
                .map(|r| r.hydrate);

            let Some(hydrate) = hydrate else {
                // Plugin not loaded, or loaded but doesn't export this script.
                // Leave the marker so it resolves if the plugin shows up
                // later; warn once so a typo'd script name isn't silent.
                all_resolved = false;
                let key = (binding.plugin.clone(), binding.script.clone());
                let is_new = world.resource_mut::<ReportedMissingScripts>().0.insert(key);
                if is_new {
                    let plugin_loaded = world
                        .resource::<LoadedPlugins>()
                        .plugins
                        .contains_key(&binding.plugin);
                    if plugin_loaded {
                        bevy::log::warn!(
                            "[scripts] '{}' is loaded but exports no script named '{}' \
                             — binding left dormant",
                            binding.plugin,
                            binding.script
                        );
                    } else {
                        bevy::log::warn!(
                            "[scripts] script '{}' needs plugin '{}', which is not loaded \
                             — binding left dormant",
                            binding.script,
                            binding.plugin
                        );
                    }
                }
                continue;
            };

            let Ok(entity_ref) = world.get_entity(entity) else {
                break; // despawned underneath us
            };
            let before: HashSet<ComponentId> = entity_ref
                .archetype()
                .components()
                .iter()
                .copied()
                .collect();

            let panicked = std::panic::catch_unwind(AssertUnwindSafe(|| {
                let mut em = world.entity_mut(entity);
                hydrate(&mut em, binding);
            }))
            .is_err();

            if panicked {
                bevy::log::error!(
                    "[scripts] '{}' script '{}' panicked while hydrating {entity:?}",
                    binding.plugin,
                    binding.script
                );
            }

            let Ok(entity_ref) = world.get_entity(entity) else {
                break;
            };
            let after: HashSet<ComponentId> = entity_ref
                .archetype()
                .components()
                .iter()
                .copied()
                .collect();

            // Record the diff even if the factory panicked part-way, so that
            // whatever it did manage to insert is still cleaned up on unload.
            newly_hydrated.push(HydratedScript {
                plugin: binding.plugin.clone(),
                script: binding.script.clone(),
                component_ids: after.difference(&before).copied().collect(),
            });
        }

        let Ok(mut entity_mut) = world.get_entity_mut(entity) else {
            continue;
        };

        if !newly_hydrated.is_empty() {
            match entity_mut.get_mut::<HydratedScripts>() {
                Some(mut existing) => existing.0.extend(newly_hydrated),
                None => {
                    entity_mut.insert(HydratedScripts(newly_hydrated));
                }
            }
        }

        if all_resolved {
            entity_mut.remove::<NeedsHydration>();
        }
    }
}

/// Strip every component `plugin`'s factories inserted, and re-arm the
/// entities for hydration.
///
/// Must run while the plugin's code pages are still mapped, because removing
/// a component runs its drop glue, which lives in the dylib. Both call sites
/// (unload and reload) invoke this before `plugin_teardown` and well before
/// the library is `mem::forget`'d.
#[cfg(not(target_family = "wasm"))]
fn dehydrate_plugin(world: &mut World, plugin_id: &str) {
    let candidates: Vec<Entity> = world
        .query_filtered::<Entity, With<HydratedScripts>>()
        .iter(world)
        .collect();

    for entity in candidates {
        let Some(hydrated) = world.get::<HydratedScripts>(entity) else {
            continue;
        };
        let to_remove: Vec<ComponentId> = hydrated
            .0
            .iter()
            .filter(|s| s.plugin == plugin_id)
            .flat_map(|s| s.component_ids.iter().copied())
            .collect();
        if to_remove.is_empty() {
            continue;
        }

        let Ok(mut entity_mut) = world.get_entity_mut(entity) else {
            continue;
        };
        entity_mut.remove_by_ids(&to_remove);
        if let Some(mut hydrated) = entity_mut.get_mut::<HydratedScripts>() {
            hydrated.0.retain(|s| s.plugin != plugin_id);
        }
        // The binding is still authored on the entity, so re-arm it; if the
        // plugin is coming back (hot-reload) it re-hydrates next frame.
        entity_mut.insert(NeedsHydration);
    }

    world
        .resource_mut::<ScriptRegistry>()
        .remove_plugin(plugin_id);
    world
        .resource_mut::<AppliedPluginData>()
        .plugins
        .remove(plugin_id);
    // Let a reload that still can't resolve these bindings warn again.
    world
        .resource_mut::<ReportedMissingScripts>()
        .0
        .retain(|(p, _)| p != plugin_id);
}

/// Strip the components one specific binding hydrated onto `entity`, for the
/// editor's "detach script" action. The binding itself has already been
/// removed from `ScriptBindings` by the caller; this only undoes its effects.
///
/// Exclusive because it needs `remove_by_ids`; drive it from a system via
/// `commands.queue(move |world| detach_binding(world, ...))`.
#[cfg(not(target_family = "wasm"))]
pub fn detach_binding(world: &mut World, entity: Entity, plugin: &str, script: &str) {
    let Ok(mut entity_mut) = world.get_entity_mut(entity) else {
        return;
    };
    let Some(mut hydrated) = entity_mut.get_mut::<HydratedScripts>() else {
        return; // never hydrated (plugin not loaded) — nothing to undo
    };
    let Some(pos) = hydrated
        .0
        .iter()
        .position(|s| s.plugin == plugin && s.script == script)
    else {
        return;
    };
    let removed = hydrated.0.remove(pos);
    let now_empty = hydrated.0.is_empty();
    entity_mut.remove_by_ids(&removed.component_ids);
    if now_empty {
        entity_mut.remove::<HydratedScripts>();
    }
}

#[cfg(target_family = "wasm")]
pub fn detach_binding(_world: &mut World, _entity: Entity, _plugin: &str, _script: &str) {}

// ---------------------------------------------------------------------------
// Plugin level data
// ---------------------------------------------------------------------------

/// Hand each loaded plugin its slice of the world's saved `plugin_data`.
///
/// Runs once per (world, plugin): on world load, when a plugin is added
/// mid-session, and again after a hot-reload (which clears the plugin's entry
/// via [`dehydrate_plugin`]). Tracking the world id rather than a change
/// detection flag keeps this correct across `switch_world`, which swaps
/// `TerrainWorld` wholesale.
#[cfg(not(target_family = "wasm"))]
fn apply_plugin_level_data(world: &mut World) {
    use crate::game::world::terrain::storage::TerrainWorld;

    let Some(terrain_world) = world.get_resource::<TerrainWorld>() else {
        return;
    };
    let world_id = terrain_world.metadata.world_id.clone();

    // A different world means none of the previous applications count.
    {
        let mut applied = world.resource_mut::<AppliedPluginData>();
        if applied.world_id != world_id {
            applied.world_id = world_id.clone();
            applied.plugins.clear();
        }
    }

    // Active plugins only: the hooks target resources that `setup` creates,
    // which don't exist while the plugin is idle.
    let loaded: Vec<String> = active_plugin_ids(world);

    for plugin_id in loaded {
        if world
            .resource::<AppliedPluginData>()
            .plugins
            .contains(&plugin_id)
        {
            continue;
        }

        let hooks = world.resource::<ScriptRegistry>().level_hooks(&plugin_id);

        // Mark applied regardless of whether a hook or a blob exists, so this
        // doesn't re-scan the same plugin every frame.
        world
            .resource_mut::<AppliedPluginData>()
            .plugins
            .insert(plugin_id.clone());

        let Some(hooks) = hooks else {
            continue;
        };

        let blob: Option<Vec<u8>> = world
            .resource::<TerrainWorld>()
            .metadata
            .plugin_data
            .iter()
            .find(|d| d.plugin == plugin_id)
            .map(|d| d.data.clone());

        let Some(blob) = blob else {
            continue;
        };

        let failed = std::panic::catch_unwind(AssertUnwindSafe(|| {
            (hooks.load)(world, &blob);
        }))
        .is_err();

        if failed {
            bevy::log::error!("[scripts] '{plugin_id}' level-data load hook panicked");
        } else {
            bevy::log::info!(
                "[scripts] '{plugin_id}' loaded {} byte(s) of level data for world '{world_id}'",
                blob.len()
            );
        }
    }
}

/// Ask every loaded plugin for its level data and stage it on the live
/// `TerrainWorld.metadata`, which the terrain save then writes out.
#[cfg(not(target_family = "wasm"))]
fn collect_plugin_level_data(
    world: &mut World,
    mut cursor: Local<
        bevy::ecs::message::MessageCursor<crate::game::world::terrain::save::SaveTerrainMessage>,
    >,
) {
    use crate::game::world::terrain::save::SaveTerrainMessage;
    use crate::game::world::terrain::storage::{PluginLevelData, TerrainWorld};

    let requested = {
        let Some(messages) =
            world.get_resource::<bevy::ecs::message::Messages<SaveTerrainMessage>>()
        else {
            return;
        };
        cursor.read(messages).count() > 0
    };
    if !requested {
        return;
    }
    if !world.contains_resource::<TerrainWorld>() {
        return;
    }

    // Active plugins only, matching `apply_plugin_level_data`. Saving from
    // editor mode therefore leaves existing blobs untouched rather than
    // overwriting them with the state of a plugin that isn't running: the
    // loop below only rewrites slots for plugins it actually visits.
    let loaded: Vec<String> = active_plugin_ids(world);

    for plugin_id in loaded {
        let Some(hooks) = world.resource::<ScriptRegistry>().level_hooks(&plugin_id) else {
            continue;
        };

        let result = std::panic::catch_unwind(AssertUnwindSafe(|| (hooks.save)(world)));

        let blob = match result {
            Ok(blob) => blob,
            Err(_) => {
                bevy::log::error!("[scripts] '{plugin_id}' level-data save hook panicked");
                continue;
            }
        };

        let mut terrain_world = world.resource_mut::<TerrainWorld>();
        let slot = terrain_world
            .metadata
            .plugin_data
            .iter_mut()
            .position(|d| d.plugin == plugin_id);

        match (blob, slot) {
            (Some(data), Some(i)) => terrain_world.metadata.plugin_data[i].data = data,
            (Some(data), None) => terrain_world.metadata.plugin_data.push(PluginLevelData {
                plugin: plugin_id.clone(),
                data,
            }),
            // An explicit `None` means "store nothing for me", so drop any
            // blob a previous save left behind.
            (None, Some(i)) => {
                terrain_world.metadata.plugin_data.remove(i);
            }
            (None, None) => {}
        }
    }

    // Record which plugins this level's bindings depend on, so a later load
    // without them can warn.
    //
    // This is a *union* with the stored list, not a replacement. Only loaded
    // chunks have entities, so a replacement would silently drop a dependency
    // whose only bindings happen to sit in a chunk the player has streamed
    // out. The cost of the union is a stale entry after the last binding for
    // a plugin is deleted, which at worst produces a spurious warning on load
    // — strictly better than losing a real dependency.
    let live: Vec<String> = world
        .query::<&ScriptBindings>()
        .iter(world)
        .flat_map(|b| b.0.iter().map(|binding| binding.plugin.clone()))
        .collect();

    let mut terrain_world = world.resource_mut::<TerrainWorld>();
    let required = &mut terrain_world.metadata.required_plugins;
    required.extend(live);
    required.sort();
    required.dedup();
}

// ---------------------------------------------------------------------------
// Platform helpers
// ---------------------------------------------------------------------------

#[cfg(all(not(target_family = "wasm"), target_os = "windows"))]
fn add_dll_search_dir(dir: &Path) {
    use std::os::windows::ffi::OsStrExt;
    let wide: Vec<u16> = dir
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    unsafe {
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn SetDllDirectoryW(path: *const u16) -> i32;
        }
        SetDllDirectoryW(wide.as_ptr());
    }
}

#[cfg(all(not(target_family = "wasm"), not(target_os = "windows")))]
fn add_dll_search_dir(_dir: &Path) {}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// Hydration is exercised without any dylib: script factories are plain `fn`
/// pointers, so a test can register one directly and drive the same
/// hydrate / dehydrate / re-hydrate cycle the loader drives.
#[cfg(all(test, not(target_family = "wasm")))]
mod tests {
    use super::*;
    use engine_api::{ScriptBinding, ScriptSchema};

    /// Stands in for a plugin-private component (the thing whose `TypeId`
    /// would change on every rebuild).
    #[derive(Component, Debug, PartialEq)]
    struct Spinner(f32);

    /// A second component the same factory adds, to prove the diff captures
    /// every insert rather than just the first.
    #[derive(Component)]
    struct AlsoAdded;

    /// Something the entity already had; dehydration must not touch it.
    #[derive(Component)]
    struct PreExisting;

    fn hydrate_spinner(entity: &mut bevy::ecs::world::EntityWorldMut, binding: &ScriptBinding) {
        entity.insert((Spinner(binding.float_or("speed", 1.0)), AlsoAdded));
    }

    fn test_world() -> World {
        let mut world = World::new();
        world.init_resource::<ScriptRegistry>();
        world.init_resource::<ReportedMissingScripts>();
        world.init_resource::<AppliedPluginData>();
        world.insert_resource(LoadedPlugins::default());
        world
    }

    fn spawn_bound(world: &mut World) -> Entity {
        let binding =
            ScriptBinding::new("example", "spinner").with_props(vec![engine_api::ScriptProp::new(
                "speed",
                engine_api::PropValue::Float(2.5),
            )]);
        world
            .spawn((PreExisting, ScriptBindings(vec![binding]), NeedsHydration))
            .id()
    }

    fn register(world: &mut World) {
        world.resource_mut::<ScriptRegistry>().register_script(
            "example",
            ScriptSchema::new("spinner"),
            hydrate_spinner,
        );
    }

    #[test]
    fn hydration_inserts_components_and_clears_the_marker() {
        let mut world = test_world();
        register(&mut world);
        let entity = spawn_bound(&mut world);

        hydrate_script_bindings(&mut world);

        assert_eq!(world.get::<Spinner>(entity), Some(&Spinner(2.5)));
        assert!(world.get::<AlsoAdded>(entity).is_some());
        assert!(
            world.get::<NeedsHydration>(entity).is_none(),
            "a fully resolved entity should no longer be queued"
        );
    }

    #[test]
    fn hydration_is_idempotent_across_frames() {
        let mut world = test_world();
        register(&mut world);
        let entity = spawn_bound(&mut world);

        hydrate_script_bindings(&mut world);
        hydrate_script_bindings(&mut world);

        // One binding must not accumulate duplicate hydration records, or
        // dehydration would try to remove the same ids twice.
        assert_eq!(world.get::<HydratedScripts>(entity).unwrap().0.len(), 1);
    }

    #[test]
    fn dehydration_removes_exactly_what_the_factory_added() {
        let mut world = test_world();
        register(&mut world);
        let entity = spawn_bound(&mut world);
        hydrate_script_bindings(&mut world);

        dehydrate_plugin(&mut world, "example");

        assert!(world.get::<Spinner>(entity).is_none());
        assert!(world.get::<AlsoAdded>(entity).is_none());
        // Untouched components and the authored bindings both survive.
        assert!(world.get::<PreExisting>(entity).is_some());
        assert!(world.get::<ScriptBindings>(entity).is_some());
        // Re-armed, so a reload picks it back up.
        assert!(world.get::<NeedsHydration>(entity).is_some());
        assert!(
            world
                .resource::<ScriptRegistry>()
                .get("example", "spinner")
                .is_none()
        );
    }

    #[test]
    fn reload_cycle_rehydrates_from_the_new_registration() {
        let mut world = test_world();
        register(&mut world);
        let entity = spawn_bound(&mut world);
        hydrate_script_bindings(&mut world);
        dehydrate_plugin(&mut world, "example");

        // Stand-in for the rebuilt dylib registering a changed factory.
        fn hydrate_v2(entity: &mut bevy::ecs::world::EntityWorldMut, _: &ScriptBinding) {
            entity.insert(Spinner(99.0));
        }
        world.resource_mut::<ScriptRegistry>().register_script(
            "example",
            ScriptSchema::new("spinner"),
            hydrate_v2,
        );

        hydrate_script_bindings(&mut world);

        assert_eq!(world.get::<Spinner>(entity), Some(&Spinner(99.0)));
        assert!(world.get::<NeedsHydration>(entity).is_none());
    }

    #[test]
    fn binding_for_an_unloaded_plugin_stays_dormant_then_resolves() {
        let mut world = test_world();
        // Nothing registered yet — this is the "level references a plugin the
        // project doesn't load" case.
        let entity = spawn_bound(&mut world);

        hydrate_script_bindings(&mut world);

        assert!(world.get::<Spinner>(entity).is_none());
        assert!(
            world.get::<NeedsHydration>(entity).is_some(),
            "an unresolved binding must stay queued so a later load picks it up"
        );
        assert!(world.get::<ScriptBindings>(entity).is_some());

        // Plugin shows up mid-session (e.g. added via the pause menu).
        register(&mut world);
        hydrate_script_bindings(&mut world);

        assert_eq!(world.get::<Spinner>(entity), Some(&Spinner(2.5)));
        assert!(world.get::<NeedsHydration>(entity).is_none());
    }

    #[test]
    fn dehydrating_one_plugin_leaves_another_plugins_components_alone() {
        #[derive(Component)]
        struct OtherComponent;

        fn hydrate_other(entity: &mut bevy::ecs::world::EntityWorldMut, _: &ScriptBinding) {
            entity.insert(OtherComponent);
        }

        let mut world = test_world();
        register(&mut world);
        world.resource_mut::<ScriptRegistry>().register_script(
            "other",
            ScriptSchema::new("thing"),
            hydrate_other,
        );

        let entity = world
            .spawn((
                ScriptBindings(vec![
                    ScriptBinding::new("example", "spinner"),
                    ScriptBinding::new("other", "thing"),
                ]),
                NeedsHydration,
            ))
            .id();
        hydrate_script_bindings(&mut world);
        assert!(world.get::<Spinner>(entity).is_some());
        assert!(world.get::<OtherComponent>(entity).is_some());

        dehydrate_plugin(&mut world, "example");

        assert!(world.get::<Spinner>(entity).is_none());
        assert!(
            world.get::<OtherComponent>(entity).is_some(),
            "unloading one plugin must not strip another's components"
        );
    }

    #[test]
    fn a_panicking_factory_is_contained_and_its_partial_work_still_tracked() {
        fn hydrate_panics(entity: &mut bevy::ecs::world::EntityWorldMut, _: &ScriptBinding) {
            entity.insert(AlsoAdded);
            panic!("plugin bug");
        }

        let mut world = test_world();
        world.resource_mut::<ScriptRegistry>().register_script(
            "example",
            ScriptSchema::new("spinner"),
            hydrate_panics,
        );
        let entity = spawn_bound(&mut world);

        // Must not unwind out of the host.
        hydrate_script_bindings(&mut world);

        // Whatever it managed to insert before panicking is still recorded,
        // so unloading the plugin cleans it up rather than leaking it.
        assert!(world.get::<AlsoAdded>(entity).is_some());
        dehydrate_plugin(&mut world, "example");
        assert!(world.get::<AlsoAdded>(entity).is_none());
    }
}
