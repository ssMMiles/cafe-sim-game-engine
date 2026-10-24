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

## Repository Layout

```
crates/engine-api/      # Shared crate linked by host AND plugin dylibs (stable TypeIds)
│   ├── lib.rs          # DynamicPlugin trait, add! macro, ScriptOwned, ABI version
│   ├── script.rs       # ScriptBinding(s), PropValue, ScriptSchema, ScriptRegistry, LevelDataHooks
│   └── toon_material.rs
plugins/                # Dynamic plugin crates (dylibs): cafe-sim, example, example2
projects/               # Project manifests: projects/{id}/project.toml (plugins, worlds, start_world)
docs/                   # Design docs (LEVEL_SAVE_DESIGN.md — level save system, v3 format)
src/
├── lib.rs              # App init, plugin registration, custom "world" asset source
├── main.rs             # Binary entry point
├── projects.rs         # Project discovery (project.toml), ActiveProject resource
├── script_host.rs      # Dylib loader, hot-reload (Shift+R), script hydration/dehydration,
│                       #   plugin level-data save/load hooks
├── settings.rs         # UserSettings resource, SliderSetting trait
├── storage/            # Platform-agnostic async file I/O (native + WASM)
├── ui/                 # UiTheme, UiAnchors, UiExt widget trait
│   ├── theme.rs        # Colors, typography, spacing constants
│   ├── anchors.rs      # Persistent screen-edge anchor entities
│   └── widgets.rs      # ui_title, ui_button, ui_slider, ui_divider
├── menu/               # Main menu screen + navigation
│   ├── mod.rs          # MenuPlugin, Camera2d setup
│   └── main.rs         # Start Game / Settings buttons
├── editor/             # EditorPlugin — all authoring-only systems (F1 toggles Editor/Gameplay)
│   ├── mod.rs          # EditorMode state, EditorSet system sets
│   ├── controls.rs         # Context-sensitive controls panel (rows filtered by sub-mode)
│   ├── grab.rs             # R: lift a placed entity into the preview, drop it with its own id
│   ├── input.rs            # Shared scroll handling (none=cycle, Shift=scale, Alt=rotate)
│   ├── mesh_selector.rs    # Placement catalog (Tab): meshes + lights + plugin presets
│   ├── object_placement.rs # Placement preview/config, PlacedPicker, place/delete under crosshair
│   ├── script_binding.rs   # Scripts panel (K): attach/detach script bindings, stamp mode
│   ├── terrain_edit.rs     # Terrain brush (raise/lower, Shift+G toggle)
│   ├── terrain_save.rs     # Shift+S save trigger
│   ├── baking_trigger.rs   # Shift+B bake trigger
│   ├── debug_hud.rs        # HUD panels (FPS, keybinds, position, interaction)
│   ├── lightmap_toggle.rs  # Shift+L: baked lighting vs flat unlit debug view
│   └── physics_debug.rs    # Gizmo toggle (Shift+P)
└── game/               # Core simulation (SimulationPlugin)
    ├── mod.rs           # Plugin orchestration, system set config
    ├── simulation.rs    # Player spawn, world setup, cleanup
    ├── physics.rs       # CharacterState, kinematic controller (avian3d)
    ├── animation.rs     # AnimationGraph, state machine (Idle/Run/Jump)
    ├── gltf_loading.rs  # Scene loading, toon material conversion, interactions
    ├── pause_menu.rs    # Pause overlay, settings submenu, plugin add/remove
    ├── gfx/             # Rendering pipeline
    │   ├── camera.rs    # PlayerCamera, render-to-texture (1920x1080 HDR)
    │   ├── toon_material.rs  # ToonMaterial (StandardMaterial extension + lightmap)
    │   ├── post_process.rs   # Edge detection post-processing
    │   ├── outline.rs
    │   └── crosshair.rs
    └── world/           # WorldPlugin - terrain, objects, baking, streaming
        ├── mod.rs       # WorldPlugin, sun/ambient setup, apply_environment, missing-plugin warnings
        ├── sky.rs       # SkyDomeMaterial, 4-band toon gradient
        ├── streaming.rs # Chunk load/unload by player proximity
        ├── chunk_assets.rs  # Custom asset loaders (.terrain, .objects, world.meta)
        ├── objects.rs   # PlacedObjectRegistry, PlacedEntityRecord/PlacedKind, spawn/despawn,
        │                #   persistence, light gizmos (legacy module reads v2 files)
        ├── terrain/
        │   ├── types.rs      # TerrainChunk, HeightmapData (sparse HashMap)
        │   ├── constants.rs  # CHUNK_SIZE=256 vertices, TILE_SIZE=0.25 → 63.75 m chunks (CHUNK_WORLD_SIZE in objects.rs)
        │   ├── storage.rs    # rkyv WorldMetadata (v5: environment in lightmap units,
        │   │                 #   required_plugins, plugin_data, next_entity_id) / ChunkData,
        │   │                 #   quantized heights, legacy v2 + v4 readers
        │   ├── bootstrap.rs  # Two-stage async terrain init
        │   ├── mesh.rs       # LOD mesh generation (4 levels), skirts
        │   ├── material.rs   # TerrainMaterial (stochastic no-tile sampling)
        │   ├── lod.rs        # LOD selection by camera distance
        │   ├── save.rs       # Dirty chunk + metadata persistence (always rewrites world.meta)
        │   └── reconcile.rs  # Mesh-to-storage consistency
        └── baking/
            ├── mod.rs        # BakeConfig, BakingPlugin (Shift+B trigger)
            ├── bvh.rs        # Flat triangle BVH over static scenery (PlacedObject or RigidBody::Static
            │                 #   ancestry), built once per bake, traversed on CPU + uploaded to GPU
            ├── lightmap.rs   # BakeLighting model, LightmapBuffer (f32 RGB), EXR I/O,
            │                 #   versioned lightmap paths, stale-bake warning
            ├── object_lightmaps.rs  # Per-sub-mesh object lightmap paths, NeedsObjectLightmaps,
            │                 #   apply_object_lightmaps loader system
            ├── raycast.rs    # CPU shadow/AO rays: heightmap DDA + BVH test (dual origins)
            ├── terrain.rs    # Terrain lightmap baking
            ├── objects.rs    # Per-object lightmap baking. collect_object_bake_jobs (main world,
            │                 #   needs Assets) → run_object_bake_jobs (AsyncComputeTaskPool, no ECS:
            │                 #   rasterize → shade via rayon → blur/dilate → EXR) → apply_object_bake.
            │                 #   BakeStats reports per-pass timings
            ├── gpu.rs        # GPU compute pipeline (native only)
            └── uv_unwrap.rs  # Non-overlapping per-triangle lightmap UV2 + LightmapUvCache
```

