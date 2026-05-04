# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

# Key Information

NEVER guess or make assumptions about the codebase or external dependencies. Always ask yourself if you are 100% confident in something, and if not ask for clarification or investigate further before proceeding.

## Development Commands

- **Build**: `cargo build`
- **Run**: `cargo run`
- **Check**: `cargo check`
- **Format**: `cargo fmt`
- **Lint**: `cargo clippy`

## Development Notes

- Whenever you make a change that affects the project structure or just something that you find might be important to remember, please write it to your memory file yourself.
- Always take your time to write clean, well designed and organised code. Do not repeat yourself, but also don't break things up to a ridiculous extent before it is needed.
- You have full access to the bevy repository (in ../bevy) if you need to check anything about bevy itself.
- Instead of running the program as it's a visual app you can't interact with, just ask me. You can however run tests and check if things compile.

---

# Architecture Overview

**engine** is a 3D terrain editor and exploration game built with Bevy 0.18 and Rust. It supports both native (Windows/Linux/Mac) and WASM (WebGPU) platforms.

## Tech Stack

- **Bevy 0.18** - Game engine (ECS)
- **avian3d 0.5** - Physics (kinematic character controller, colliders)
- **rkyv 0.8** - Zero-copy serialization for terrain/object persistence
- **wasm-bindgen / web-sys** - WASM platform support (OPFS storage)

## App State Machine

```
AppState::Starting → AppState::MainMenu → AppState::InGame
                                              ↓
                              SimulationState::Running ↔ SimulationState::Paused
```

System sets enforce execution order: `SimSet::InGame` > `SimSet::Running` / `SimSet::Paused`.

## Module Structure

```
src/
├── lib.rs              # App init, plugin registration, custom "world" asset source
├── main.rs             # Binary entry point
├── settings.rs         # UserSettings resource, SliderSetting trait
├── storage/            # Platform-agnostic async file I/O (native + WASM)
├── ui/                 # UiTheme, UiAnchors, UiExt widget trait
│   ├── theme.rs        # Colors, typography, spacing constants
│   ├── anchors.rs      # Persistent screen-edge anchor entities
│   └── widgets.rs      # ui_title, ui_button, ui_slider, ui_divider
├── menu/               # Main menu screen + navigation
│   ├── mod.rs          # MenuPlugin, Camera2d setup
│   └── main.rs         # Start Game / Settings buttons
└── game/               # Core simulation (SimulationPlugin)
    ├── mod.rs           # Plugin orchestration, system set config
    ├── simulation.rs    # Player spawn, world setup, cleanup
    ├── physics.rs       # CharacterState, kinematic controller (avian3d)
    ├── animation.rs     # AnimationGraph, state machine (Idle/Run/Jump)
    ├── gltf_loading.rs  # Scene loading, toon material conversion, interactions
    ├── ui_displays.rs   # HUD panels (FPS, keybinds, position, interaction)
    ├── pause_menu.rs    # Pause overlay, settings submenu, generic sliders
    ├── gfx/             # Rendering pipeline
    │   ├── camera.rs    # PlayerCamera, render-to-texture (1920x1080 HDR)
    │   ├── toon_material.rs  # ToonMaterial (StandardMaterial extension + lightmap)
    │   ├── post_process.rs   # Edge detection post-processing
    │   ├── crosshair.rs
    │   └── physics_debug.rs  # Gizmo toggle (Shift+P)
    └── world/           # WorldPlugin - terrain, objects, baking, streaming
        ├── mod.rs       # WorldPlugin, sun/ambient light setup
        ├── sky.rs       # SkyDomeMaterial, 4-band toon gradient
        ├── streaming.rs # Chunk load/unload by player proximity
        ├── chunk_assets.rs  # Custom asset loaders (.terrain, .objects, world.meta)
        ├── objects.rs   # PlacedObjectRegistry, spawn/despawn, persistence
        ├── edit_mode.rs # Placement preview, object placement (LMB)
        ├── mesh_selector.rs # Mesh catalog panel, scroll-to-select
        ├── terrain/
        │   ├── types.rs      # TerrainChunk, HeightmapData (sparse HashMap)
        │   ├── constants.rs  # CHUNK_SIZE=65, TILE_SIZE=1.0
        │   ├── storage.rs    # rkyv WorldMetadata/ChunkData, quantized heights
        │   ├── bootstrap.rs  # Two-stage async terrain init
        │   ├── mesh.rs       # LOD mesh generation (4 levels), skirts
        │   ├── material.rs   # TerrainMaterial (stochastic no-tile sampling)
        │   ├── lod.rs        # LOD selection by camera distance
        │   ├── edit.rs       # Terrain brush (raise/lower, Shift+G toggle)
        │   ├── save.rs       # Dirty chunk persistence (Shift+S)
        │   └── reconcile.rs  # Mesh-to-storage consistency
        └── baking/
            ├── mod.rs        # BakeConfig, BakingPlugin (Shift+B trigger)
            ├── raycast.rs    # CPU shadow/AO raycasting (DDA)
            ├── terrain.rs    # Terrain lightmap baking
            ├── objects.rs    # Per-object lightmap baking
            ├── gpu.rs        # GPU compute pipeline (native only)
            └── uv_unwrap.rs  # xatlas UV2 generation
```

