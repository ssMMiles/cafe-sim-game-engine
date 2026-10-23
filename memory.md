# Memory

## Object Placement System Update - January 2025

Added a feature to cycle through different objects to place in edit mode:

- **New PlacementObjectType enum**: Added to settings.rs with variants: Table, Cooker, Fridge, CuttingBoard
- **Settings Integration**: Added current_placement_object field to UserSettings and cycle_placement_object() method
- **Keybind**: Shift+R cycles through placement objects, displayed in the keybinds UI
- **Object Types**: Currently all use the same table mesh but with different colors:
  - Table: Brown wood (0.6, 0.4, 0.2)
  - Cooker: Bright red (0.9, 0.2, 0.2) with slight metallic finish
  - Fridge: Blue (0.2, 0.4, 0.8) with metallic finish
  - Cutting Board: Green (0.2, 0.8, 0.3)
- **Marker Components**: Added CookerMarker, FridgeMarker, CuttingBoardMarker components for identifying object types
- **Material System**: Updated override_table_materials to override_object_materials to handle all object types

The system is designed to be easily extended - when proper 3D models are available for each object type, only the asset loading in handle_edit_mode_click needs to be updated.

## Green Transparent Placement Preview - January 2025

Added a visual preview system that shows where objects will be placed in edit mode:

- **PlacementPreview Component**: Marks the preview entity
- **PlacementPreviewEntity Resource**: Tracks the current preview entity 
- **Preview Systems**:
  - `setup_placement_preview`: Creates preview entity when entering edit mode
  - `cleanup_placement_preview`: Removes preview when exiting edit mode  
  - `update_placement_preview`: Updates preview position to follow mouse cursor
  - `apply_preview_material`: Applies green transparent material to preview meshes
- **Visual Features**:
  - Green semi-transparent material (50% alpha) for clear preview indication
  - Slightly elevated (+0.05 units) to prevent z-fighting with ground
  - Hidden when no valid placement position available
  - Uses same mesh as actual objects for accurate preview
- **Integration**: Preview position is used by `handle_edit_mode_click` for precise placement alignment
## Level Save System — approved design, October 2026

Design doc: `docs/LEVEL_SAVE_DESIGN.md` (approved 2026-10-21; Phases 0–2 implemented, Phase 3 — cafe-sim migration — outstanding).

Locked decisions, do not reopen:
- Plugin script components go **directly on the placed entity**; the host records the `ComponentId`s a factory inserted (archetype diff) and strips them on plugin unload/reload.
- Script props are a **typed `PropValue` bag**, not opaque bytes.
- Keep the **`.objects`** extension; the record generalises to `PlacedEntityRecord` with a `kind` (Mesh / PointLight / SpotLight / Empty).
- `required_plugins` in `world.meta` **only warns**; never auto-add plugins to the project manifest.

Implementation order: Phase 0 formats (v2 → v3 with legacy parsers) → Phase 1 `ScriptRegistry` + hydration in `engine-api` / `script_host.rs` (bump `ENGINE_SCRIPTS_ABI_VERSION`) → Phase 2 editor → Phase 3 migrate cafe-sim.

### Phase 0 status (formats) — complete

- `.objects` is **v3**: `ChunkObjectsFile.entities: Vec<PlacedEntityRecord>` (was `objects: Vec<PlacedObjectRecord>`), each with a `PlacedKind` (Mesh / PointLight / SpotLight / Empty) and `scripts: Vec<ScriptBindingRecord>`. v2 files read via `objects::legacy` and upgrade in memory.
- `world.meta` is **v3**: adds `environment: EnvironmentData`, `required_plugins`, `plugin_data`. v2 read via `storage::legacy`.
- **The two version constants are now separate**: `CHUNK_FORMAT_VERSION = 2` (`.terrain` payload unchanged) and `WORLD_FORMAT_VERSION = 3`. Don't re-merge them.
- `PlacedObject` component now carries `kind`, `name`, and `scripts` so a load→save round trip can't drop authored data. `save_placed_objects` rebuilds records via `PlacedObject::to_record`.
- One `spawn_entity_record` replaces the old duplicated `spawn_placed_object` / `spawn_chunk_objects` bodies. It does **not** mark the chunk dirty — the editor caller does, so streaming can't dirty a chunk merely by loading it.
- Light markers are immediate-mode `Gizmos` (`draw_light_gizmos`), deliberately **not** child mesh entities: the lightmap baker walks mesh descendants of every `PlacedObject`, so a gizmo mesh would cast baked shadows and get an atlas. Keep it that way.
- `save_terrain` no longer early-returns on an empty dirty set — `world.meta` must be rewritten for environment/plugin-only edits.

