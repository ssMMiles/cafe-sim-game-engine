# Cafe Sim Game Engine

An engine for my cafe simulator game written in Rust on top of the Bevy framework. Early stages, but supports dynamically loadable gameplay modules and showcases the beginnings of a level editor. Designed to be easily built and deployed for WASM, we use minimal dependencies and aim for performance to be as high as possible to allow running on low power mobile devices.

### Build

To run:

```bash
cargo run
```

If updating a plugin, rebuild the workspace:

```
cargo build --workspace
```

To build for web (into `pkg/`; needs `cargo install wasm-bindgen-cli` at the version in `Cargo.lock`):
```
tools/web.sh            # iterate: web-dev profile, thin LTO
tools/web.sh profile    # with bevy spans, for tools/web-trace.mjs
tools/web.sh release    # deploy artifact: fat LTO + wasm-opt -Oz, function names stripped
```

Benchmarks (the `#[ignore]`d tests): `cargo perf <name> -- --ignored --nocapture`.

Every build goes through one of the fixed configurations listed at the top of `.cargo/config.toml`, each with its own target directory, so switching between them never throws away another's incremental state. Don't pass `--features`/`--profile`/`--target` by hand — see `docs/BUILD_TIMES.md`.

Then serve the repo root with `python3 tools/serve.py [port]` (plain `http.server` with `Cache-Control: no-cache`, so a rebuilt wasm or shader is never paired with a stale cached one) and open `index.html`. The start page shows the worlds stored in the browser and imports world zips (drag one in, *Import zip…*, or `?world=<url>`); `?play` skips the landing card. Export a world from the native editor with File ▸ Export world as zip — see `docs/WORLD_SHARING.md`.