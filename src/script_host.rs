//! Host-side plugin loader.
//!
//! A plugin reaches the host one of two ways ([`PluginBackend`]):
//!
//! - **Static**: compiled into the binary and listed in [`StaticPlugins`].
//!   The only option on wasm, which has no dylibs. Never hot-reloaded.
//! - **Dylib** (native only): `<exe-dir>/<id>.dll` (cargo's artifact name for
//!   the crate under `plugins/<id>`), loaded through `libloading`. Rebuild a
//!   plugin and the loader picks up the mtime change; **Shift+R** forces a
//!   reload of everything.
//!
//! A static entry wins over a dylib of the same id. Everything past loading —
//! the per-plugin [`PluginSchedule`], activation on entering gameplay, script
//! hydration, level data — is the same for both. `docs/STATIC_PLUGINS_PLAN.md`.

use std::collections::{HashMap, HashSet};
use std::panic::AssertUnwindSafe;
#[cfg(not(target_family = "wasm"))]
use std::{
    ffi::c_void,
    path::{Path, PathBuf},
    time::SystemTime,
};

use bevy::ecs::component::ComponentId;
use bevy::ecs::schedule::ScheduleLabel;
use bevy::prelude::*;
use engine_api::{AppState, DynamicPlugin, ScriptBindings, ScriptHydrateFn, ScriptRegistry};

use crate::projects::ActiveProject;

// ---------------------------------------------------------------------------
// Schedule
// ---------------------------------------------------------------------------

/// One schedule per loaded plugin, keyed by plugin id (the source directory
/// name under `plugins/`). Replaced wholesale on hot-reload.
#[derive(ScheduleLabel, Debug, Clone, PartialEq, Eq, Hash)]
pub struct PluginSchedule(pub String);

// ---------------------------------------------------------------------------
// Static plugins
// ---------------------------------------------------------------------------

/// Builds a statically linked plugin. `Send + Sync` because the instance lives
/// in a resource.
pub type StaticPluginCtor = fn() -> Box<dyn DynamicPlugin + Send + Sync>;

