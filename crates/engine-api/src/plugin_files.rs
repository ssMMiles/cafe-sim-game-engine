//! Per-plugin persistent files (save games, settings), readable synchronously
//! on every platform.
//!
//! The browser's file storage (OPFS) is async-only, so a plugin can't block
//! on a read the way it could with `std::fs`. Instead the host keeps an
//! in-memory mirror of each loaded plugin's folder,
//! `projects/{project}/saves/{plugin}/` (OPFS on the web, relative to the
//! working directory natively):
//!
//! - it reads the whole folder in the background when the plugin loads —
//!   until that lands, [`PluginFiles::read`] answers [`FileRead::Loading`];
//! - [`PluginFiles::write`] updates the mirror at once and queues the bytes;
//!   the host writes them out in the background (and flushes before the
//!   plugin unloads). Fire-and-forget: a failed write is logged by the host.
//!
//! Meant for small files — everything is held in memory while the plugin is
//! loaded.

use std::collections::HashMap;

use bevy::prelude::*;

/// What [`PluginFiles::read`] knows about a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileRead<'a> {
    /// The plugin's folder is still being read; the file may exist.
    Loading,
    /// The folder has been read and has no such file.
    Missing,
    Present(&'a [u8]),
}

#[derive(Default, Debug)]
struct PluginFolder {
    /// Whether the host has finished reading the folder.
    loaded: bool,
    files: HashMap<String, Vec<u8>>,
    /// Writes not yet handed to the host, oldest first.
    pending: Vec<(String, Vec<u8>)>,
}

/// The in-memory mirror of every loaded plugin's files. Plugins address
/// their own folder with their `PLUGIN_NAME` and a relative key such as
/// `"slot1.bin"` (`/` separates sub-folders; no `..`).
#[derive(Resource, Default, Debug)]
pub struct PluginFiles {
    folders: HashMap<String, PluginFolder>,
}

impl PluginFiles {
    /// Whether `plugin`'s folder has been read, so `Missing` means missing.
    pub fn is_loaded(&self, plugin: &str) -> bool {
        self.folders.get(plugin).is_some_and(|f| f.loaded)
    }

    pub fn read(&self, plugin: &str, key: &str) -> FileRead<'_> {
        let Some(folder) = self.folders.get(plugin) else {
            return FileRead::Loading;
        };
        match folder.files.get(key) {
            Some(bytes) => FileRead::Present(bytes),
            None if folder.loaded => FileRead::Missing,
            None => FileRead::Loading,
        }
    }

    /// Replace a file's contents. Visible to [`read`](Self::read) at once;
    /// persisted by the host in the background.
    ///
    /// # Panics
    /// On a key that isn't a plain relative path (empty segment, `.`, `..`).
    pub fn write(&mut self, plugin: &str, key: &str, bytes: Vec<u8>) {
        assert!(is_valid_key(key), "invalid plugin file key {key:?}");
        let folder = self.folders.entry(plugin.to_owned()).or_default();
        folder.files.insert(key.to_owned(), bytes.clone());
        folder.pending.push((key.to_owned(), bytes));
    }

    // ---- host side ----------------------------------------------------------

    /// Host: the folder has been read. Keys written meanwhile keep their new
    /// contents.
    pub fn finish_loading(&mut self, plugin: &str, files: HashMap<String, Vec<u8>>) {
        let folder = self.folders.entry(plugin.to_owned()).or_default();
        for (key, bytes) in files {
            folder.files.entry(key).or_insert(bytes);
        }
        folder.loaded = true;
    }

    /// Host: take every queued write as `(plugin, key, bytes)`, oldest first
    /// within each plugin.
    pub fn take_pending(&mut self) -> Vec<(String, String, Vec<u8>)> {
        self.folders
            .iter_mut()
            .flat_map(|(plugin, folder)| {
                folder
                    .pending
                    .drain(..)
                    .map(move |(key, bytes)| (plugin.clone(), key, bytes))
            })
            .collect()
    }

    /// Host: drop `plugin`'s mirror (it unloaded). Take its pending writes
    /// first, or they are lost.
    pub fn forget(&mut self, plugin: &str) {
        self.folders.remove(plugin);
    }
}

/// A relative path of plain segments: what the host may join onto the
/// plugin's folder on any backend.
pub fn is_valid_key(key: &str) -> bool {
    !key.is_empty()
        && key
            .split('/')
            .all(|seg| !seg.is_empty() && seg != "." && seg != ".." && !seg.contains('\\'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_are_loading_until_the_folder_lands() {
        let mut files = PluginFiles::default();
        assert_eq!(files.read("p", "a"), FileRead::Loading);
        files.finish_loading("p", HashMap::from([("a".into(), vec![1])]));
        assert_eq!(files.read("p", "a"), FileRead::Present(&[1]));
        assert_eq!(files.read("p", "b"), FileRead::Missing);
    }

    #[test]
    fn a_write_before_the_load_wins_over_the_stored_copy() {
        let mut files = PluginFiles::default();
        files.write("p", "a", vec![2]);
        files.finish_loading("p", HashMap::from([("a".into(), vec![1])]));
        assert_eq!(files.read("p", "a"), FileRead::Present(&[2]));
        assert_eq!(
            files.take_pending(),
            vec![("p".into(), "a".into(), vec![2])]
        );
        assert!(files.take_pending().is_empty());
    }

    #[test]
    fn keys_must_stay_inside_the_folder() {
        assert!(is_valid_key("slot1.bin"));
        assert!(is_valid_key("sub/slot1.bin"));
        for bad in ["", "/abs", "a//b", "../x", "a/./b", "a\\b", "dir/"] {
            assert!(!is_valid_key(bad), "{bad:?}");
        }
    }
}
