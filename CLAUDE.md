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

**engine** is a 3D terrain editor and exploration game built with Bevy 0.20 and Rust. It supports both native (Windows/Linux/Mac) and WASM (WebGPU) platforms.

## Tech Stack

- **Bevy 0.20** - Game engine (ECS)
- **bevy_egui 0.43 + egui_dock 0.21** - Editor chrome only (docked viewport, panels). The invariant: **bevy_egui and egui_dock must agree on the `egui` minor** (0.36 on this line). Never add `egui` directly; use `bevy_egui::egui`. Panels and `DockArea` take a `&mut Ui` (the editor builds one root `Ui` per pass); `Window` still takes `&Context`. Upgrade history: `docs/BEVY_0_20_UPGRADE_PLAN.md`
- **avian3d 0.8** - Physics (kinematic character controller, colliders)
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
docs/                   # Design docs (LEVEL_SAVE_DESIGN.md — level save system; EDITOR_UI_DESIGN.md — docked editor, phased)
src/
├── lib.rs              # App init, plugin registration, custom "world" asset source
├── main.rs             # Binary entry point
├── projects.rs         # Project discovery (project.toml), ActiveProject resource
├── script_host.rs      # Dylib loader, hot-reload (Shift+R), script hydration/dehydration,
│                       #   plugin level-data save/load hooks
├── settings.rs         # UserSettings + InputLock resources (engine-api types)
├── storage/            # Platform-agnostic async file I/O (native + WASM)
├── ui/                 # bevy_ui for *player-facing* UI (main menu, plugins). Nothing editor-side uses it
│   ├── theme.rs        # Colors, typography, spacing constants
│   └── widgets.rs      # ui_title, ui_button (+ checkbox, dropdown, text_input modules). Buttons are
│                       #   `ui_widgets::Button` + picking `Hovered`; handlers poll `Added<Pressed>` (the
│                       #   old `Interaction` enum is gone), `update_interactive_visuals` recolours on change
├── menu/               # Main menu screen + navigation
│   ├── mod.rs          # MenuPlugin, Camera2d setup
│   └── main.rs         # Start Game / Settings buttons
├── editor/             # EditorPlugin — docked editor (F1 toggles Editor/Gameplay). See docs/EDITOR_UI_DESIGN.md
│   ├── mod.rs              # EditorMode state, EditorSet {Prepare, InGame, Active}, wiring
│   ├── tool.rs             # EditorTool state (Select/Place/Terrain): 1/2/3, Shift+E, Shift+G, Esc cancels
│   ├── ui/                 # egui chrome (EguiPrimaryContextPass, runs in PostUpdate)
│   │   ├── mod.rs          #   EguiPlugin bootstrap, DockLayout, `Panels` SystemParam, status bar
│   │   ├── viewport.rs     #   TabViewer; viewport tab draws the render texture, fills ViewportInput
│   │   ├── menu.rs         #   Menu bar (File/Edit/View/Tools/Plugins), toolbar, New-world dialog
│   │   ├── outliner.rs     #   Loaded placed entities: filter, select, focus, context menu
│   │   ├── inspector.rs    #   Name/transform/kind/light params, script props, lighting; environment when nothing selected
│   │   ├── tool_panel.rs   #   "Tool" tab: shows the active tool's settings (Select: none yet)
│   │   ├── history_panel.rs #  "History" tab: undo/redo stacks, click an entry to jump
│   │   ├── assets.rs       #   "Assets" tab: catalog by category (meshes/built-ins/prefabs/textures/audio),
│   │   │                   #   search, thumbnail grid; click arms placement + Place tool; save-as-prefab
│   │   ├── place.rs        #   Place body: what's being placed, Assets… shortcut, scale/yaw/stamp
│   │   └── terrain.rs      #   Terrain body: brush mode (sculpt/flatten/smooth), radius, strength, falloff
│   ├── edits.rs            # PlacedEdited → commit_placed_edits (chunk re-derive, dirty, name sync, LightingState);
│   │                       #   ScriptEdit (attach/detach/set props → detach_binding + NeedsHydration); RespawnPlaced
│   ├── actions.rs          # EditorAction (save, bake, worlds, plugins, exit, selection ops) applied in Update
│   ├── catalog.rs          # AssetCatalog/AssetKind/AssetEntry: manifest (meshes/textures/audio) + built-ins
│   │                       #   + PrefabLibrary + plugin presets; select() arms placement and stamps
│   ├── previews.rs         # PreviewImages: bevy Image → egui TextureId hand-off (bound in editor_ui)
│   ├── thumbnails.rs       # ThumbnailCache + offscreen rig (layer 7 camera + light) rendering catalog entries
│   ├── picking.rs          # ViewportInput (pointer in texture px) → EditorRay; PlacedPicker
│   ├── camera.rs           # Fly camera (RMB look, WASD/QE), cursor lock while looking,
│   │                       #   FlyLock (viewport button → fly without holding RMB, Esc exits),
│   │                       #   render-target resize to the tab, sprite hide/show
│   ├── selection.rs        # Selection {primary, hovered, focus_requested}, click-select, gizmo box
│   │                       #   (hover + boxes are Select-tool-only), F, Delete
│   ├── gizmo.rs            # Transform gizmo (Select tool): drag XYZ arrows to translate, Ctrl = rotation
│   │                       #   arcs; hand-rolled ray math vs EditorRay, Esc cancels the drag.
│   │                       #   EditorGizmoGroup (fat lines, depth_bias -1)
│   ├── history.rs          # Undo/redo: per-world Transaction stacks of before/after Changes keyed by placed id;
│   │                       #   LevelOps applies them through the same messages (flagged `replay`); chunk pinning
│   ├── prefab_edit.rs      # PrefabAction {SaveSelectionAs, Apply, Revert, Unpack}; instance re-expansion
│   ├── controls.rs         # ActiveControls: context-filtered key hints for the status bar
│   ├── grab.rs             # R: lift a placed entity into the preview, drop it with its own id
│   ├── input.rs            # keyboard_free (egui text focus), scroll modifier + notches
│   ├── object_placement.rs # Placement preview/config, place at cursor ray (Place tool)
│   ├── terrain_edit.rs     # Terrain brush: BrushSettings {mode sculpt/flatten/smooth, radius, strength,
│   │                       #   falloff}; flatten targets the stroke-start height, smooth averages a snapshot
│   ├── terrain_save.rs     # Shift+S save trigger
│   ├── baking_trigger.rs   # Shift+B bake trigger
│   ├── lightmap_toggle.rs  # Shift+L: baked lighting vs flat unlit debug view
│   └── physics_debug.rs    # Gizmo toggle (Shift+P)
└── game/               # Core simulation (SimulationPlugin)
    ├── mod.rs           # Plugin orchestration, system set config
    ├── simulation.rs    # Player spawn, world setup, cleanup
    ├── physics.rs       # CharacterState, kinematic controller (avian3d)
    ├── animation.rs     # AnimationGraph, state machine (Idle/Run/Jump)
    ├── gltf_loading.rs  # Scene loading, toon material conversion, interactions
    ├── gfx/             # Rendering pipeline
    │   ├── camera.rs    # PlayerCamera, render-to-texture (GameRenderTarget: 1920x1080 HDR in gameplay,
    │   │                #   resized to the viewport tab in the editor); Camera2d hosts PrimaryEguiContext
    │   ├── toon_material.rs  # ToonMaterial (StandardMaterial extension + lightmap)
    │   ├── post_process.rs   # Edge detection post-processing
    │   ├── outline.rs
    │   └── crosshair.rs
    └── world/           # WorldPlugin - terrain, objects, baking, streaming
        ├── mod.rs       # WorldPlugin, sun/ambient setup, apply_environment, missing-plugin warnings
        ├── sky.rs       # SkyDomeMaterial, 4-band toon gradient
        ├── streaming.rs # Chunk load/unload by player proximity
        ├── chunk_assets.rs  # Custom asset loaders (.terrain, .objects, world.meta)
        ├── objects.rs   # PlacedObjectRegistry, PlacedEntityRecord/PlacedKind, spawn/despawn
        │                #   (prefab roots expand there), persistence, light gizmos, legacy readers
        ├── prefabs.rs   # PrefabFile/PrefabInstanceData/PrefabOverride, PrefabLibrary (assets/prefabs,
        │                #   OPFS on web), define/expand/diff helpers — see docs/PREFABS_PLAN.md
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
- `ENGINE_SCRIPTS_ABI_VERSION` (currently 7) gates loading; bump on any engine-api ABI change. WASM has no dylibs — everything degrades to a no-op.

