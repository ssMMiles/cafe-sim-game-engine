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
