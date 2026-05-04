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

#[cfg(not(target_family = "wasm"))]
use engine_api::AppState;
#[cfg(not(target_family = "wasm"))]
use crate::projects::ActiveProject;

// ---------------------------------------------------------------------------
// Schedule
// ---------------------------------------------------------------------------

/// One schedule per loaded plugin, keyed by plugin id (the source directory
/// name under `plugins/`). Replaced wholesale on hot-reload.
#[derive(ScheduleLabel, Debug, Clone, PartialEq, Eq, Hash)]
pub struct PluginSchedule(pub String);

// ---------------------------------------------------------------------------
// Plugin
// ---------------------------------------------------------------------------

pub struct ScriptHostPlugin;

impl Plugin for ScriptHostPlugin {
    fn build(&self, app: &mut App) {
        #[cfg(not(target_family = "wasm"))]
        {
            app.insert_resource(LoadedPlugins::default());
            app.add_systems(OnEnter(AppState::InGame), load_active_project_plugins);
            app.add_systems(OnExit(AppState::InGame), unload_all_plugins);
            app.add_systems(Update, run_scripts.run_if(in_state(AppState::InGame)));
        }
        #[cfg(target_family = "wasm")]
        {
            let _ = app;
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
}

#[cfg(not(target_family = "wasm"))]
#[derive(Resource, Default)]
struct LoadedPlugins {
    plugins: HashMap<String, LoadedPlugin>,
    /// mtime of last *failed* load attempt per plugin id. Used to skip
    /// retrying a busted DLL until its mtime changes.
    failed_mtimes: HashMap<String, Option<SystemTime>>,
}

/// Returns `<exe-dir>/<id>.dll`, or `None` if the exe path can't be resolved.
#[cfg(not(target_family = "wasm"))]
fn plugin_dll_path(id: &str) -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    Some(exe.parent()?.join(format!("{id}.dll")))
}

#[cfg(not(target_family = "wasm"))]
fn file_mtime(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).ok()?.modified().ok()
}

/// Copy the DLL to a numbered variant so the original stays unlocked for
/// `cargo build`. Returns the path to the copy.
#[cfg(not(target_family = "wasm"))]
fn copy_dll(source: &Path, n: u32) -> std::io::Result<PathBuf> {
    let stem = source
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy();
    let ext = source
        .extension()
        .unwrap_or_default()
        .to_string_lossy();
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
/// No world state has been touched yet — call [`run_setup`] to commit.
#[cfg(not(target_family = "wasm"))]
struct PreparedLibrary {
    library: libloading::Library,
    plugin_name: String,
    setup_fn: PluginSetupFn,
}

/// A dylib whose `plugin_setup` has run successfully.
#[cfg(not(target_family = "wasm"))]
struct LoadedDylib {
    library: libloading::Library,
    plugin_name: String,
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
                bevy::log::error!(
                    "[scripts] '{plugin_id}' missing plugin_abi_version symbol: {e}"
                );
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
                std::ffi::CStr::from_ptr(ptr)
                    .to_string_lossy()
                    .into_owned()
            }
            Err(e) => {
                bevy::log::error!("[scripts] '{plugin_id}' missing plugin_name symbol: {e}");
                return None;
            }
        }
    };

    let setup_fn: PluginSetupFn = unsafe {
        match library.get::<PluginSetupFn>(b"plugin_setup") {
            Ok(sym) => *sym,
            Err(e) => {
                bevy::log::error!("[scripts] '{plugin_id}' missing plugin_setup symbol: {e}");
                return None;
            }
        }
    };

    if let Err(e) = unsafe { library.get::<PluginTeardownFn>(b"plugin_teardown") } {
        bevy::log::error!("[scripts] '{plugin_id}' missing plugin_teardown symbol: {e}");
        return None;
    }

    Some(PreparedLibrary {
        library,
        plugin_name,
        setup_fn,
    })
}

