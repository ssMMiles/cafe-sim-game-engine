# Static plugins: one plugin codebase, dylib in dev, compiled-in for web/release

Status: **Steps 1–3 landed 2026-11-13** (host backend split; crates, features, static list; plugin files). Step 4 open.

## Why

Plugins (`plugins/*`) are dylibs loaded by `src/script_host.rs`. That gives fast
iteration — editing a plugin never rebuilds the engine, and Shift+R / an mtime
change hot-reloads it — but WASM has no dylibs, so on the web *nothing* a plugin
does exists: no scripts, no hydration, no level data, no gameplay. That blocks
moving game-specific code (the character controller first) out of the engine
and into the game's plugin.

Goal: the **same plugin source** runs either way:

| Build | How plugins arrive |
|---|---|
| native dev (default `cargo build/run`) | dylibs, hot-reloadable — unchanged |
| web (`tools/web.sh`, `cargo web-check`) | statically linked, always |
| native single binary (`cargo run-static` / `build-static` / `test-static`) | statically linked, bevy too |

## What already makes this cheap

- The host talks to plugins through the `DynamicPlugin` trait
  (`register` / `setup` / `teardown`). The dylib layer is only the
  `extern "C"` shims `engine_api::add!` generates around it.
- Script factories and level hooks are plain `fn` pointers in
  `ScriptRegistry`, so they work identically whether the code is in a dylib or
  the main binary (the `script_host` tests already rely on this).

## Verified facts (throwaway workspace, 2026-11-13)

- A crate with `crate-type = ["dylib", "rlib"]` works as a normal path
  dependency: the dependent links the rlib.
- On `wasm32-unknown-unknown` rustc only warns
  (`dropping unsupported crate type dylib`) and builds — one `Cargo.toml` per
  plugin serves every target.
- Natively, a dependent build still produces the plugin's `.dylib` as a side
  artefact. Harmless, costs a link.
- A dependent may be listed both as an optional `[dependencies]` entry (for a
  feature) and as a plain `[target.'cfg(target_arch = "wasm32")'.dependencies]`
  entry: native gets it only with the feature, wasm always.
- **Not caught by the throwaway test, found in the real build:** with bevy's
  `dynamic_linking` on, rustc links every dependency it can dynamically, so it
  picks a plugin's *dylib* form — which embeds `engine_api`, `rkyv`, … — and
  fails with "cannot satisfy dependencies so `engine_api` only shows up once".
  So a native static build must turn `dynamic_linking` off (it wanted that
  anyway: with it on, the binary still needs `libbevy_dylib` beside it).

## Steps

### 1. Host: plugin backends (done)

`LoadedPlugin` holds a `PluginBackend`:

- `Dylib { library, source_path, loaded_mtime, reload_count }` — native only;
  everything `libloading`, DLL copying, ABI/TypeId checks, mtime polling and
  hot-reload stays behind `cfg(not(wasm))` and only applies to this variant.
- `Static(Box<dyn DynamicPlugin + Send + Sync>)` — calls the trait directly.

The lifecycle logic the FFI shims used to inline (register through
`resource_scope`, teardown + the `ScriptOwned` sweep) moved into
`engine_api::lifecycle`, called by both the shims and the static backend, so
the two paths can't drift. No FFI signature changed → no ABI bump.

The host learns which static plugins exist from the `StaticPlugins` resource
(`id → constructor`). Loading an id prefers a static entry; otherwise (native)
looks for the dylib. Everything that isn't dylib mechanics — hydration,
`detach_binding`, per-plugin schedules, activate/deactivate on F1, plugin level
data save/load — is now compiled on every platform.

Static plugins are never hot-reloaded (Shift+R skips them) and are dropped
normally on unload rather than `mem::forget`-ed.

### 2. Plugin crates + the static list (done)

- Every plugin: `crate-type = ["dylib", "rlib"]`; features
  `default = ["dynamic"]`, `dynamic = ["bevy/dynamic_linking"]`.