## Level Persistence (see docs/LEVEL_SAVE_DESIGN.md)

```
worlds/{id}/
  world.meta                   # v5 (WORLD_FORMAT_VERSION): terrain config + chunk index
                               #   + EnvironmentData (sun/ambient, intensities in lightmap units) + required_plugins + plugin_data
                               #   + next_entity_id (placed-entity id counter, seeds the registry on load)
  chunks/chunk_X_Y.terrain     # v2 (CHUNK_FORMAT_VERSION), rkyv frame: magic + version + crc
  chunks/chunk_X_Y.objects     # v6: Vec<PlacedEntityRecord> — kind (Mesh/PointLight/SpotLight/Empty/Group;
                               #   light intensity is in lightmap units, not lumens)
                               #   + Vec<ScriptBinding> with typed PropValue props + parent: Option<u64> (group id)
                               #   + prefab: Option<PrefabInstanceData> on instance roots (members never stored)
  chunks/chunk_X_Y_lightmap.v6.exr, objects/obj_{id}_{k}_lightmap.v6.exr  (k = sub-mesh index)
                               # HDR OpenEXR (f16 RGB, ZIP) radiance; LIGHTMAP_FORMAT_VERSION is in the
                               #   file name, so older bakes are skipped (with a warning), never misread
assets/prefabs/{name}.prefab   # prefab definitions (v1, global): records with local ids,
                               #   root-relative transforms; OPFS `prefabs/` on web
```