## Shaders (`assets/shaders/`)

| Shader | Purpose |
|--------|---------|
| `sky_dome.wgsl` | 4-band toon sky gradient |
| `toon_lighting.wgsl` | Object fragment shader (lightmap + toon bands) |
| `terrain_toon.wgsl` | Terrain fragment (stochastic texture + toon lighting) |
| `lightmap_bake.wgsl` | GPU compute lightmap generation |
| `edge_detection_post.wgsl` | Post-process edge detection |

## Key Systems & Keybinds

| Key | Action |
|-----|--------|
| WASD | Movement |
| Space | Jump |
| Escape | Pause menu |
| Shift+E | Toggle edit mode |
| Tab | Toggle mesh selector (edit mode) |
| LMB | Place object (edit mode) / Raise terrain (terrain edit) |
| RMB | Lower terrain (terrain edit) |
| Scroll | Cycle mesh selection (edit mode) |
| Shift+Scroll | Adjust placement scale |
| Shift+G | Toggle terrain edit mode |
| `[` / `]` | Adjust brush size |
| Shift+S | Save terrain |
| Shift+B | Bake lightmaps |
| Shift+P | Toggle physics debug gizmos |

## Terrain Streaming Pipeline

1. `update_streaming_ring` - Determines which chunks are in range (Chebyshev distance, load_radius=4, unload_radius=6)
2. `launch_chunk_tasks` - When asset loads complete, spawns AsyncComputeTaskPool mesh generation
3. `finalize_computed_chunks` - Polls tasks, spawns chunk entities + objects (max 2/frame)

Chunk data uses rkyv zero-copy serialization. Heights are quantized u16 (0-65535) with smart storage modes: UseBase (unmodified) > Delta (sparse edits) > Full (heavy mods).

## Rendering Pipeline

- Camera renders to 1920x1080 float16 HDR image, displayed via sprite on layer 1
- Materials: `ExtendedMaterial<StandardMaterial, ToonMaterial>` for objects, `ExtendedMaterial<StandardMaterial, TerrainMaterial>` for terrain
- Material bind group 3; use `@group(#{MATERIAL_BIND_GROUP})` in WGSL (naga_oil substitution)
- Lightmaps baked via GPU compute (native) or CPU raycast (WASM), stored as PNG
- Volumetric fog + distance fog for atmosphere; sky dome follows camera

## Physics (avian3d 0.5)

- Player: `RigidBody::Kinematic` + capsule collider (radius=0.3, cylinder_height from CHAR_HEIGHT=1.5)
- Ground detection: heightmap sampling + shape cast downward
- Wall depenetration: shape intersection + 8-directional probes
- Placed objects: `RigidBody::Static` + `ColliderConstructorHierarchy(TrimeshFromMesh)`
- Terrain: `RigidBody::Static` + heightfield collider (4x downsampled)

## Build System

`build.rs` generates `assets/meshes/manifest.manifest` at compile time by scanning `assets/meshes/` (excluding `character/`). Pipe-delimited `name|asset_path` format, loaded async via custom `MeshManifestLoader`.

## Platform Differences

| Feature | Native | WASM |
|---------|--------|------|
| Storage | `std::fs` + tokio | OPFS (web-sys) |
| Lightmap baking | GPU compute shaders | CPU raycast fallback |
| Chunk streaming | Bevy asset_server | spawn_local futures + Arc<Mutex> |

## Worktree Workflow

For large features, use `./wt.sh new <name>` to create an isolated worktree at `.worktrees/<name>/` on branch `feature/<name>`. Run a separate `claude` instance there. Merge back when done.