## Dynamic Plugins & Scripts

- Plugins are dylib crates under `plugins/` implementing `DynamicPlugin` (engine-api). `cargo run` does **not** build them — use `cargo build --workspace` first, or the loader silently finds no dylib.
- Projects (`projects/{id}/project.toml`) declare which plugins load; `script_host.rs` loads/hot-reloads dylibs (Shift+R or mtime change), each with its own `PluginSchedule`.
- Plugins register named *scripts* (factories) and optional `LevelDataHooks` in `ScriptRegistry`. The host hydrates `ScriptBindings` on placed entities by calling the factory and recording inserted `ComponentId`s via archetype diff; dehydration removes exactly those on unload/reload.
- **Loaded ≠ active.** `DynamicPlugin` has three hooks: `register` (scripts + level hooks; runs on every load/reload in *both* modes), `setup` (gameplay bootstrap + schedule), `teardown`. A plugin is loaded whenever the project declares it, but only *active* in `EditorMode::Gameplay` — only active plugins' schedules tick, so plugin keybinds and HUD panels can't fight the editor. Entering the editor runs every plugin's teardown, so **switching modes resets the gameplay session**. Script bindings stay hydrated in both modes (only hot-reload dehydrates), so placed plugin objects still look right while authoring.
- A plugin can claim a *level* entity instead of spawning its own: register a script with `with_default_mesh(...)` and it appears in the editor's placement catalog as a one-click "mesh + binding" preset; the factory inserts a marker the plugin's systems look for. `cafe-sim`'s `cafe-shell` works this way, so the cafe shell is level geometry (collider + baked lightmaps + BVH occlusion in the editor) and the plugin only *reads* it, for the furniture-grid anchor. Materials ship inside `Cafe.gltf` — see `docs/PLUGIN_LEVEL_ENTITY_CONTRACT.md` for why a plugin must not dress level geometry at runtime (nothing unwinds it on unload).
- `ENGINE_SCRIPTS_ABI_VERSION` (currently 4) gates loading; bump on any engine-api ABI change. WASM has no dylibs — everything degrades to a no-op.

