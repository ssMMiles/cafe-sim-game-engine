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

To build for web (deploy artifact — fat LTO, slow):
```
wasm-pack build --target web --release --features opfs
```

To iterate on the web build (thin LTO, `web-dev` profile; needs `cargo install wasm-bindgen-cli` at the version in `Cargo.lock`):
```
cargo build --target wasm32-unknown-unknown --profile web-dev --features opfs
wasm-bindgen --target web --out-dir pkg --out-name game_engine_lib target/wasm32-unknown-unknown/web-dev/game_engine_lib.wasm
```

Benchmarks (the `#[ignore]`d tests) run optimised without LTO: `cargo test --profile perf <name> -- --ignored --nocapture`. Builds use `sccache` and cranelift for the dev profile — see `docs/BUILD_TIMES.md`.

Then serve the repo root with `python3 tools/serve.py [port]` (plain `http.server` with `Cache-Control: no-cache`, so a rebuilt wasm or shader is never paired with a stale cached one) and open `index.html`. The start page shows the worlds stored in the browser and imports world zips (drag one in, *Import zip…*, or `?world=<url>`); `?play` skips the landing card. Export a world from the native editor with File ▸ Export world as zip — see `docs/WORLD_SHARING.md`.