/// Plugins compiled into this binary, by id ([`crate::static_plugins::all`]).
/// Empty unless the build links plugins in; a project's manifest still decides
/// which of them load.
#[derive(Resource, Default)]
pub struct StaticPlugins(pub Vec<(&'static str, StaticPluginCtor)>);

impl StaticPlugins {
    fn get(&self, id: &str) -> Option<StaticPluginCtor> {
        self.0.iter().find(|(name, _)| *name == id).map(|(_, f)| *f)
    }
}

#[cfg(not(target_family = "wasm"))]
fn is_static(world: &World, id: &str) -> bool {
    world.resource::<StaticPlugins>().get(id).is_some()
}

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
#[derive(Component, Default)]
pub struct NeedsHydration;

/// Ids of the plugins actually loaded right now.
///
/// Distinct from `ActiveProject.plugins`, which is only what the project
/// *declares*. A plugin can be declared but absent here because its dylib was
/// never built (`cargo run` does not build plugin crates), it isn't compiled
/// into this binary, it failed its ABI check, or it panicked in `register`.
/// The editor uses the difference between the two lists to explain an empty
/// script list instead of just showing nothing.
#[derive(Resource, Default)]
pub struct LoadedPluginIds(pub Vec<String>);

/// Set by the editor's Plugins ▸ Reload all; equivalent to Shift+R. Cleared
/// when the reload runs.
#[derive(Resource, Default)]
pub struct ReloadRequested(pub bool);

/// Records which components a plugin's hydrate fn actually inserted, so the
/// host can remove exactly those again when the plugin goes away.
#[derive(Component, Default)]
struct HydratedScripts(Vec<HydratedScript>);

struct HydratedScript {
    plugin: String,
    script: String,
    /// Components added by the factory call, captured as an archetype diff.
    component_ids: Vec<ComponentId>,
}

/// Which plugins have had their level-data `load` hook run for the currently
/// loaded world, so a hook runs once per (world, plugin) rather than per frame.
#[derive(Resource, Default)]
struct AppliedPluginData {
    world_id: String,
    plugins: HashSet<String>,
}

/// `(plugin, script)` pairs already warned about, so an unresolvable binding
/// logs once instead of every frame. Entries for a plugin are dropped when it
/// unloads, so a reload that still fails warns again.
#[derive(Resource, Default)]
struct ReportedMissingScripts(HashSet<(String, String)>);

// ---------------------------------------------------------------------------
// Plugin
// ---------------------------------------------------------------------------

pub struct ScriptHostPlugin;

impl Plugin for ScriptHostPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(crate::plugin_files::PluginFilesPlugin);
        app.init_resource::<ScriptRegistry>();
        app.insert_resource(StaticPlugins(crate::static_plugins::all()));
        app.init_resource::<ReloadRequested>();
        app.init_resource::<LoadedPluginIds>();
        app.init_resource::<LoadedPlugins>();
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
}

// ---------------------------------------------------------------------------
// Backends
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

/// Where a loaded plugin's code lives.
enum PluginBackend {
    #[cfg(not(target_family = "wasm"))]
    Dylib(DylibPlugin),
    Static(Box<dyn DynamicPlugin + Send + Sync>),
}

/// A mapped dylib plus what hot-reload needs to know about it.
#[cfg(not(target_family = "wasm"))]
struct DylibPlugin {
    library: libloading::Library,
    /// Path to the original DLL (the one cargo writes to).
    source_path: PathBuf,
    /// mtime of the source DLL when we last loaded it.
    loaded_mtime: Option<SystemTime>,
}

/// Per-plugin state held for the lifetime of the process (or until hot-reload
/// replaces it).
struct LoadedPlugin {
    backend: PluginBackend,
    /// Plugin's self-reported name (used for logging only — not the same as
    /// the plugin id, which is the source directory name).
    plugin_name: String,
    /// Whether the plugin's `setup` has run and its schedule is installed.
    ///
    /// Loading and activation are independent: a plugin is loaded whenever
    /// the project declares it, but only *active* in `EditorMode::Gameplay`.
    /// While inactive its scripts stay in the registry (so the editor can
    /// still list and attach them) but none of its systems run.
    active: bool,
}

#[derive(Resource, Default)]
struct LoadedPlugins {
    plugins: HashMap<String, LoadedPlugin>,
    /// mtime of last *failed* dylib load attempt per plugin id. Used to skip
    /// retrying a busted DLL until its mtime changes.
    #[cfg(not(target_family = "wasm"))]
    failed_mtimes: HashMap<String, Option<SystemTime>>,
    /// Next unused copy index per plugin id. Persists across unload/reload
    /// cycles within a process — unloaded libraries are `mem::forget`'d,
    /// so their copy files stay locked, and reusing index 0 would collide.
    #[cfg(not(target_family = "wasm"))]
    next_copy_index: HashMap<String, u32>,
}

impl PluginBackend {
    /// Run `register`. Returns `false` (already logged) on failure, in which
    /// case the caller must roll the load back.
    ///
    /// Registration is mode-independent — it only populates the script
    /// registry — so this runs on every load and hot-reload, in both editor
    /// and gameplay mode.
    fn register(&self, world: &mut World, plugin_id: &str) -> bool {
        let result = match self {
            #[cfg(not(target_family = "wasm"))]
            PluginBackend::Dylib(dylib) => {
                // Make sure Time is registered host-side before the dylib touches it.
                let _ = world.register_component::<bevy::time::Time>();
                // SAFETY: `prepare_library` resolved this symbol already.
                let register_fn: PluginRegisterFn =
                    match unsafe { dylib.library.get::<PluginRegisterFn>(b"plugin_register") } {
                        Ok(sym) => *sym,
                        Err(e) => {
                            error!("[scripts] '{plugin_id}' missing plugin_register symbol: {e}");
                            return false;
                        }
                    };
                let world_ptr = world as *mut World as *mut c_void;
                std::panic::catch_unwind(AssertUnwindSafe(|| unsafe { register_fn(world_ptr) }))
            }
            PluginBackend::Static(plugin) => std::panic::catch_unwind(AssertUnwindSafe(|| {
                engine_api::lifecycle::register(plugin.as_ref(), world);
                0
            })),
        };
        succeeded(plugin_id, "register", result)
    }

    /// Run `setup` into `schedule`. Returns `false` (already logged) on failure.
    fn setup(&self, world: &mut World, plugin_id: &str, schedule: &mut Schedule) -> bool {
        let result = match self {
            #[cfg(not(target_family = "wasm"))]
            PluginBackend::Dylib(dylib) => {
                let setup_fn: PluginSetupFn =
                    match unsafe { dylib.library.get::<PluginSetupFn>(b"plugin_setup") } {
                        Ok(sym) => *sym,
                        Err(e) => {
                            error!("[scripts] '{plugin_id}' missing plugin_setup symbol: {e}");
                            return false;
                        }
                    };
                let _ = world.register_component::<bevy::time::Time>();
                let schedule_ptr = schedule as *mut Schedule as *mut c_void;
                let world_ptr = world as *mut World as *mut c_void;
                std::panic::catch_unwind(AssertUnwindSafe(|| unsafe {
                    setup_fn(world_ptr, schedule_ptr)
                }))
            }
            PluginBackend::Static(plugin) => std::panic::catch_unwind(AssertUnwindSafe(|| {
                plugin.setup(world, schedule);
                0
            })),
        };
        succeeded(plugin_id, "setup", result)
    }

    /// Run `teardown`, which also despawns the plugin's `ScriptOwned`
    /// entities. For a dylib this must happen while its pages are mapped.
    fn teardown(&self, world: &mut World, plugin_id: &str) {
        let result = match self {
            #[cfg(not(target_family = "wasm"))]
            PluginBackend::Dylib(dylib) => {
                let teardown_fn: PluginTeardownFn = match unsafe {
                    dylib.library.get::<PluginTeardownFn>(b"plugin_teardown")
                } {
                    Ok(sym) => *sym,
                    Err(e) => {
                        error!(
                            "[scripts] '{plugin_id}' missing plugin_teardown symbol on unload: {e}"
                        );
                        return;
                    }
                };
                let world_ptr = world as *mut World as *mut c_void;
                std::panic::catch_unwind(AssertUnwindSafe(|| unsafe { teardown_fn(world_ptr) }))
            }
            PluginBackend::Static(plugin) => std::panic::catch_unwind(AssertUnwindSafe(|| {
                engine_api::lifecycle::teardown(plugin.as_ref(), plugin_id, world);
                0
            })),
        };
        if succeeded(plugin_id, "teardown", result) {
            info!("[scripts] '{plugin_id}' torn down");
        }
    }

    /// Let go of the plugin's code. A dylib is `mem::forget`'d, never closed:
    /// stray vtable or fn pointers into it may outlive the unload.
    fn release(self) {
        match self {
            #[cfg(not(target_family = "wasm"))]
            PluginBackend::Dylib(dylib) => std::mem::forget(dylib.library),
            PluginBackend::Static(plugin) => drop(plugin),
        }
    }
}

/// Interpret a lifecycle call's outcome: `0` is success, `2` is the dylib
/// shims' TypeId-mismatch code, anything else (or a panic) a plugin error.
fn succeeded(plugin_id: &str, step: &str, result: std::thread::Result<u32>) -> bool {
    match result {
        Ok(0) => true,
        Ok(2) => {
            error!(
                "[scripts] '{plugin_id}' TypeId mismatch in {step} — the dylib was compiled \
                 against different bevy metadata than the engine. Rebuild both together \
                 (cargo build --workspace) and restart; a dependency only a plugin has \
                 must be mirrored in the engine's Cargo.toml."
            );
            false
        }
        Ok(code) => {
            error!("[scripts] '{plugin_id}' {step} returned error code {code}");
            false
        }
        Err(_) => {
            error!("[scripts] '{plugin_id}' {step} panicked");
            false
        }
    }
}

// ---------------------------------------------------------------------------
// Dylib loading (native only)
// ---------------------------------------------------------------------------

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

/// Take the next copy index for `plugin_id`.
#[cfg(not(target_family = "wasm"))]
fn next_copy_index(world: &mut World, plugin_id: &str) -> u32 {
    let mut state = world.resource_mut::<LoadedPlugins>();
    let slot = state
        .next_copy_index
        .entry(plugin_id.to_owned())
        .or_insert(0);
    let n = *slot;
    *slot = n.wrapping_add(1);
    n
}

/// Copy the DLL, map it, verify the ABI, and check every symbol the host will
/// need exists. Doesn't touch world state, so failure here is safe to roll
/// back from — the caller can keep the previous version running. Returns the
/// library and its self-reported name.
#[cfg(not(target_family = "wasm"))]
fn prepare_library(
    plugin_id: &str,
    source_path: &Path,
    reload_count: u32,
) -> Option<(libloading::Library, String)> {
    if let Some(dir) = source_path.parent() {
        add_dll_search_dir(dir);
    }

    let copy_path = match copy_dll(source_path, reload_count) {
        Ok(p) => p,
        Err(e) => {
            error!(
                "[scripts] '{plugin_id}' failed to copy {}: {e}",
                source_path.display()
            );
            return None;
        }
    };

    info!(
        "[scripts] '{plugin_id}' loading {} (copy: {})",
        source_path.display(),
        copy_path.display()
    );

    let library = match unsafe { libloading::Library::new(&copy_path) } {
        Ok(lib) => lib,
        Err(e) => {
            error!(
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
                error!("[scripts] '{plugin_id}' missing plugin_abi_version symbol: {e}");
                return None;
            }
        }
    };
    if dylib_abi != engine_api::ENGINE_SCRIPTS_ABI_VERSION {
        error!(
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
                    error!("[scripts] '{plugin_id}' plugin_name returned null");
                    return None;
                }
                std::ffi::CStr::from_ptr(ptr).to_string_lossy().into_owned()
            }
            Err(e) => {
                error!("[scripts] '{plugin_id}' missing plugin_name symbol: {e}");
                return None;
            }
        }
    };

    // Resolved up front so a dylib missing any lifecycle symbol is rejected in
    // this phase, while rolling back is still free.
    for symbol in [
        &b"plugin_register"[..],
        &b"plugin_setup"[..],
        &b"plugin_teardown"[..],
    ] {
        if let Err(e) = unsafe { library.get::<*const c_void>(symbol) } {
            error!(
                "[scripts] '{plugin_id}' missing {} symbol: {e}",
                String::from_utf8_lossy(symbol)
            );
            return None;
        }
    }

    Some((library, plugin_name))
}