/// Phase 2 of loading: run the plugin's `setup` and install its schedule.
/// Touches world state — only call this once you're committed to swapping
/// in the new dylib (i.e. after the old one's teardown has run).
#[cfg(not(target_family = "wasm"))]
fn run_setup(
    world: &mut World,
    plugin_id: &str,
    prepared: PreparedLibrary,
) -> Option<LoadedDylib> {
    // Make sure Time is registered host-side before the dylib touches it.
    let _ = world.register_resource::<bevy::time::Time>();

    let mut new_schedule = Schedule::new(PluginSchedule(plugin_id.to_owned()));
    let schedule_ptr = &mut new_schedule as *mut bevy::ecs::schedule::Schedule as *mut c_void;
    let world_ptr = world as *mut World as *mut c_void;

    let result = std::panic::catch_unwind(AssertUnwindSafe(|| unsafe {
        (prepared.setup_fn)(world_ptr, schedule_ptr)
    }));

    match result {
        Ok(0) => {}
        Ok(2) => {
            bevy::log::error!(
                "[scripts] '{plugin_id}' TypeId mismatch — the DLL was compiled \
                 against different bevy metadata. Rebuild the full workspace \
                 (cargo build --workspace) and restart."
            );
            return None;
        }
        Ok(code) => {
            bevy::log::error!("[scripts] '{plugin_id}' plugin_setup returned error code {code}");
            return None;
        }
        Err(_) => {
            bevy::log::error!("[scripts] '{plugin_id}' plugin_setup panicked");
            return None;
        }
    }

    // Replaces any prior schedule with the same label. The dropped schedule's
    // systems hold function pointers into the old DLL, but those pages are
    // either still mapped (reload case: caller mem::forgets the old library)
    // or this is the first load for this id.
    let mut schedules = world.resource_mut::<bevy::ecs::schedule::Schedules>();
    schedules.insert(new_schedule);

    bevy::log::info!(
        "[scripts] '{plugin_id}' set up successfully (reports name: '{}')",
        prepared.plugin_name
    );
    Some(LoadedDylib {
        library: prepared.library,
        plugin_name: prepared.plugin_name,
    })
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
    let result = std::panic::catch_unwind(AssertUnwindSafe(|| unsafe {
        teardown_fn(world_ptr)
    }));

    match result {
        Ok(0) => bevy::log::info!("[scripts] '{plugin_id}' torn down"),
        Ok(code) => bevy::log::error!(
            "[scripts] '{plugin_id}' plugin_teardown returned error code {code}"
        ),
        Err(_) => bevy::log::error!(
            "[scripts] '{plugin_id}' plugin_teardown panicked"
        ),
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
        bevy::log::info!(
            "[scripts] project '{}' declares no plugins",
            active.0.id
        );
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
    bevy::log::info!("[scripts] unloading {} plugin(s) on exit to menu", ids.len());

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
    let Some(loaded) = world.resource_mut::<LoadedPlugins>().plugins.remove(id) else {
        return;
    };

    run_teardown(world, &loaded.library, id);

    if let Some(mut schedules) = world.get_resource_mut::<bevy::ecs::schedule::Schedules>() {
        schedules.remove(PluginSchedule(id.to_owned()));
    }

    std::mem::forget(loaded.library);
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

    let Some(prepared) = prepare_library(plugin_id, &source_path, 0) else {
        let mut state = world.resource_mut::<LoadedPlugins>();
        state.failed_mtimes.insert(plugin_id.to_owned(), mtime);
        return;
    };
    let Some(loaded) = run_setup(world, plugin_id, prepared) else {
        let mut state = world.resource_mut::<LoadedPlugins>();
        state.failed_mtimes.insert(plugin_id.to_owned(), mtime);
        return;
    };

    let mut state = world.resource_mut::<LoadedPlugins>();
    state.failed_mtimes.remove(plugin_id);
    state.plugins.insert(
        plugin_id.to_owned(),
        LoadedPlugin {
            library: loaded.library,
            plugin_name: loaded.plugin_name,
            source_path,
            loaded_mtime: mtime,
            reload_count: 0,
        },
    );
}

/// Exclusive `Update` system: poll for reload triggers, then run every
/// loaded plugin's schedule.
#[cfg(not(target_family = "wasm"))]
fn run_scripts(world: &mut World) {
    let manual_reload = {
        let input = world.resource::<ButtonInput<KeyCode>>();
        let shift = input.pressed(KeyCode::ShiftLeft) || input.pressed(KeyCode::ShiftRight);
        shift && input.just_pressed(KeyCode::KeyR)
    };

    sync_plugins(world, manual_reload);

    let labels: Vec<PluginSchedule> = world
        .resource::<LoadedPlugins>()
        .plugins
        .keys()
        .map(|id| PluginSchedule(id.clone()))
        .collect();
    for label in labels {
        let _ = world.try_run_schedule(label);
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
            let loaded_mtime = state
                .plugins
                .get(&id)
                .and_then(|p| p.loaded_mtime);
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
            bevy::log::warn!(
                "[scripts] '{plugin_id}' reload requested but not currently loaded"
            );
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

    let new_count = prev.reload_count + 1;
    let new_mtime = file_mtime(&prev.source_path);

    bevy::log::info!("[scripts] '{plugin_id}' reloading (#{new_count})...");

    // Phase 1: ABI-check the new dylib without touching world state.
    let Some(prepared) = prepare_library(plugin_id, &prev.source_path, new_count) else {
        bevy::log::error!(
            "[scripts] '{plugin_id}' reload failed, keeping previous version"
        );
        world
            .resource_mut::<LoadedPlugins>()
            .plugins
            .insert(plugin_id.to_owned(), prev);
        return;
    };

    // Tear down OLD before NEW spawns its entities — otherwise the old
    // teardown's plugin-name match would catch the new plugin's entities.
    run_teardown(world, &prev.library, plugin_id);

    // Phase 2: setup the new dylib.
    let Some(new_dylib) = run_setup(world, plugin_id, prepared) else {
        bevy::log::error!(
            "[scripts] '{plugin_id}' new plugin_setup failed after old teardown — \
             plugin state is empty until next successful reload"
        );
        // Keep the old library mapped so a subsequent reload has something
        // valid to operate on (its teardown just ran, so it's a no-op).
        world
            .resource_mut::<LoadedPlugins>()
            .plugins
            .insert(plugin_id.to_owned(), prev);
        return;
    };

    // Leak old code pages. The old schedule was dropped inside run_setup
    // (Schedules::insert returns and drops the prior entry) while the old
    // library was still mapped, so its system drop glue ran safely. Keep
    // pages mapped permanently for any other stray vtable pointers.
    let old_library = std::mem::replace(&mut prev.library, new_dylib.library);
    std::mem::forget(old_library);

    prev.plugin_name = new_dylib.plugin_name;
    prev.loaded_mtime = new_mtime;
    prev.reload_count = new_count;

    world
        .resource_mut::<LoadedPlugins>()
        .plugins
        .insert(plugin_id.to_owned(), prev);

    bevy::log::info!("[scripts] '{plugin_id}' reload #{new_count} complete");
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