All rkyv; v2 and v4 `world.meta` (and v2–v5 `.objects` files) are read via `legacy` modules and upgraded in memory, rewritten as current on next save (v3 `world.meta`, never released, is rejected and starts fresh — pre-v4 test worlds are meant to be deleted, not migrated). Shift+S saves dirty chunks, placed entities, environment, and plugin level data (`world.meta` is always rewritten, even with no dirty chunks). `required_plugins` is a warn-only union — missing plugins are never auto-added to the project manifest, and unhydrated bindings survive save round-trips.

## Shaders (`assets/shaders/`)

Shaders are **WESL** (`.wesl`): `import bevy_pbr::render::forward_io::VertexOutput;` (module paths mirror the crate's file layout — `bevy_pbr::render::*`, `bevy_pbr::prepass::io`, `bevy_pbr::deferred::functions`), `@if(DEF)` / `@else` attach to one declaration, import or statement, and bind-group indices are `@group(constants::MATERIAL_BIND_GROUP)`. naga_oil (`#import`, `#ifdef`, `#{…}`) is gone since Bevy 0.20. A plain WGSL file with no directives still loads as `.wgsl` — `lightmap_bake.wgsl` is one, and `cargo test` validates it with the same `naga` wgpu links (keep the `naga` dev-dependency on wgpu's major).

| Shader | Purpose |
|--------|---------|
| `sky_dome.wesl` | 4-band toon sky gradient |
| `terrain.wesl` | Terrain vertex: passes UV1 lightmap coords, flips skirt normals |
| `toon_lighting.wesl` | Object fragment: samples baked HDR lightmap, hue-preserving toon bands (no runtime lights) |
| `terrain_toon.wesl` | Terrain fragment: stochastic stone texture × the same baked toon shading |
| `lightmap_bake.wgsl` | GPU compute terrain bake (sun + lights + AO + bounce; stack BVH traversal → packed f16 RGB) |
| `edge_detection_post.wesl` | Post-process edge detection (a `Core3d` system after `tonemapping`, pipeline specialized on `ExtractedView::target_format`) |

