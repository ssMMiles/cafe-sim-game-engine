use std::{
    fs,
    path::{Path, PathBuf},
};

use bevy::prelude::*;

use super::StorageError;

#[derive(Resource)]
pub struct NativeStorage {
    base_path: PathBuf,
}

impl NativeStorage {
    pub fn new() -> Self {
        Self {
            base_path: PathBuf::from("."),
        }
    }

    fn key_to_path(&self, key: &str) -> PathBuf {
        let mut path = self.base_path.clone();
        for part in key.split('/') {
            path.push(part);
        }
        path
    }

    pub async fn save(&self, key: &str, bytes: &[u8]) -> Result<(), StorageError> {
        let path = self.key_to_path(key);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| StorageError::Io(e.to_string()))?;
        }
        fs::write(&path, bytes).map_err(|e| StorageError::Io(e.to_string()))
    }

    pub async fn load(&self, key: &str) -> Result<Option<Vec<u8>>, StorageError> {
        let path = self.key_to_path(key);
        match fs::read(&path) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(StorageError::Io(e.to_string())),
        }
    }

    pub async fn delete(&self, key: &str) -> Result<(), StorageError> {
        let path = self.key_to_path(key);
        match fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(StorageError::Io(e.to_string())),
        }
    }

    pub async fn list_keys(&self, prefix: &str) -> Result<Vec<String>, StorageError> {
        let dir_path = self.key_to_path(prefix);
        let mut keys = Vec::new();
        collect_keys(&dir_path, &self.base_path, &mut keys)?;
        Ok(keys)
    }
}

fn collect_keys(dir: &Path, base: &Path, out: &mut Vec<String>) -> Result<(), StorageError> {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(StorageError::Io(e.to_string())),
    };
    for entry in entries {
        let entry = entry.map_err(|e| StorageError::Io(e.to_string()))?;
        let path = entry.path();
        if path.is_dir() {
            collect_keys(&path, base, out)?;
        } else {
            if let Ok(rel) = path.strip_prefix(base) {
                // Convert OS path separators to forward slashes for the key
                let key = rel
                    .components()
                    .map(|c| c.as_os_str().to_string_lossy())
                    .collect::<Vec<_>>()
                    .join("/");
                out.push(key);
            }
        }
    }
    Ok(())
}