- `add!` always emits `PLUGIN_NAME` and
  `pub fn plugin() -> Box<dyn DynamicPlugin + Send + Sync>`; the `#[no_mangle]`
  exports are `#[cfg(feature = "dynamic")]` (the calling crate's feature).
  Required: every plugin exports the same symbol names, so two statically
  linked plugins would be a duplicate-symbol link error.
- **`dynamic_linking` left the workspace bevy feature list.** The engine's
  default feature `dynamic` and each plugin's `dynamic` turn it on, so in every
  existing configuration bevy's feature set — and hash — is exactly what it
  was (checked: dev, `--workspace` and web rebuilt only our crates). Plugins
  carry it themselves so a plugin built on its own still links the bevy
  dylib (`docs/BUILD_TIMES.md` §3 warned about exactly that). Separately,
  `cargo build-plugins` was `--workspace --exclude engine`, which resolved
  bevy with fewer features than the host and so produced dylibs the loader
  rejected; it is plain `--workspace` now.
- Engine `Cargo.toml`: each plugin is an optional `default-features = false`
  dependency behind `static-plugins`, and a plain one in the wasm table — the
  web build (`tools/web.sh`, `cargo web-check`, CI's `wasm-pack`) links them
  with no flag. Never a plain native dependency, or editing a plugin rebuilds
  the engine.
- `.cargo/config.toml`: `run-static` / `build-static` / `test-static` =
  `--no-default-features --features static-plugins --target-dir target/static`.
  Its own dir because bevy's features differ (one cold bevy build).
- `src/static_plugins.rs::all()` lists `(PLUGIN_NAME, plugin)` per crate behind
  `cfg(any(feature = "static-plugins", target_arch = "wasm32"))`; `ScriptHostPlugin`
  inserts it as `StaticPlugins`. `every_plugin_is_wired_for_static_builds`
  (default build) checks the feature, the optional deps and the wasm table all
  name exactly `plugins/*/`; `static_list_covers_every_plugin` (`test-static`)
  checks the list itself.
- `libloading` moved to the native-only dependency table.

**Adding a plugin** = its crate under `plugins/` with the `[lib]`/`[features]`
block the others have, three lines in the engine's `Cargo.toml` (optional dep,
wasm dep, `static-plugins` entry) and one in `static_plugins::all`. The test
names whichever is missing.

### 3. Plugin files (done)

OPFS is async-only, so a plugin can't do a blocking read on the web. Rather
than make plugin save code async, the host mirrors each plugin's folder in
memory: `engine_api::PluginFiles` (resource, ABI 17).

- Folder: `projects/{project}/saves/{plugin}/` — under the working dir
  natively, the same key in OPFS. Keys are plain relative paths
  (`is_valid_key`).
- `src/plugin_files.rs` (host): on plugin load, reads the folder on the
  `IoTaskPool` through the engine's `Storage` and lands it in an inbox
  (`Arc<Mutex>` — the futures aren't `Send` on wasm); `apply_loaded_folders`
  hands it over. Until then `read` says `FileRead::Loading`, after it
  `Missing`/`Present`. A read error still marks the folder loaded, or the
  plugin would wait forever.
- `write` updates the mirror at once and queues the bytes; `flush_writes`
  (`Last`) spawns the writes; `release` flushes and drops the mirror on
  unload — needed because cafe-sim saves on Quit and the unload runs on the
  next state transition. Fire-and-forget: a failed write is only logged.
- cafe-sim: `saves.rs` reads/writes `slot{n}.bin` through it; the main menu
  waits for `is_loaded` as it waits for the shell ("Reading save files...").

### 4. Move the character controller into cafe-sim

Engine keeps the editor fly camera and a minimal "the player exists here" hook;
cafe-sim (or a shared `plugins/character` crate if reuse appears) owns
movement, the gameplay camera and the spawn point (cafe-sim's `spawn-point`
script already does that job).

## Known differences in static builds

- **Panics:** wasm is `panic = "abort"`, so the host's `catch_unwind` around
  plugin calls contains nothing there; a plugin panic kills the tab.
- **`rand` on wasm:** covered by the engine's `getrandom` `wasm_js` pin —
  `cargo web-check` builds cafe-sim cleanly.
- **rustc warns** `dropping unsupported crate type dylib` once per plugin on
  wasm. Expected; crate types can't be per-target.
- **Project manifests still decide** which plugins a project *uses*; the static
  list is only what's *available*. A project naming a plugin that isn't compiled
  in logs the usual "not loaded" warning.