**WebGPU is stricter than native about uniform control flow.** `textureSample`/`textureSampleBias` (implicit LOD) and `dpdx`/`fwidth` must sit in uniform control flow. Chrome's Tint enforces this; naga hard-codes `DISABLE_UNIFORMITY_REQ_FOR_FRAGMENT_STAGE = true`, so native compiles the shader fine and the web build dies at pipeline creation with `[Invalid ShaderModule]` and a black screen. A `var` initialised from fragment inputs (`pbr_input` from `pbr_input_from_vertex_output(in, is_front, …)`) stays non-uniform forever — partial assignments to its fields don't launder it — so branch on a `let` read straight from the uniform (`let flags = pbr_bindings::material.flags;`), the way `pbr_fragment.wesl` does, never on `pbr_input.material.flags`. The `naga` test can't catch this class; Chrome's Tint can — `node tools/tint-check.mjs file.wgsl` compiles WGSL through headless Chrome and exits non-zero on error. It takes WGSL, not WESL (our shaders only become WGSL inside bevy's resolver at runtime), so feed it a reduced repro of the construct in question. Background: `memory.md`, "WASM parity".

## Editor Model (docs/EDITOR_UI_DESIGN.md, all five phases)

- The editor is a **docked window**: egui panels around a viewport tab that shows the camera's render texture. The 3D camera still renders to `GameRenderTarget`; in the editor the image is resized to the tab's physical size (`sync_render_target`) and the full-window sprite is hidden, in gameplay it goes back to 1920×1080 + sprite. `PrimaryEguiContext` is attached explicitly to the in-game `Camera2d` (`auto_create_primary_context` is off, because auto-creation grabs the main-menu camera). bevy_ui renders *above* egui.
- **Pointer authority is `ViewportInput`**, filled from the viewport tab's egui `Response` each pass (hovered, cursor in texture pixels, look-drag, click, scroll, size). bevy_egui's "wants pointer" helpers are useless with a whole-window dock. `compute_editor_ray` (first in `EditorSet::Prepare`) turns it into one `EditorRay` per frame; the picker, placement preview and terrain brush all read that ray. Mouse/wheel consumers gate on `viewport_pointer_free`; key bindings gate on `keyboard_free` (egui text focus). One frame of latency (egui pass is in PostUpdate) is by design.
- **One camera, two drivers.** In `EditorMode::Editor` `editor_fly_camera` owns the transform (RMB drag look, WASD/QE while looking, scroll = fly speed while looking / dolly idle); `camera_follow`, free-cam, `character_physics`, `check_interactions` and the sprite sizing are gated to `EditorMode::Gameplay`. `WorldFocus` (game/mod.rs) is the camera in the editor and the player in gameplay; streaming and the bake read it instead of `Player`.
- **Cursor**: free in the editor; `CursorGrabMode::Locked` + hidden only while looking, restored to where the drag began. `simulation::setup` only locks in Gameplay mode.
- **`EditorTool` state** (Select / Place / Terrain) replaced `UserSettings.edit_mode` / `terrain_edit_mode` (ABI 5). `OnExit(Place)` releases a grab, tears down the preview and closes the catalog; `OnExit(Terrain)` removes the brush disc.
- **`Selection { primary, hovered }`** is what the inspector, Delete, F and the outliner act on. Viewport click (Select tool) or an outliner row sets it; `validate_selection` drops dead entities each frame; placement/drop/duplicate select the new entity. A **transform gizmo** (`gizmo.rs`) draws XYZ arrows (Ctrl: rotation arcs) at the selection origin; dragging a handle writes `Transform` + `PlacedEdited` per frame (so group deltas stay correct) and suppresses click-select via `gizmo_pointer_free`. Drag state is a resource (`GizmoInteraction`), never `ViewportInput` (rebuilt every egui pass).
- **Every level-data mutation goes through `edits.rs`.** Panels write components directly and send `PlacedEdited { entity, what }`; `commit_placed_edits` re-derives `PlacedObject.chunk` from the transform (dirtying both chunks on a boundary crossing), syncs `Interactable.name` to the mesh children, and sets `LightingState.stale`. Script props are *drafted* in the inspector and committed as one `ScriptEdit::SetProps` when the user lets go, because applying = `detach_binding` + `NeedsHydration` (the host skips already-hydrated pairs), which rebuilds the plugin's components. Mesh asset / collider changes go through `RespawnPlaced::InPlace` (same id).
- **Menu/toolbar commands are `EditorAction` messages** applied in `Update` (`actions.rs`): save, bake, world switch/create (with the editor camera re-seated), plugin add/remove/reload (`ReloadRequested` in script_host), exit to menu, selection ops. Panels never touch those systems directly.
- **The engine has no pause menu any more.** `pause_menu.rs`, `PauseMenuState`, `UiAnchors` and the bevy_ui slider are gone. In the editor, Escape backs out one level (put a carried entity back → Select tool → deselect). `SimulationState::Paused` still exists for plugins. The engine never pauses on its own.
- Environment (sun azimuth/elevation, colours, intensities) is edited in the inspector when nothing is selected, by writing `TerrainWorld.metadata.environment`; `apply_environment` reacts to the resource change and Shift+S persists it.
- **Undo/redo (`history.rs`, `docs/UNDO_REDO_PLAN.md`).** `History` holds per-world stacks of `Transaction`s, each a list of `Change`s: before/after pairs of plain data keyed by **placed id**, never `Entity` (grab, respawn and streaming all mint new entities). Recording happens at the choke points, not in panels: `PlacedEdited` carries the value it replaced (`PlacedEdited::transform(entity, before)` etc.), so `commit_placed_edits` records transform/name/kind edits — drags coalesce into one open transaction per `(kind, id)` that `settle_history` closes when the pointer is up and egui is idle (`ViewportInput.egui_busy`), and a group move folds every member's exact before/after into the group's transaction so undo never re-runs the cascade. `apply_script_edits`, `delete_placed`, place, duplicate, grab drop/destroy, group/parent actions, the terrain stroke end and the environment panel record too. Undo applies in reverse through `LevelOps`, re-using the same messages flagged as replays (`PlacedEdited.replay`, `ScriptEdit::Replace`) so nothing is recorded twice. Each transaction restores the selection it touched. Chunks the stacks reference are pinned against streaming unload (`History::pinned`, checked in `update_streaming_ring`); if a target still isn't loaded the whole transaction is skipped and dropped with a status-bar notice. Caps: 200 transactions and 64 MB of terrain tiles per world. Switching worlds swaps stacks; nothing clears them. A terrain stroke also rebuilds the chunk's heightfield collider (`TerrainChunks::rebuild_collider`), which sculpting used to leave stale until reload.
- **Prefabs are linked instances (`world/prefabs.rs`, `editor/prefab_edit.rs`, `docs/PREFABS_PLAN.md`).** A definition lives in `assets/prefabs/` (library loaded at startup; OPFS on web, saves are fire-and-forget there). Only the instance **root** is stored in a chunk file; `spawn_entity_record` expands members from the library into the root's chunk, the root's own kind/name/scripts come from the definition (so editing the prefab updates every instance on next load, or immediately via Apply), and `(local, world)` member-id pairs keep ids stable across re-expansion. Overrides are **section-level** per member and recomputed by diffing live state at every snapshot point (save, delete, respawn, grab). `save_placed_objects` skips member ids. Apply rewrites the definition keeping surviving locals (so other instances' pairs stay meaningful), folds extras in, and re-expands other loaded instances with their deviations preserved; Revert drops deviations; Unpack severs the link (entities become plain level data). All three are undoable (`Change::PrefabDefinition`, `Change::PrefabInstance`). Missing prefab file → bare roots + status-bar warning, healing when the file returns; nested prefabs are refused; members can't be re-filed; instance roots can't be ungrouped.
- **Groups are organisational, not transform parents.** `PlacedKind::Group` is a placed entity like any other (transform, chunk, id); members carry `PlacedObject.parent = Some(group id)` and keep *world* transforms, no ECS `ChildOf`. The outliner draws the id tree (members whose group isn't loaded show at top level with ⤴), rows drag onto groups, a bottom strip drops to top level. Moving a group in the inspector applies the delta to its loaded members (`commit_placed_edits` keeps a per-group transform snapshot to measure the delta). Deleting a group re-files members under its parent; Shift+Delete / "Delete with members" is recursive. Groups aren't viewport-pickable; Alt+click a member to select its outermost group. `selection::{descendants, ancestors, PlacedTree}` are the tree helpers.

## Key Systems & Keybinds

| Key | Action |
|-----|--------|
| WASD / Space | Gameplay movement / jump |
| F1 | Toggle Editor / Gameplay mode |
| Shift+R | Force hot-reload all plugins |
| RMB drag (viewport) | Look; WASD/QE fly while held, Shift ×4, scroll = fly speed |
| "Fly" button (viewport corner) | Lock the cursor and fly without holding RMB (clicks suppressed); Esc exits |
| Scroll (viewport, idle) | Dolly |
| 1 / 2 / 3 | Select / Place / Terrain tool (viewport hovered); Shift+E / Shift+G toggle Place / Terrain |
| LMB | Select tool: select (or drag a gizmo handle) · Place tool: place / drop grabbed · Terrain: apply brush |
| Alt+LMB | Select the outermost group of the entity under the cursor |
| Ctrl (Select tool) | Gizmo shows rotation arcs instead of translation arrows |
| Ctrl+LMB | Lower terrain (Terrain tool, Sculpt mode) |
| Esc | Exit fly lock → cancel a gizmo drag (restores the start transform) → put a carried entity back → back to Select tool → deselect |
| Ctrl+Z / Ctrl+Shift+Z / Ctrl+Y | Undo / redo (Cmd works too; refused mid-drag or mid-stroke, puts a carried entity back first) |
| F | Focus camera on the selection (also outliner double-click) |
| Delete | Delete the selection (a group keeps its members); Shift+Delete deletes a group with its members |
| R | Grab the entity under the cursor (else the selection); again to cancel. Switches to Place while carrying |
| Shift+Scroll | Adjust placement scale |
| Alt+Scroll | Rotate placement (yaw, 15° steps) |
| `[` / `]` | Adjust brush size (Terrain tool) |
| Shift+S | Save terrain |
| Shift+B | Bake lightmaps (3×3 chunks around the player by default; `BakeConfig::bake_radius`) |
| Shift+L | Toggle baked lighting / flat unlit debug view |
| Shift+P | Toggle physics debug gizmos |

## Terrain Streaming Pipeline

1. `update_streaming_ring` - Determines which chunks are in range of `WorldFocus` (Chebyshev distance, load_radius=4, unload_radius=6)
2. `launch_chunk_tasks` - When asset loads complete, spawns AsyncComputeTaskPool mesh generation
3. `finalize_computed_chunks` - Polls tasks, spawns chunk entities + objects (max 2/frame)

Chunk data uses rkyv zero-copy serialization. Heights are quantized u16 (0-65535) with smart storage modes: UseBase (unmodified) > Delta (sparse edits) > Full (heavy mods).

## Rendering Pipeline

- The camera uses `Tonemapping::Linear` (no tone curve, keeps the clamp). Bevy 0.20 made `None` a full passthrough that drops the negative-channel clamp — never put that in front of the 5-band quantiser
- Camera renders to a float16 HDR image (`GameRenderTarget`): 1920x1080 displayed via sprite on layer 1 in gameplay; in the editor the image is resized to the egui viewport tab and drawn as an egui texture (bevy_egui binds any filterable float texture and does its own linear→sRGB, so colours match the sprite path)
- Materials: `ExtendedMaterial<StandardMaterial, ToonMaterial>` for objects, `ExtendedMaterial<StandardMaterial, TerrainMaterial>` for terrain
- Material bind group 3; use `@group(constants::MATERIAL_BIND_GROUP)` in WESL
- **All lighting is baked** — sun, ambient, and every placed point/spot light. A lightmap texel is outgoing radiance in *lightmap units* (1.0 = full base colour); the toon shaders only sample, band (hue-preserving, 5 bands, bottom band cross-faded to a neutral `TOON_FLOOR` of 0.18 so nothing is pure black), and multiply by base colour. **Band boundaries are antialiased with `fwidth`** — a hard `floor()` threshold on a filtered texture aliases at texel granularity no matter how many texels you spend, which is what made shadow edges a staircase; the ramp makes each boundary ~1 screen pixel wide at any resolution or distance. `toon_band`/`toon_shade`/`luminance` are duplicated verbatim in `toon_lighting.wesl` and `terrain_toon.wesl` (no shared import module exists) — keep them in step — no runtime light loops, shadow maps, or clustering. Model, identical in the CPU terrain, CPU object, and GPU terrain bakers: `sun_rgb * (ndotl * shadow + bounce) + ambient_rgb * ao + Σ local lights`. Local lights use a smooth bounded range falloff (not inverse-square) and a segment shadow ray that stops at the light. See `baking/lightmap.rs`.
- **`LightmapBuffer::into_image()` sets `ImageSampler::linear()` explicitly, and must keep doing so.** `lib.rs` sets `ImagePlugin::default_nearest()` for the game's pixel-art look, so an image left on `ImageSampler::Default` inherits *nearest* — which point-samples the lightmap into piecewise-constant texel blocks and makes every toon band edge pixelated. Nothing downstream can compensate (the shader's `fwidth` AA measures a rate of change that is zero inside each texel; the bake-side blur just yields a staircase of flat blocks; more resolution only shrinks them), and `uv_unwrap`'s gutter/dilation exists specifically to feed bilinear taps. Covered by `baked_lightmaps_carry_an_explicit_linear_sampler`.
- **The CPU terrain bake runs two passes.** AO and indirect bounce were ~95% of its ray cost (measured: 329 ns sun / 3347 ns AO / 2829 ns bounce per texel) and both vary over metres, so they are evaluated on a grid `BakeConfig::cpu_lowfreq_downsample` (default 4) times coarser and bilinearly interpolated, while the sun and local-light shadows — the sharp terms the toon bands key off — stay per-texel. `blur_radius: 4` already smears the result over ~25 cm at 16 texels/m, so the lost detail was being discarded anyway. Separately, every terrain march early-outs once a climbing ray clears `BakeHeightCache::max_height` (exact, not an approximation). Together ~39× (a 3×3 region: ~61 s → ~1.6 s single-threaded). The GPU path does neither, so native GPU and CPU bakes differ very slightly in AO; set `cpu_lowfreq_downsample: 1` for parity. Re-measure with `cargo test --release terrain_bake_cost_breakdown -- --ignored --nocapture`.
- **The GPU bake runs on WebGPU too**, gated on a runtime capability flag (`gpu::GpuBakeSupported`), never a `cfg`: one wasm binary gets compute on WebGPU and not on a WebGL2 fallback. Set at `RenderStartup` from the adapter's `DownlevelFlags::COMPUTE_SHADERS` **and** the `max_compute_workgroup_storage_size` canary (WebGL2 downlevel limits fake a lack of compute while still reporting the flag) — both halves are bevy's own test. Defaults to `false`, so a bake before `RenderStartup` takes the CPU path.
- **The CPU bake is single-threaded on WASM and blocks the tab**, which is why the GPU path matters most there. rayon falls back to a one-thread pool on `wasm32-unknown-unknown` (documented in rayon-core: `std::thread` returns `Unsupported`), so every `into_par_iter` in the bakers is serial, and bevy's wasm task pool is `spawn_local` on the main thread — `AsyncComputeTaskPool` does not move work off it. The **object** bake is still CPU on every platform, so it stays subject to this.
- Lightmaps baked via GPU compute (native) or CPU raycast (WASM) into `Rgba16Float` textures, stored as OpenEXR. Terrain: one per chunk, decoded in the chunk's async compute task. Objects: one per toon-shaded sub-mesh entity (shared mesh assets share UVs, so a per-object atlas would collide), applied by `apply_object_lightmaps` once the scene has toon materials
- Object lightmap UVs are generated by `uv_unwrap`: **every triangle gets its own cell** in a grid atlas, with a gutter, and the mesh is de-indexed so corners are unshared. Cell size is budgeted by **world-space** triangle size (`BakeConfig::mesh_texels_per_metre`, floored by `mesh_texels_per_triangle`, capped by `mesh_resolution_max`) and computed per *placed instance*, not per mesh asset — triangle count alone gave the cafe's 20×12 m two-triangle floor a 32 px atlas (~0.5 texels/m vs the terrain's 16), which the 5-band toon quantiser turned into staircased shadow edges. Dilation passes are derived from the cell size (`dilate_passes_for_atlas`), since the gutter is a fraction of the cell and grows with resolution. Objects are blurred with `LightmapBuffer::blur_cells` (confined to each triangle's cell, masked to covered texels) rather than the plain `blur` terrain uses — atlas neighbours are not surface neighbours. Overlapping UVs are the one thing a lightmap cannot tolerate — the previous triplanar projection bucketed by *unsigned* dominant axis, so a cube's top and bottom shared texels and faces wore each other's lighting.
- Object texels are positioned with **each sub-mesh entity's own `GlobalTransform`** (`ToonMeshEntry::transform`), never the placed root's: glTF nodes carry their own scale/offset (the rock is scaled 2.5× inside its file), and a texel placed by the root transform alone sits inside the real mesh, where self-shadowing bakes it black.
- The one `DirectionalLight` (`BakedSun`, shadows off) and `GlobalAmbientLight` exist only for meshes still on `StandardMaterial` (player character, editor previews); `world/mod.rs` derives their PBR values from the lightmap-unit intensities
- Placed `PointLight`/`SpotLight` entities are **bake inputs only** — `spawn_placed_entity` never attaches a bevy light component, so they cost nothing at runtime and are visualised by `draw_light_gizmos` in the editor. Consequence: baked light does not fall on dynamic objects (the player), which stay lit by the sun + ambient.
- **Shadow casting uses real triangles**, not convex hulls: `baking/bvh.rs` builds one flat BVH per bake over every toon-shaded mesh whose ancestry includes a `PlacedObject` or a `RigidBody::Static` (so editor props and plugin scenery like the cafe shell occlude, but a plugin's moving toon mesh does not). The CPU bakers traverse it and the GPU bake traverses the same uploaded nodes. Objects shadow themselves; acne is avoided by offsetting ray origins along the *face* normal, and terrain rays use two origins (0.5 up for the coarse DDA, at the surface for the exact triangle test). AO rays also test the BVH, which yields contact shadows under furniture. Bounce rays are absorbed by scenery but only terrain re-emits.
- Volumetric fog + distance fog for atmosphere; sky dome follows camera

## Physics (avian3d 0.8)

- Player: `RigidBody::Kinematic` + capsule collider (radius=0.3, cylinder_height from CHAR_HEIGHT=1.5)
- Ground detection: heightmap sampling + shape cast downward
- Wall depenetration: shape intersection + 8-directional probes
- Placed objects: `RigidBody::Static` + `ColliderConstructorHierarchy(TrimeshFromMesh)`
- Terrain: `RigidBody::Static` + heightfield collider (4x downsampled)

## Build System

`build.rs` generates `assets/asset_manifest.manifest` at compile time: `kind|name|asset_path` lines for meshes (first glTF per `assets/meshes/*/`, excluding `character/`), textures (`assets/textures/*.{png,jpg,jpeg}`) and audio (`assets/audio/*.{ogg,wav,mp3}`), loaded async via `AssetManifestLoader`. It exists because WASM cannot list directories. Prefabs are *not* in it — the `PrefabLibrary` discovers them at runtime.

## Assets Browser (`docs/ASSET_BROWSER_PLAN.md`)

- `EditorTab::Assets` (default dock: the strip under the viewport; Tool sits under the Inspector) draws `AssetCatalog` by `AssetKind`. Placeable entries (meshes, built-ins, prefabs, plugin presets) arm `PlacementConfig` through `AssetCatalog::select` — the preset stamp disarm logic from the old `MeshCatalog` is preserved and tested — and switch to the Place tool. Textures are inspect-only (nothing in the level consumes one yet); audio is listed only. The Place panel no longer lists assets.
- Thumbnails: `thumbnails::drive_thumbnails` runs one job per frame-step on a persistent rig — `Camera3d` + `DirectionalLight` on `RenderLayers::layer(7)` at `RIG_ORIGIN` (y = −5000) — rendering into a 128 px `Rgba8UnormSrgb` image with an explicit **linear** sampler (the app default is nearest). Subjects are spawned **without** `ConvertToToon`, so they keep glTF `StandardMaterial`s; `RenderLayers` is not inherited, so the driver stamps layer 7 onto `Mesh3d` descendants as they appear. Framing waits for `Aabb`s (`PostUpdate`), so a settled scene is detected a frame late by design; a subject that never loads times out to its glyph and is not retried. Only drawn cells request thumbnails; presets share their mesh's thumbnail (`AssetEntry::thumbnail_key`). `PreviewImages` is the bevy→egui texture hand-off: panels register handles, `editor_ui` binds the unbound ones each pass (`EguiTextureHandle::Weak`).

## Platform Differences

| Feature | Native | WASM |
|---------|--------|------|
| Storage | `std::fs` + tokio | OPFS (web-sys) — terrain, objects, lightmaps, **and project manifests** |
| Lightmap baking | GPU compute (any adapter with compute, incl. WebGPU) | same; CPU raycast only without compute (WebGL2) |
| Project discovery | `projects/` dir scan at startup | async OPFS scan + a seeded fallback project |
| Chunk streaming | Bevy asset_server | spawn_local futures + Arc<Mutex> |

## Worktree Workflow

For large features, use `./wt.sh new <name>` to create an isolated worktree at `.worktrees/<name>/` on branch `feature/<name>`. Run a separate `claude` instance there. Merge back when done.
