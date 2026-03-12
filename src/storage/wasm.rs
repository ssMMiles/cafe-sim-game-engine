use bevy::prelude::*;
use js_sys::{Object, Uint8Array};
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;
use web_sys::{
    File, FileSystemDirectoryHandle, FileSystemFileHandle, FileSystemGetDirectoryOptions,
    FileSystemGetFileOptions, FileSystemWritableFileStream,
};

use super::StorageError;

/// OPFS-backed storage for WASM. Unit struct — trivially Send + Sync.
#[derive(Resource)]
pub struct WasmStorage;

impl WasmStorage {
    /// Verifies OPFS is accessible. Called once during plugin init.
    pub async fn init() -> Result<Self, StorageError> {
        get_root().await?;
        Ok(Self)
    }

    pub async fn save(&self, key: &str, bytes: &[u8]) -> Result<(), StorageError> {
        let (dir_parts, filename) = split_key(key);
        let root = get_root().await?;
        let dir = navigate_to(root, &dir_parts, true).await?;

        let mut opts = FileSystemGetFileOptions::new();
        opts.create(true);
        let file_handle: FileSystemFileHandle =
            JsFuture::from(dir.get_file_handle_with_options(filename, &opts))
                .await
                .map_err(js_err)?
                .dyn_into()
                .map_err(js_err)?;

        let writable: FileSystemWritableFileStream =
            JsFuture::from(file_handle.create_writable())
                .await
                .map_err(js_err)?
                .dyn_into()
                .map_err(js_err)?;

        let array = Uint8Array::from(bytes);
        let chunk = array.unchecked_ref::<Object>();
        let write_promise = writable.write_with_buffer_source(chunk).map_err(js_err)?;
        JsFuture::from(write_promise).await.map_err(js_err)?;

        JsFuture::from(writable.close()).await.map_err(js_err)?;
        Ok(())
    }

    pub async fn load(&self, key: &str) -> Result<Option<Vec<u8>>, StorageError> {
        let (dir_parts, filename) = split_key(key);
        let root = get_root().await?;

        let dir = match navigate_to(root, &dir_parts, false).await {
            Ok(d) => d,
            Err(StorageError::Opfs(ref msg)) if is_not_found_msg(msg) => return Ok(None),
            Err(e) => return Err(e),
        };

        let file_handle: FileSystemFileHandle =
            match JsFuture::from(dir.get_file_handle(filename)).await {
                Ok(h) => h.dyn_into().map_err(js_err)?,
                Err(e) if is_not_found_js(&e) => return Ok(None),
                Err(e) => return Err(js_err(e)),
            };

        let file: File = JsFuture::from(file_handle.get_file())
            .await
            .map_err(js_err)?
            .dyn_into()
            .map_err(js_err)?;

        let array_buffer = JsFuture::from(file.array_buffer())
            .await
            .map_err(js_err)?;

        Ok(Some(Uint8Array::new(&array_buffer).to_vec()))
    }

    pub async fn delete(&self, key: &str) -> Result<(), StorageError> {
        let (dir_parts, filename) = split_key(key);
        let root = get_root().await?;

        let dir = match navigate_to(root, &dir_parts, false).await {
            Ok(d) => d,
            Err(StorageError::Opfs(ref msg)) if is_not_found_msg(msg) => return Ok(()),
            Err(e) => return Err(e),
        };

        match JsFuture::from(dir.remove_entry(filename)).await {
            Ok(_) => Ok(()),
            Err(e) if is_not_found_js(&e) => Ok(()),
            Err(e) => Err(js_err(e)),
        }
    }

    pub async fn list_keys(&self, prefix: &str) -> Result<Vec<String>, StorageError> {
        let parts: Vec<&str> = prefix.split('/').filter(|s| !s.is_empty()).collect();
        let root = get_root().await?;

        let dir = match navigate_to(root, &parts, false).await {
            Ok(d) => d,
            Err(StorageError::Opfs(ref msg)) if is_not_found_msg(msg) => return Ok(vec![]),
            Err(e) => return Err(e),
        };

        let mut keys = Vec::new();
        collect_keys_recursive(&dir, prefix, &mut keys).await?;
        Ok(keys)
    }
}

// ============================================================================
// Helpers
// ============================================================================