Caveat: the v2→v3 readers are validated by synthetic round-trip tests only; this worktree has no `worlds/` data (it's gitignored). Loading the real `cafe` / `default` worlds is still the live check.

Note: `docs/` was removed from `.gitignore` so `docs/LEVEL_SAVE_DESIGN.md` is tracked.

### Phase 1 status (registry + hydration) — complete

- `crates/engine-api/src/script.rs` owns the shared script vocabulary: `ScriptBinding`, `ScriptProp`, `PropValue`, `ScriptBindings` (Component), `PropDef`/`PropDefault`, `ScriptSchema`, `ScriptHydrateFn`, `LevelDataHooks`, `ScriptRegistry`. **engine-api now depends on rkyv** — `ScriptBinding` is simultaneously the runtime component payload and the on-disk record, deliberately one type so the two can't drift.
- `ENGINE_SCRIPTS_ABI_VERSION` is now **3**. Host and plugins must be rebuilt together.
- Hydration lives in `script_host.rs`: `hydrate_script_bindings` snapshots the entity archetype, calls the plugin's `fn` factory, and diffs to learn which `ComponentId`s were inserted; `dehydrate_plugin` removes exactly those via `remove_by_ids`. Called from `unload_one` and `reload_plugin` **before** teardown, while the dylib's drop glue is still mapped.
- `NeedsHydration` is never cleared for a binding whose plugin isn't loaded — that's what lets a plugin added via the pause menu adopt already-spawned entities with no reload.
- `required_plugins` is written as a **union**, not a replacement: only loaded chunks have entities, so replacing would drop dependencies living in streamed-out chunks.
- Plugin level data: `collect_plugin_level_data` (ordered `.before(save_terrain)`) and `apply_plugin_level_data` (keyed on world_id so `switch_world` re-applies).
- 7 hydration tests in `script_host.rs` run with **no dylib** — factories are plain `fn` pointers, so the whole hydrate/dehydrate/reload cycle is unit-testable. Keep it that way.

Gap: nothing can *author* a binding until Phase 2, so `spinner` in `plugins/example` is unit-tested only. The level-data hooks are runtime-testable now (Shift+S, reload world). `example` is not in any project's plugin list — add it via the pause menu to try it.

### Phase 2 status (editor) — complete

- Catalog strip (Tab) now lists `CatalogEntry { name, kind: PlacedKind, preset_script }`: manifest meshes + "Point Light" + "Spot Light" + one preset per plugin script with `default_mesh`. Rebuilt when `ScriptRegistry` changes. Selection is **index-based**, not path-based.
- `PlacementConfig` is `{ kind: PlacedKind, scale, stamp_script: Option<(plugin, script)> }`. `stamp_script` is **sticky** across mesh changes by design; only the scripts panel clears it.
- `PlacedPicker` (`editor/object_placement.rs`) is the one way to find "the placed entity under the crosshair": mesh raycast → root via `InteractableParent`, then ray/sphere fallback for mesh-less kinds. Use it rather than re-implementing raycasts — it's what makes lights deletable/attachable.
- Scripts panel is **K** (cafe-sim owns plain B). Enter = attach, Backspace = detach (Delete still deletes the entity). Status line shows bindings + `(awaiting plugin)`.
- Scale/stamp edits use `bypass_change_detection()` on `PlacementConfig` so they don't trigger `rebuild_preview_on_config_change`; only a `kind` change should rebuild the preview.
- `adjust_camera_distance` (gfx/camera.rs) yields scroll when either `MeshSelectorOpen` or `ScriptPanelOpen` is set.
- Sun/ambient sliders not implemented (were optional).

End-to-end test recipe: add `example` plugin to a project via the pause menu → Shift+E → Tab, place a rock → K, scroll to `example/spinner`, look at the rock, Enter → it spins. Shift+S, exit, re-enter → still spins (hydrated from disk). Backspace → stops. Place a Point Light from the catalog; Delete it while looking at it.

### Gotcha: `cargo run` does NOT build plugin dylibs

Plugin crates under `plugins/` are workspace members the host does **not** depend on, so `cargo run` / `cargo build` build only `game_engine`. The loader then finds no `lib<name>.dylib` next to the exe and logs a single `warn!`, leaving the script registry empty with no visible explanation. **Always `cargo build --workspace` (or `cargo build -p <plugin>`) before running.**

This bit us once: the scripts panel showed only "No stamp" because no plugin dylib had ever been built in the main checkout. Compounding it, the pause menu's Add Plugin list comes from `discover_plugin_ids()`, which scans plugin **source** dirs — so you can add a plugin that was never compiled and it looks fine.

Mitigations now in place: `LoadedPluginIds` resource (actually-loaded ids, vs `ActiveProject.plugins` which is only declared); the scripts panel shows `empty_reason(...)` in red naming declared-but-unloaded plugins; `try_load_plugin` / `reload_plugin` log how many scripts each plugin registered.

Also fixed: both `reload_plugin` failure paths left `loaded_mtime` stale, so a plugin that failed to prepare or set up was retried **every frame**, copying a fresh `.loaded.N.dylib` each time and re-clearing its registry entries. Both paths now bank `new_mtime`.