## Level Persistence (see docs/LEVEL_SAVE_DESIGN.md)

```
worlds/{id}/
  world.meta                   # v5 (WORLD_FORMAT_VERSION): terrain config + chunk index
                               #   + EnvironmentData (sun/ambient, intensities in lightmap units) + required_plugins + plugin_data
                               #   + next_entity_id (placed-entity id counter, seeds the registry on load)
  chunks/chunk_X_Y.terrain     # v2 (CHUNK_FORMAT_VERSION), rkyv frame: magic + version + crc
  chunks/chunk_X_Y.objects     # v4: Vec<PlacedEntityRecord> — kind (Mesh/PointLight/SpotLight/Empty;
                               #   light intensity is in lightmap units, not lumens)
                               #   + Vec<ScriptBinding> with typed PropValue props
  chunks/chunk_X_Y_lightmap.v6.exr, objects/obj_{id}_{k}_lightmap.v6.exr  (k = sub-mesh index)
                               # HDR OpenEXR (f16 RGB, ZIP) radiance; LIGHTMAP_FORMAT_VERSION is in the
                               #   file name, so older bakes are skipped (with a warning), never misread
```

All rkyv; v2 and v4 `world.meta` (and v2/v3 `.objects` files) are read via `legacy` modules and upgraded in memory, rewritten as current on next save (v3 `world.meta`, never released, is rejected and starts fresh — pre-v4 test worlds are meant to be deleted, not migrated). Shift+S saves dirty chunks, placed entities, environment, and plugin level data (`world.meta` is always rewritten, even with no dirty chunks). `required_plugins` is a warn-only union — missing plugins are never auto-added to the project manifest, and unhydrated bindings survive save round-trips.

## Shaders (`assets/shaders/`)

| Shader | Purpose |
|--------|---------|
| `sky_dome.wgsl` | 4-band toon sky gradient |
| `toon_lighting.wgsl` | Object fragment: samples baked HDR lightmap, hue-preserving toon bands (no runtime lights) |
| `terrain_toon.wgsl` | Terrain fragment: stochastic stone texture × the same baked toon shading |
| `lightmap_bake.wgsl` | GPU compute terrain bake (sun + lights + AO + bounce; stack BVH traversal → packed f16 RGB) |
| `edge_detection_post.wgsl` | Post-process edge detection |

## Key Systems & Keybinds

