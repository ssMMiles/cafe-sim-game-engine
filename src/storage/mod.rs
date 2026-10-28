use std::fmt;

use bevy::prelude::*;

// ============================================================================
// Error type
// ============================================================================

// Only the wasm backend has live call sites today (the native save/load paths
// go through `terrain::storage` directly), so on a native build this and
// `NativeStorage` look unused. They are the other half of the platform
// abstraction `Storage` documents — keep them in step with `wasm.rs`.
#[allow(dead_code)]
#[derive(Debug)]
pub enum StorageError {
    /// Native I/O error (std::fs).
    Io(String),
    /// OPFS/WebAPI error (wasm32).
    Opfs(String),
}

impl fmt::Display for StorageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StorageError::Io(msg) => write!(f, "storage I/O error: {msg}"),
            StorageError::Opfs(msg) => write!(f, "OPFS error: {msg}"),
        }
    }
}

// ============================================================================
// Backend modules + type alias
// ============================================================================

#[cfg(not(target_arch = "wasm32"))]
mod native;
#[cfg(target_arch = "wasm32")]
pub mod wasm;

/// Platform-specific storage backend. Use this type alias throughout the game
/// so no call site needs `#[cfg]`.
///
/// Both backends expose identical `async fn save/load/delete/list_keys` methods.
#[cfg(not(target_arch = "wasm32"))]
#[allow(unused_imports)]
pub use native::NativeStorage as Storage;
#[cfg(target_arch = "wasm32")]
#[allow(unused_imports)]
pub use wasm::WasmStorage as Storage;

// ============================================================================
// Plugin
// ============================================================================

pub struct StoragePlugin;

impl Plugin for StoragePlugin {
    fn build(&self, app: &mut App) {
        #[cfg(not(target_arch = "wasm32"))]
        app.insert_resource(native::NativeStorage::new());

        // WasmStorage is a unit struct (trivially Send + Sync) — insert immediately.
        // Individual OPFS calls will surface errors at use time if OPFS is unavailable.
        #[cfg(target_arch = "wasm32")]
        app.insert_resource(wasm::WasmStorage);
    }
}