/// Find and map `plugin_id`'s dylib. Records the failed mtime on failure so
/// we don't retry until the DLL is rebuilt.
#[cfg(not(target_family = "wasm"))]
fn open_dylib(world: &mut World, plugin_id: &str) -> Option<(PluginBackend, String)> {
    let Some(source_path) = plugin_dll_path(plugin_id) else {
        warn!("[scripts] '{plugin_id}' could not resolve exe path");
        return None;
    };
    if !source_path.exists() {
        warn!(
            "[scripts] '{plugin_id}' DLL not found at {} (build it with `cargo build -p {plugin_id}`)",
            source_path.display()
        );
        return None;
    }

    let mtime = file_mtime(&source_path);
    let copy_index = next_copy_index(world, plugin_id);
    let Some((library, plugin_name)) = prepare_library(plugin_id, &source_path, copy_index) else {
        world
            .resource_mut::<LoadedPlugins>()
            .failed_mtimes
            .insert(plugin_id.to_owned(), mtime);
        return None;
    };
    let backend = PluginBackend::Dylib(DylibPlugin {
        library,
        source_path,
        loaded_mtime: mtime,
    });
    Some((backend, plugin_name))
}

// ---------------------------------------------------------------------------
// Load / activate / deactivate / unload
// ---------------------------------------------------------------------------

