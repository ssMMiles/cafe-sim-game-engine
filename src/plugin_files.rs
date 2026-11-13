//! Host half of [`engine_api::PluginFiles`]: reads a plugin's folder when it
//! loads, writes queued files out in the background, flushes before it
//! unloads. Goes through [`Storage`], so OPFS on the web and `std::fs`
//! natively, with the same keys.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use bevy::prelude::*;
use bevy::tasks::IoTaskPool;
use engine_api::PluginFiles;

use crate::projects::ActiveProject;
use crate::storage::Storage;

/// A finished background read: `(plugin, files by key)`.
type LoadedFolder = (String, Result<HashMap<String, Vec<u8>>, String>);

/// Where background reads land, for [`apply_loaded_folders`] to pick up.
/// A shared inbox rather than polled tasks because the storage futures aren't
/// `Send` on wasm.
#[derive(Resource, Default, Clone)]
struct LoadInbox(Arc<Mutex<Vec<LoadedFolder>>>);

pub struct PluginFilesPlugin;

impl Plugin for PluginFilesPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<PluginFiles>()
            .init_resource::<LoadInbox>()
            .add_systems(Update, apply_loaded_folders)
            .add_systems(Last, flush_writes);
    }
}

/// Storage key of `plugin`'s folder in the active project.
fn folder_key(project: &str, plugin: &str) -> String {
    format!("projects/{project}/saves/{plugin}")
}

fn active_project(world: &World) -> Option<String> {
    world
        .get_resource::<ActiveProject>()
        .map(|p| p.0.id.clone())
}

/// Start reading `plugin`'s folder. Called when the plugin loads.
pub fn request_load(world: &mut World, plugin: &str) {
    let Some(project) = active_project(world) else {
        // Nothing to read from; the plugin still gets a working (empty,
        // unsaved) mirror rather than waiting forever.
        warn!("[plugin files] no active project; '{plugin}' starts with no files");
        world
            .resource_mut::<PluginFiles>()
            .finish_loading(plugin, HashMap::new());
        return;
    };
    let storage = world.resource::<Storage>().clone();
    let inbox = world.resource::<LoadInbox>().clone();
    let plugin = plugin.to_owned();
    let prefix = folder_key(&project, &plugin);
    IoTaskPool::get()
        .spawn(async move {
            let result = read_folder(&storage, &prefix).await;
            inbox.0.lock().unwrap().push((plugin, result));
        })
        .detach();
}

async fn read_folder(storage: &Storage, prefix: &str) -> Result<HashMap<String, Vec<u8>>, String> {
    let mut files = HashMap::new();
    let keys = storage.list_keys(prefix).await.map_err(|e| e.to_string())?;
    for key in keys {
        let Some(relative) = key.strip_prefix(prefix).and_then(|k| k.strip_prefix('/')) else {
            continue;
        };
        if let Some(bytes) = storage.load(&key).await.map_err(|e| e.to_string())? {
            files.insert(relative.to_owned(), bytes);
        }
    }
    Ok(files)
}

/// Hand finished reads to [`PluginFiles`]. A read for a plugin that has
/// unloaded since is dropped (its mirror was forgotten).
fn apply_loaded_folders(
    inbox: Res<LoadInbox>,
    mut files: ResMut<PluginFiles>,
    loaded: Res<crate::script_host::LoadedPluginIds>,
) {
    let done: Vec<LoadedFolder> = std::mem::take(&mut *inbox.0.lock().unwrap());
    for (plugin, result) in done {
        if !loaded.0.contains(&plugin) {
            continue;
        }
        let folder = match result {
            Ok(folder) => {
                info!("[plugin files] '{plugin}': {} file(s)", folder.len());
                folder
            }
            Err(e) => {
                // Still mark it loaded: "Loading" forever would lock the
                // plugin out of its own saves. Its writes still go through.
                error!("[plugin files] reading '{plugin}' failed: {e}");
                HashMap::new()
            }
        };
        files.finish_loading(&plugin, folder);
    }
}

fn flush_writes(
    mut files: ResMut<PluginFiles>,
    storage: Res<Storage>,
    project: Option<Res<ActiveProject>>,
) {
    write_pending(
        &mut files,
        &storage,
        project.as_ref().map(|p| p.0.id.as_str()),
    );
}

/// Hand every queued write to the I/O pool.
fn write_pending(files: &mut PluginFiles, storage: &Storage, project: Option<&str>) {
    let pending = files.take_pending();
    if pending.is_empty() {
        return;
    }
    let Some(project) = project else {
        warn!(
            "[plugin files] no active project; {} write(s) dropped",
            pending.len()
        );
        return;
    };
    for (plugin, key, bytes) in pending {
        let storage = storage.clone();
        let path = format!("{}/{key}", folder_key(project, &plugin));
        IoTaskPool::get()
            .spawn(async move {
                if let Err(e) = storage.save(&path, &bytes).await {
                    error!("[plugin files] writing {path} failed: {e}");
                }
            })
            .detach();
    }
}

/// Flush `plugin`'s queued writes and drop its mirror. Called when the plugin
/// unloads — the plugin may have queued a save in its last frame (cafe-sim
/// saves on quit).
pub fn release(world: &mut World, plugin: &str) {
    let project = active_project(world);
    world.resource_scope(|world, mut files: Mut<PluginFiles>| {
        // `Storage` only lacks in headless tests, which have nothing to write.
        if let Some(storage) = world.get_resource::<Storage>() {
            write_pending(&mut files, storage, project.as_deref());
        }
        files.forget(plugin);
    });
}