| Key | Action |
|-----|--------|
| WASD | Movement |
| Space | Jump |
| Escape | Pause menu |
| F1 | Toggle Editor / Gameplay mode |
| Shift+R | Force hot-reload all plugins |
| Shift+E | Toggle edit mode |
| Tab | Toggle placement catalog: meshes, lights, plugin presets (edit mode) |
| K | Toggle scripts panel (edit mode) |
| Enter / Backspace | Attach / detach selected script to the placed entity under the crosshair (scripts panel open) |
| LMB | Place selected entry (edit mode) / Raise terrain (terrain edit) |
| RMB | Lower terrain (terrain edit) |
| Delete | Delete placed entity under the crosshair, lights included (edit mode) |
| R | Grab the placed entity under the crosshair; again to cancel (edit mode) |
| Scroll | Cycle the open panel's selection (edit mode) |
| Shift+Scroll | Adjust placement scale |
| Alt+Scroll | Rotate placement (yaw, 15° steps) |
| Shift+G | Toggle terrain edit mode |
| `[` / `]` | Adjust brush size |
| Shift+S | Save terrain |
| Shift+B | Bake lightmaps (3×3 chunks around the player by default; `BakeConfig::bake_radius`) |
| Shift+L | Toggle baked lighting / flat unlit debug view |
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
- **All lighting is baked** — sun, ambient, and every placed point/spot light. A lightmap texel is outgoing radiance in *lightmap units* (1.0 = full base colour); the toon shaders only sample, band (hue-preserving, 5 bands, bottom band cross-faded to a neutral `TOON_FLOOR` of 0.18 so nothing is pure black), and multiply by base colour. **Band boundaries are antialiased with `fwidth`** — a hard `floor()` threshold on a filtered texture aliases at texel granularity no matter how many texels you spend, which is what made shadow edges a staircase; the ramp makes each boundary ~1 screen pixel wide at any resolution or distance. `toon_band`/`toon_shade`/`luminance` are duplicated verbatim in `toon_lighting.wgsl` and `terrain_toon.wgsl` (no shared WGSL import module exists) — keep them in step — no runtime light loops, shadow maps, or clustering. Model, identical in the CPU terrain, CPU object, and GPU terrain bakers: `sun_rgb * (ndotl * shadow + bounce) + ambient_rgb * ao + Σ local lights`. Local lights use a smooth bounded range falloff (not inverse-square) and a segment shadow ray that stops at the light. See `baking/lightmap.rs`.
- **`LightmapBuffer::into_image()` sets `ImageSampler::linear()` explicitly, and must keep doing so.** `lib.rs` sets `ImagePlugin::default_nearest()` for the game's pixel-art look, so an image left on `ImageSampler::Default` inherits *nearest* — which point-samples the lightmap into piecewise-constant texel blocks and makes every toon band edge pixelated. Nothing downstream can compensate (the shader's `fwidth` AA measures a rate of change that is zero inside each texel; the bake-side blur just yields a staircase of flat blocks; more resolution only shrinks them), and `uv_unwrap`'s gutter/dilation exists specifically to feed bilinear taps. Covered by `baked_lightmaps_carry_an_explicit_linear_sampler`.
- Lightmaps baked via GPU compute (native) or CPU raycast (WASM) into `Rgba16Float` textures, stored as OpenEXR. Terrain: one per chunk, decoded in the chunk's async compute task. Objects: one per toon-shaded sub-mesh entity (shared mesh assets share UVs, so a per-object atlas would collide), applied by `apply_object_lightmaps` once the scene has toon materials
- Object lightmap UVs are generated by `uv_unwrap`: **every triangle gets its own cell** in a grid atlas, with a gutter, and the mesh is de-indexed so corners are unshared. Cell size is budgeted by **world-space** triangle size (`BakeConfig::mesh_texels_per_metre`, floored by `mesh_texels_per_triangle`, capped by `mesh_resolution_max`) and computed per *placed instance*, not per mesh asset — triangle count alone gave the cafe's 20×12 m two-triangle floor a 32 px atlas (~0.5 texels/m vs the terrain's 16), which the 5-band toon quantiser turned into staircased shadow edges. Dilation passes are derived from the cell size (`dilate_passes_for_atlas`), since the gutter is a fraction of the cell and grows with resolution. Objects are blurred with `LightmapBuffer::blur_cells` (confined to each triangle's cell, masked to covered texels) rather than the plain `blur` terrain uses — atlas neighbours are not surface neighbours. Overlapping UVs are the one thing a lightmap cannot tolerate — the previous triplanar projection bucketed by *unsigned* dominant axis, so a cube's top and bottom shared texels and faces wore each other's lighting.
- Object texels are positioned with **each sub-mesh entity's own `GlobalTransform`** (`ToonMeshEntry::transform`), never the placed root's: glTF nodes carry their own scale/offset (the rock is scaled 2.5× inside its file), and a texel placed by the root transform alone sits inside the real mesh, where self-shadowing bakes it black.
- The one `DirectionalLight` (`BakedSun`, shadows off) and `GlobalAmbientLight` exist only for meshes still on `StandardMaterial` (player character, editor previews); `world/mod.rs` derives their PBR values from the lightmap-unit intensities
- Placed `PointLight`/`SpotLight` entities are **bake inputs only** — `spawn_placed_entity` never attaches a bevy light component, so they cost nothing at runtime and are visualised by `draw_light_gizmos` in the editor. Consequence: baked light does not fall on dynamic objects (the player), which stay lit by the sun + ambient.
- **Shadow casting uses real triangles**, not convex hulls: `baking/bvh.rs` builds one flat BVH per bake over every toon-shaded mesh whose ancestry includes a `PlacedObject` or a `RigidBody::Static` (so editor props and plugin scenery like the cafe shell occlude, but a plugin's moving toon mesh does not). The CPU bakers traverse it and the GPU bake traverses the same uploaded nodes. Objects shadow themselves; acne is avoided by offsetting ray origins along the *face* normal, and terrain rays use two origins (0.5 up for the coarse DDA, at the surface for the exact triangle test). AO rays also test the BVH, which yields contact shadows under furniture. Bounce rays are absorbed by scenery but only terrain re-emits.
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