/// Lift `plugin_id`'s entry out of `LoadedPlugins` for the duration of `f`,
/// so plugin code gets an unaliased `&mut World`, and put it back afterwards.
fn with_entry<R>(
    world: &mut World,
    plugin_id: &str,
    f: impl FnOnce(&mut World, &mut LoadedPlugin) -> R,
) -> Option<R> {
    let mut entry = world
        .resource_mut::<LoadedPlugins>()
        .plugins
        .remove(plugin_id)?;
    let result = f(world, &mut entry);
    world
        .resource_mut::<LoadedPlugins>()
        .plugins
        .insert(plugin_id.to_owned(), entry);
    Some(result)
}

/// Start a loaded plugin's gameplay systems: run `setup` and install its
/// schedule. No-op if the plugin isn't loaded or is already active.
fn activate_plugin(world: &mut World, plugin_id: &str) {
    with_entry(world, plugin_id, |world, entry| {
        if entry.active {
            return;
        }

        let mut schedule = Schedule::new(PluginSchedule(plugin_id.to_owned()));
        if !entry.backend.setup(world, plugin_id, &mut schedule) {
            // A partial setup may have spawned ScriptOwned entities; sweep them
            // so the next activation starts clean. Registry entries stay — they
            // came from `register`, which succeeded.
            despawn_owned_entities(world, plugin_id);
            return;
        }

        // Replaces any prior schedule with the same label.
        world
            .resource_mut::<bevy::ecs::schedule::Schedules>()
            .insert(schedule);

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
        info!("[scripts] '{plugin_id}' activated");
    });
}

