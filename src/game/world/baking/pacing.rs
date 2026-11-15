//! Letting the browser breathe during a CPU bake.
//!
//! Natively the CPU bakes (objects, probes) run on `AsyncComputeTaskPool`
//! worker threads and the frame never notices them. On wasm there are no
//! worker threads: the "pool" is `wasm_bindgen_futures::spawn_local`, which
//! polls the task on the main thread's microtask queue — and microtasks run
//! to exhaustion before the browser gets to paint, so a bake that never
//! awaits is a frozen tab for its whole duration (`docs/WEB_PROFILING.md`).
//!
//! [`breathe`] is the one yield point. Awaiting it inside a long loop ends
//! the current task and resumes on a fresh *macrotask* (`setTimeout(0)`),
//! which is what actually lets rendering and input through. Natively it is a
//! no-op, so the bakers call it unconditionally and the band sizes they call
//! it at are tuned for the wasm main thread (tens of milliseconds a band).

/// Yield to the browser's event loop; nothing, natively.
#[cfg(target_arch = "wasm32")]
pub async fn breathe() {
    use wasm_bindgen::prelude::*;
    let promise = js_sys::Promise::new(&mut |resolve, _reject| {
        let Some(window) = web_sys::window() else {
            // No window (a worker?): resolve immediately rather than hang.
            let _ = resolve.call0(&JsValue::NULL);
            return;
        };
        if window
            .set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, 0)
            .is_err()
        {
            let _ = resolve.call0(&JsValue::NULL);
        }
    });
    let _ = wasm_bindgen_futures::JsFuture::from(promise).await;
}

/// Yield to the browser's event loop; nothing, natively.
#[cfg(not(target_arch = "wasm32"))]
pub async fn breathe() {}