async fn get_root() -> Result<FileSystemDirectoryHandle, StorageError> {
    let window = web_sys::window().ok_or_else(|| StorageError::Opfs("no window".into()))?;
    let handle: FileSystemDirectoryHandle =
        JsFuture::from(window.navigator().storage().get_directory())
            .await
            .map_err(js_err)?
            .dyn_into()
            .map_err(js_err)?;
    Ok(handle)
}

async fn navigate_to(
    root: FileSystemDirectoryHandle,
    parts: &[&str],
    create: bool,
) -> Result<FileSystemDirectoryHandle, StorageError> {
    let mut current = root;
    for part in parts {
        let mut opts = FileSystemGetDirectoryOptions::new();
        opts.create(create);
        current =
            JsFuture::from(current.get_directory_handle_with_options(part, &opts))
                .await
                .map_err(js_err)?
                .dyn_into()
                .map_err(js_err)?;
    }
    Ok(current)
}

/// Recurse through `dir`, collecting file paths relative to `prefix`.
async fn collect_keys_recursive(
    dir: &FileSystemDirectoryHandle,
    prefix: &str,
    out: &mut Vec<String>,
) -> Result<(), StorageError> {
    // `entries()` returns an AsyncIterableIterator<[string, FileSystemHandle]>.
    // We use JS reflection to drive the async iteration protocol.
    let entries_fn: js_sys::Function =
        js_sys::Reflect::get(dir.as_ref(), &JsValue::from_str("entries"))
            .map_err(reflect_err)?
            .dyn_into()
            .map_err(js_err)?;

    let iterator: Object = entries_fn
        .call0(dir.as_ref())
        .map_err(js_err)?
        .dyn_into()
        .map_err(js_err)?;

    let next_fn: js_sys::Function =
        js_sys::Reflect::get(&iterator, &JsValue::from_str("next"))
            .map_err(reflect_err)?
            .dyn_into()
            .map_err(js_err)?;

    loop {
        let result =
            JsFuture::from(js_sys::Promise::from(next_fn.call0(&iterator).map_err(js_err)?))
                .await
                .map_err(js_err)?;

        let done = js_sys::Reflect::get(&result, &JsValue::from_str("done"))
            .map_err(reflect_err)?
            .as_bool()
            .unwrap_or(true);

        if done {
            break;
        }

        let value =
            js_sys::Reflect::get(&result, &JsValue::from_str("value")).map_err(reflect_err)?;
        let pair: js_sys::Array = value.dyn_into().map_err(js_err)?;

        let name = pair.get(0).as_string().unwrap_or_default();
        let handle = pair.get(1);

        let child_path = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}/{name}")
        };

        let kind = js_sys::Reflect::get(&handle, &JsValue::from_str("kind"))
            .map_err(reflect_err)?;
        if kind == JsValue::from_str("file") {
            out.push(child_path);
        } else {
            let child_dir: FileSystemDirectoryHandle = handle.dyn_into().map_err(js_err)?;
            Box::pin(collect_keys_recursive(&child_dir, &child_path, out)).await?;
        }
    }
    Ok(())
}

/// `"a/b/c/file.ext"` → `(["a", "b", "c"], "file.ext")`
fn split_key(key: &str) -> (Vec<&str>, &str) {
    let mut parts: Vec<&str> = key.split('/').collect();
    let filename = parts.pop().unwrap_or("");
    (parts, filename)
}

fn js_err(e: JsValue) -> StorageError {
    let msg = js_sys::Reflect::get(&e, &JsValue::from_str("message"))
        .ok()
        .and_then(|m| m.as_string())
        .unwrap_or_else(|| format!("{e:?}"));
    StorageError::Opfs(msg)
}

fn reflect_err(e: JsValue) -> StorageError {
    StorageError::Opfs(format!("reflect error: {e:?}"))
}

fn is_not_found_msg(msg: &str) -> bool {
    msg.contains("NotFound") || msg.contains("not found") || msg.contains("No such")
}

fn is_not_found_js(e: &JsValue) -> bool {
    js_sys::Reflect::get(e, &JsValue::from_str("name"))
        .ok()
        .and_then(|n| n.as_string())
        .map(|n| n == "NotFoundError")
        .unwrap_or(false)
}