/// Stop a plugin's gameplay systems: run `teardown` (which sweeps its
/// `ScriptOwned` entities) and drop its schedule. The code stays loaded and
/// its registry entries stay put, so the editor keeps working.
///
/// No-op if the plugin isn't loaded or is already inactive.
fn deactivate_plugin(world: &mut World, plugin_id: &str) {
    with_entry(world, plugin_id, |world, entry| {
        if !entry.active {
            return;
        }

        entry.backend.teardown(world, plugin_id);

        // Dropped while a dylib is still mapped, so the systems' drop glue is
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
        // string may point into dylib pages, so release it by id.
        if let Some(mut lock) = world.get_resource_mut::<engine_api::InputLock>() {
            lock.unlock_id(plugin_id);
        }

        entry.active = false;
        info!("[scripts] '{plugin_id}' deactivated");
    });
}

/// Despawn every entity tagged `ScriptOwned` for `plugin_id`.
///
/// Normally the plugin's teardown does this; the host needs it for the one
/// case where that can't be trusted to have run — a `setup` or `register`
/// that failed or panicked partway through.
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
    warn!(
        "[scripts] '{plugin_id}' left {} entity(ies) behind in a failed lifecycle call; despawning",
        orphans.len()
    );
    for entity in orphans {
        world.despawn(entity);
    }
}

/// Clean up after a `register` that failed or panicked partway through.
///
/// A failed registration may already have mutated the world: registered
/// script factories and level hooks (fn pointers into the plugin) and, if the
/// plugin ignored the contract, spawned entities. A dylib is released with
/// `mem::forget` (pages stay mapped forever), so removing its registry
/// entries and despawning its orphans afterwards is safe.
fn abort_failed_load(world: &mut World, plugin_id: &str, backend: PluginBackend) {
    backend.release();
    world
        .resource_mut::<ScriptRegistry>()
        .remove_plugin(plugin_id);
    despawn_owned_entities(world, plugin_id);
}

