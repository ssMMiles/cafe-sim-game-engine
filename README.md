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

To build for web:
```
wasm-pack build --target web --release --features opfs
```

Then serve the repo root with `python3 tools/serve.py [port]` (plain `http.server` with `Cache-Control: no-cache`, so a rebuilt wasm or shader is never paired with a stale cached one) and open `index.html`. The start page shows the worlds stored in the browser and imports world zips (drag one in, *Import zip…*, or `?world=<url>`); `?play` skips the landing card. Export a world from the native editor with File ▸ Export world as zip — see `docs/WORLD_SHARING.md`.