/// Exclusive `OnEnter(AppState::InGame)` system: load every plugin declared
/// by the active project.
fn load_active_project_plugins(world: &mut World) {
    #[cfg(not(target_family = "wasm"))]
    {
        info!(
            "[HOST] Time TypeId   = {:?}",
            std::any::TypeId::of::<bevy::time::Time>()
        );
        info!(
            "[HOST] Player TypeId = {:?}",
            std::any::TypeId::of::<engine_api::Player>()
        );
    }

    let Some(active) = world.get_resource::<ActiveProject>() else {
        warn!("[scripts] entered InGame without an ActiveProject — no plugins will load");
        return;
    };
    let ids = active.0.plugins.clone();
    if ids.is_empty() {
        info!("[scripts] project '{}' declares no plugins", active.0.id);
        return;
    }
    info!(
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
/// currently loaded plugin.
fn unload_all_plugins(world: &mut World) {
    let ids: Vec<String> = world
        .resource::<LoadedPlugins>()
        .plugins
        .keys()
        .cloned()
        .collect();
    if !ids.is_empty() {
        info!(
            "[scripts] unloading {} plugin(s) on exit to menu",
            ids.len()
        );
    }

    for id in ids {
        unload_one(world, &id);
    }

    // Wipe the failed mtimes too so a fresh load attempt can proceed next time.
    #[cfg(not(target_family = "wasm"))]
    world.resource_mut::<LoadedPlugins>().failed_mtimes.clear();
}

/// Tear down a single loaded plugin: strip its hydrated components, run
/// teardown, drop its schedule, and release its code. No-op if the id isn't
/// currently loaded.
fn unload_one(world: &mut World, id: &str) {
    if !world.resource::<LoadedPlugins>().plugins.contains_key(id) {
        return;
    }

    // Strip hydrated components first: for a dylib their drop glue lives in
    // pages leaked below. This also clears the plugin's registry entries,
    // which a plain deactivation deliberately leaves alone.
    dehydrate_plugin(world, id);

    // Runs teardown and drops the schedule, but only if it was active.
    deactivate_plugin(world, id);

    let Some(loaded) = world.resource_mut::<LoadedPlugins>().plugins.remove(id) else {
        return;
    };
    loaded.backend.release();
    refresh_loaded_plugin_ids(world);
    // After teardown, which may have queued a last save.
    crate::plugin_files::release(world, id);
    info!("[scripts] '{id}' unloaded");
}

/// Public hook for live-loading a plugin while the game is running. Use
/// from `Commands::queue` so it runs as a deferred exclusive command.
/// Idempotent — if the plugin is already loaded, this is a no-op.
pub fn load_plugin(world: &mut World, id: &str) {
    if world.resource::<LoadedPlugins>().plugins.contains_key(id) {
        return;
    }
    try_load_plugin(world, id);
}

/// Public hook for live-unloading a plugin. Same shape as the on-exit
/// cleanup, but for one id.
pub fn unload_plugin(world: &mut World, id: &str) {
    unload_one(world, id);
}

/// Attempt to load a plugin by id: the static build of it if this binary has
/// one, otherwise (native) its dylib.
fn try_load_plugin(world: &mut World, plugin_id: &str) {
    let ctor = world.resource::<StaticPlugins>().get(plugin_id);
    let opened = match ctor {
        Some(ctor) => Some((PluginBackend::Static(ctor()), plugin_id.to_owned())),
        #[cfg(not(target_family = "wasm"))]
        None => open_dylib(world, plugin_id),
        #[cfg(target_family = "wasm")]
        None => {
            warn!("[scripts] '{plugin_id}' is not compiled into this build");
            None
        }
    };
    let Some((backend, plugin_name)) = opened else {
        return;
    };

    if !backend.register(world, plugin_id) {
        #[cfg(not(target_family = "wasm"))]
        if let PluginBackend::Dylib(dylib) = &backend {
            let mtime = dylib.loaded_mtime;
            world
                .resource_mut::<LoadedPlugins>()
                .failed_mtimes
                .insert(plugin_id.to_owned(), mtime);
        }
        abort_failed_load(world, plugin_id, backend);
        return;
    }

    let mut state = world.resource_mut::<LoadedPlugins>();
    #[cfg(not(target_family = "wasm"))]
    state.failed_mtimes.remove(plugin_id);
    state.plugins.insert(
        plugin_id.to_owned(),
        LoadedPlugin {
            backend,
            plugin_name,
            active: false,
        },
    );
    refresh_loaded_plugin_ids(world);
    crate::plugin_files::request_load(world, plugin_id);

    // Gameplay mode wants it running immediately; the editor leaves it
    // registered but idle.
    if in_gameplay(world) {
        activate_plugin(world, plugin_id);
    }

    // Say what the plugin actually contributed — "loaded successfully" alone
    // doesn't tell you whether its scripts reached the registry.
    let registry = world.resource::<ScriptRegistry>();
    let script_count = registry.scripts().filter(|s| s.plugin == plugin_id).count();
    let has_hooks = registry.level_hooks(plugin_id).is_some();
    let loaded = &world.resource::<LoadedPlugins>().plugins[plugin_id];
    info!(
        "[scripts] '{plugin_id}' ({} \"{}\") registered {script_count} script(s){}",
        match loaded.backend {
            #[cfg(not(target_family = "wasm"))]
            PluginBackend::Dylib(_) => "dylib",
            PluginBackend::Static(_) => "static",
        },
        loaded.plugin_name,
        if has_hooks {
            " and level-data hooks"
        } else {
            ""
        }
    );
}

/// Ids of every plugin whose systems are currently running.
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

    #[cfg(not(target_family = "wasm"))]
    sync_dylibs(world, manual_reload);
    #[cfg(target_family = "wasm")]
    let _ = manual_reload; // nothing to reload: every plugin is static

    // After `sync_dylibs` so a reload in this frame has already dehydrated
    // and re-registered; before the plugin schedules so their systems see
    // fully-hydrated entities on the very first tick after a load.
    hydrate_script_bindings(world);

    // Only active plugins tick. In editor mode every plugin is loaded and
    // registered but inactive, so none of their systems run and they can't
    // fight the editor over keybinds, HUD panels, or the cursor.
    for id in active_plugin_ids(world) {
        let _ = world.try_run_schedule(PluginSchedule(id));
    }
}

/// Whether plugins should be running right now.
fn in_gameplay(world: &World) -> bool {
    world
        .get_resource::<State<crate::editor::EditorMode>>()
        .is_some_and(|state| *state.get() == crate::editor::EditorMode::Gameplay)
}

/// `OnEnter(EditorMode::Gameplay)`: start every loaded plugin.
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
fn deactivate_all_plugins(world: &mut World) {
    let ids = active_plugin_ids(world);
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

// ---------------------------------------------------------------------------
// Dylib hot-reload (native only)
// ---------------------------------------------------------------------------

/// For each dylib plugin declared by the active project: reload if its mtime
/// changed (or if `manual` is set); retry-load if it previously failed and the
/// DLL has been rebuilt since. Static plugins are skipped — their code can't
/// change under a running process. Plugins outside the active project are
/// ignored, so a sibling plugin's rebuild can't pull it into the session.
#[cfg(not(target_family = "wasm"))]
fn sync_dylibs(world: &mut World, manual: bool) {
    let ids = world
        .get_resource::<ActiveProject>()
        .map(|a| a.0.plugins.clone())
        .unwrap_or_default();

    for id in ids {
        if is_static(world, &id) {
            continue;
        }
        let Some(source_path) = plugin_dll_path(&id) else {
            continue;
        };
        if !source_path.exists() {
            continue;
        }
        let current_mtime = file_mtime(&source_path);

        let state = world.resource::<LoadedPlugins>();
        match state.plugins.get(&id).map(|p| &p.backend) {
            Some(PluginBackend::Dylib(dylib)) => {
                let changed = current_mtime.is_some() && current_mtime != dylib.loaded_mtime;
                if manual || changed {
                    reload_dylib(world, &id);
                }
            }
            Some(PluginBackend::Static(_)) => {}
            None => {
                // Not loaded. Skip if we already failed at this exact mtime;
                // otherwise try (re)loading. `manual` forces a retry.
                let already_failed_at_this_mtime = matches!(
                    state.failed_mtimes.get(&id),
                    Some(failed) if *failed == current_mtime
                );
                if manual || !already_failed_at_this_mtime {
                    try_load_plugin(world, &id);
                }
            }
        }
    }
}

/// Hot-reload a single dylib plugin: prepare the new dylib, tear down the old,
/// swap the schedule, leak the old code pages.
#[cfg(not(target_family = "wasm"))]
fn reload_dylib(world: &mut World, plugin_id: &str) {
    let Some((source_path, was_active)) = world
        .resource::<LoadedPlugins>()
        .plugins
        .get(plugin_id)
        .and_then(|p| match &p.backend {
            PluginBackend::Dylib(dylib) => Some((dylib.source_path.clone(), p.active)),
            PluginBackend::Static(_) => None,
        })
    else {
        warn!("[scripts] '{plugin_id}' reload requested but no dylib of it is loaded");
        return;
    };

    if !source_path.exists() {
        warn!(
            "[scripts] '{plugin_id}' source DLL {} disappeared, skipping reload",
            source_path.display()
        );
        return;
    }

    let new_count = next_copy_index(world, plugin_id);
    let new_mtime = file_mtime(&source_path);

    info!("[scripts] '{plugin_id}' reloading (#{new_count})...");

    // Phase 1: ABI-check the new dylib without touching world state.
    let Some((library, plugin_name)) = prepare_library(plugin_id, &source_path, new_count) else {
        error!("[scripts] '{plugin_id}' reload failed, keeping previous version");
        // Record the mtime we failed on. Without this the dylib still looks
        // "changed" next frame, so we would retry — and copy the DLL again —
        // every single frame until the user rebuilt it.
        with_entry(world, plugin_id, |_, entry| {
            if let PluginBackend::Dylib(dylib) = &mut entry.backend {
                dylib.loaded_mtime = new_mtime;
            }
        });
        return;
    };

    // Remove the old dylib's hydrated components while its code (and so its
    // drop glue) is still mapped, and clear its registry entries so the new
    // dylib's `register` re-registers into a clean slot. The bindings
    // themselves survive, re-armed with `NeedsHydration`.
    dehydrate_plugin(world, plugin_id);

    // Tear down OLD before NEW spawns its entities — otherwise the old
    // teardown's plugin-name match would catch the new plugin's entities.
    // This also drops the old schedule while the old library is still mapped,
    // so the systems' drop glue runs safely.
    deactivate_plugin(world, plugin_id);

    // Swap in the new library; leak the old one's pages, since stray vtable
    // pointers may still reference them.
    let new_backend = PluginBackend::Dylib(DylibPlugin {
        library,
        source_path,
        loaded_mtime: new_mtime,
    });
    with_entry(world, plugin_id, |_, entry| {
        std::mem::replace(&mut entry.backend, new_backend).release();
        entry.plugin_name = plugin_name;
        entry.active = false;
    });

    // Phase 2: register the new dylib's scripts.
    let registered = with_entry(world, plugin_id, |world, entry| {
        entry.backend.register(world, plugin_id)
    })
    .unwrap_or(false);
    if !registered {
        error!(
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
    info!("[scripts] '{plugin_id}' reload #{new_count} complete ({script_count} script(s))");
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
/// each frame — that is how a plugin added via the editor's Plugins menu picks up
/// already-spawned entities without a reload.
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
        world.init_resource::<engine_api::PluginFiles>();
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

    /// A statically linked plugin goes through the same lifecycle as a dylib:
    /// register → activate (setup + schedule) → deactivate (teardown sweeps
    /// its `ScriptOwned` entities) → unload (registry cleared).
    #[test]
    fn static_plugin_runs_the_full_lifecycle() {
        #[derive(Resource, Default)]
        struct Ticks(u32);

        #[derive(Default)]
        struct StaticTest;

        impl DynamicPlugin for StaticTest {
            fn register(&self, _world: &mut World, registry: &mut ScriptRegistry) {
                registry.register_script(
                    "static-test",
                    ScriptSchema::new("spinner"),
                    hydrate_spinner,
                );
            }
            fn setup(&self, world: &mut World, schedule: &mut Schedule) {
                world.init_resource::<Ticks>();
                world.spawn(engine_api::ScriptOwned::new("static-test"));
                schedule.add_systems(|mut ticks: ResMut<Ticks>| ticks.0 += 1);
            }
        }

        let mut world = test_world();
        world.init_resource::<LoadedPluginIds>();
        world.init_resource::<bevy::ecs::schedule::Schedules>();
        world.insert_resource(StaticPlugins(vec![("static-test", || {
            Box::new(StaticTest)
        })]));

        load_plugin(&mut world, "static-test");
        assert_eq!(world.resource::<LoadedPluginIds>().0, vec!["static-test"]);
        assert!(
            world
                .resource::<ScriptRegistry>()
                .get("static-test", "spinner")
                .is_some()
        );

        activate_plugin(&mut world, "static-test");
        world.run_schedule(PluginSchedule("static-test".into()));
        assert_eq!(world.resource::<Ticks>().0, 1);

        deactivate_plugin(&mut world, "static-test");
        let owned = world
            .query::<&engine_api::ScriptOwned>()
            .iter(&world)
            .count();
        assert_eq!(
            owned, 0,
            "teardown must sweep the plugin's ScriptOwned entities"
        );
        assert!(
            world
                .try_run_schedule(PluginSchedule("static-test".into()))
                .is_err()
        );

        unload_plugin(&mut world, "static-test");
        assert!(world.resource::<LoadedPluginIds>().0.is_empty());
        assert!(
            world
                .resource::<ScriptRegistry>()
                .get("static-test", "spinner")
                .is_none()
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
