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
- `PlacementConfig` is `{ kind: PlacedKind, scale, rotation, stamp_script: Option<(plugin, script)> }`. A stamp armed from the **scripts panel** is sticky across mesh changes by design. A stamp armed by selecting a **catalog preset** is not: `scroll_mesh_selector` clears it when you step off that preset onto a plain mesh, otherwise every later rock would also get e.g. a `cafe-shell` binding.
- `PlacedPicker` (`editor/object_placement.rs`) is the one way to find "the placed entity under the crosshair": mesh raycast → root via `InteractableParent`, then ray/sphere fallback for mesh-less kinds. Use it rather than re-implementing raycasts — it's what makes lights deletable/attachable.
- Scripts panel is **K** (cafe-sim owns plain B). Enter = attach, Backspace = detach (Delete still deletes the entity). Status line shows bindings + `(awaiting plugin)`.
- Scale/stamp edits use `bypass_change_detection()` on `PlacementConfig` so they don't trigger `rebuild_preview_on_config_change`; only a `kind` change should rebuild the preview.
- `adjust_camera_distance` (gfx/camera.rs) yields scroll when either `MeshSelectorOpen` or `ScriptPanelOpen` is set.
- Scroll modifiers live in `editor/input.rs` (`ScrollMod` / `scroll_modifier`): none = cycle panel, Shift = scale, **Alt/Option = rotate**. Rotation is *not* on Ctrl — `freecam_movement` (gfx/camera.rs) descends on `ControlLeft`, so Ctrl+Scroll in freecam sank the camera while rotating. Every scroll consumer has its own `MessageReader`, so they must all test the modifier through this module or one notch fires two actions.
- Sun/ambient sliders not implemented (were optional).

End-to-end test recipe: add `example` plugin to a project via the pause menu → Shift+E → Tab, place a rock → K, scroll to `example/spinner`, look at the rock, Enter → it spins. Shift+S, exit, re-enter → still spins (hydrated from disk). Backspace → stops. Place a Point Light from the catalog; Delete it while looking at it.

### Gotcha: `cargo run` does NOT build plugin dylibs

Plugin crates under `plugins/` are workspace members the host does **not** depend on, so `cargo run` / `cargo build` build only `game_engine`. The loader then finds no `lib<name>.dylib` next to the exe and logs a single `warn!`, leaving the script registry empty with no visible explanation. **Always `cargo build --workspace` (or `cargo build -p <plugin>`) before running.**

This bit us once: the scripts panel showed only "No stamp" because no plugin dylib had ever been built in the main checkout. Compounding it, the pause menu's Add Plugin list comes from `discover_plugin_ids()`, which scans plugin **source** dirs — so you can add a plugin that was never compiled and it looks fine.

Mitigations now in place: `LoadedPluginIds` resource (actually-loaded ids, vs `ActiveProject.plugins` which is only declared); the scripts panel shows `empty_reason(...)` in red naming declared-but-unloaded plugins; `try_load_plugin` / `reload_plugin` log how many scripts each plugin registered.

Also fixed: both `reload_plugin` failure paths left `loaded_mtime` stale, so a plugin that failed to prepare or set up was retried **every frame**, copying a fresh `.loaded.N.dylib` each time and re-clearing its registry entries. Both paths now bank `new_mtime`.

## Post-commit review of the level save system — October 2026

A three-way review (persistence, script host, editor) after the v3 landing found real bugs. The **critical four are fixed**:

1. Streaming no longer unloads chunks present in either dirty set (`update_streaming_ring`). Before, Shift+S after walking >6 chunks from an edit rebuilt the chunk file from zero live entities / evicted (0.0) heightmap tiles — wiping objects and flattening terrain on disk. Dirty chunks stay resident until a save drains the dirty sets.
2. (same fix covers terrain and objects)
3. `PlacedObjectRegistry.next_id` is seeded by `seed_placed_ids` (`objects.rs`) from `WorldMetadata::next_entity_id` — a persisted high-water counter written on every save (`save_terrain` maxes it with the registry). This bumped **`WORLD_FORMAT_VERSION` 3 → 4**; v3 was never released and is *rejected* (old test worlds are deleted, not migrated — user's explicit call, a first implementation that scanned all `.objects` files at load was dropped as overcomplicated). Placement stays gated on `registry.ids_seeded` so `alloc_id` can't reuse an id from a not-yet-streamed chunk (which collided `obj_{id}_lightmap.png` and `PropValue::EntityRef`). `seed_placed_ids` keys on `TerrainWorld::is_added()`, NOT on `!ids_seeded` — a lingering old `TerrainWorld` during a world switch must not seed the next world's fresh registry.
4. `run_setup` failure paths now call `abort_failed_setup`: `mem::forget` the dylib (it was **dropped** → `dlclose` → dangling registry fn pointers + unmapped drop glue → segfault), remove the plugin's registry entries, despawn its orphaned `ScriptOwned` entities.

## Smooth toon shadow edges — October 2026

### Root cause: lightmaps were point-sampled

`lib.rs` sets `ImagePlugin::default_nearest()` (deliberate, for the game's pixel-art
look). `LightmapBuffer::into_image()` built images with `Image::new`, which leaves
`sampler: ImageSampler::Default` → resolves to that app-wide default → **Nearest**.
`ToonMaterial`'s `#[texture(100)] #[sampler(101)]` takes the sampler from the image, so
*every* lightmap — terrain CPU/GPU, object bake, both EXR load paths, all five call
sites funnel through `into_image()` — was point-sampled. `textureSample` returned a
piecewise-constant value, so band edges snapped to square texel blocks.

Nothing downstream could compensate, which is why three separate rounds of work barely
moved it: the shader's `fwidth` band AA measures a rate of change that is **zero inside
every texel**; the bake-side blur turns one hard edge into a staircase of flat blocks;
extra resolution just makes the blocks smaller. Tell that the baker always assumed
linear: `uv_unwrap`'s gutter + dilation exist *specifically* to feed bilinear taps at
triangle edges.

Fixed by setting `image.sampler = ImageSampler::linear()` explicitly in `into_image()`.
**Do not** flip the global default — other textures want nearest. Locked in by
`baked_lightmaps_carry_an_explicit_linear_sampler`.

Checked before making the change: object texels use `(px + 0.5) / res` (cell centres,
exactly what bilinear wants); terrain uses `tx / (res - 1)` (inclusive endpoints) against
the sampler's `(tx + 0.5) / res`, which agree at the chunk centre and differ by ≤½ texel
(~3 cm at 1024 over 63.75 m) at the edges. Adjacent chunks clamp to the same baked
boundary value, so linear filtering does **not** open chunk seams.

Still outstanding: `camera.rs:67` renders the scene to a fixed 1920×1080 target with
`ImageSampler::nearest()` ("for pixelated look") and blits it to the window, so on a
HiDPI display everything is re-quantised to that grid and point-upscaled. Deliberate art
direction; user wants to revisit it after the scene looks right at full res.

### Secondary: two problems that were real but not the cause

Both of these were genuine defects and both are fixed, but neither produced a visible
improvement on its own — the nearest sampler above was masking them. They only start
paying off now that the lightmap is interpolated.

1. **`toon_band` was a hard `floor(v * 5) / 5`.** A hard threshold on a bilinearly
   filtered texture traces a contour reconstructed at *texel* granularity, which then
   aliases against the pixel grid. Resolution only shrinks the steps — there is always
   exactly one texel of staircase, so it never goes away. Fixed for good by ramping the
   band boundary across `fwidth` of the banded coordinate: a ~1-screen-pixel transition
   independent of lightmap resolution, surface size, and distance. Flat regions have
   `fwidth ≈ 0` so the banded look is untouched. The `if (band <= TOON_FLOOR)` was a
   second hard threshold and is now a `smoothstep` cross-fade.
   - `toon_band`/`toon_shade`/`luminance` are **duplicated verbatim** in
     `toon_lighting.wgsl` and `terrain_toon.wgsl`. There is no shared WGSL import module
     in this project (no `#define_import_path` anywhere; shaders are referenced by asset
     path), so they must be kept in step by hand or terrain and objects disagree where
     they meet.
   - Validate WGSL edits without running the app: extract the functions into a standalone
     file with a dummy `@fragment` entry point and run them through `naga` 27 (the version
     in `Cargo.lock`) from a scratch crate. Catches syntax *and* the uniform-control-flow
     rule that `fwidth` depends on.
2. **Object lightmaps got no blur at all** while terrain got `blur_radius: 4`, so 16-ray
   AO variance landed straight on the band edges as speckle. `LightmapBuffer::blur_cells`
   now blurs objects, confined to each triangle's atlas cell (atlas neighbours are *not*
   surface neighbours — a plain blur smears unrelated triangles together) and masked to
   covered texels (the gutter is empty until dilation runs, so averaging it in would draw
   a dark rim around every triangle). Radius capped at `cell/4`.

## Object lightmap bake performance — October 2026

Terrain already bakes on the GPU (`baking/gpu.rs` + `lightmap_bake.wgsl`); only
**placed objects** are CPU-bound. They were also single-threaded, where the CPU
*terrain* path has always been `into_par_iter()` (`terrain.rs:66`) — objects just
never got it.

`baking/objects.rs` is now split into `rasterize_geometry` (serial, cheap, pure
interpolation → `Vec<Option<TexelGeom>>`) and `shade_geometry` (`par_iter`, where
all the rays are). `BakeStats` prints per-pass timings and coverage at the end of
each bake; per-sub-mesh detail is at `debug` level.

The split is deliberately the same shape the GPU path needs: `TexelGeom` is the
g-buffer it would upload, since the CPU object loop and the compute shader's
`main()` differ *only* in how a texel derives its world position and normal —
everything after that is the same already-written WGSL. See
`docs/GPU_OBJECT_BAKE_PLAN.md`.

Two things to remember if stage 2 happens: objects pass the **same origin twice**
to `cast_shadow_ray` (terrain lifts its DDA origin 0.5 up so the coarse march
can't re-hit the ground it left), and objects take **no bounce term**. Get either
wrong and objects shade differently from the terrain they stand on.

**Measuring immediately found that shading was not the bottleneck.** First real
bake: shade 0.46s, blur+dilate **2.66s**. Cause: `dilate_passes_for_atlas` sized
the pass count to fill the whole atlas gutter — 94 passes for a two-triangle mesh
in a 1024² atlas, ~900M neighbour tests and a GB of cloning. The premise was
wrong: the gutter is *separation* (dead space so one triangle's filtering can't
reach another's texels), and the shader only samples inside a triangle, where a
bilinear tap reaches **one** texel out. So dilation needs a small constant, which
is what `BakeConfig::dilate_passes` already was. Deleted the derived version.

Consequence: a GPU object bake is now poor value — ~0.46s of a ~0.6s bake, against
a permanent fourth copy of the lighting model (the CPU path can't be retired; WASM
has no compute).

The frame stall (bake ran inline in `Update`) is fixed **without** the GPU, via
`AsyncComputeTaskPool`:
- `collect_object_bake_jobs` (main world) — must be here: unwraps meshes mutating
  `Assets<Mesh>`, `unique_material` can *add* an asset, heightmap is a resource.
- `run_object_bake_jobs` (task) — touches **no** ECS. Rasterise → shade → blur →
  dilate → EXR.
- `apply_object_bake` (main world) — `images.add` + bind to the material.

Jobs carry a `MeshSnapshot` (mesh arrays, a few KB) not the rasterised g-buffer
(~40 MB per 1024² atlas), so rasterisation runs inside the task. One task for the
whole pass → a single `ObjectBakeTask` slot makes "already running" trivial, and
dropping it on `OnExit(AppState::InGame)` cancels the bake.

## Plugin ↔ level-entity ownership — October 2026

See `docs/PLUGIN_LEVEL_ENTITY_CONTRACT.md`. Short version: the engine cleans up
exactly two things — entities tagged `ScriptOwned`, and components a *script
factory* inserted (recorded by archetype diff across the `hydrate` call). Anything
a plugin **system** does to a level entity it didn't spawn is invisible to both and
survives unload forever. Not a UAF — unloaded dylibs are `mem::forget`'d so drop
glue stays mapped — but a leak, and the level stays visibly mutated.

- Asset defaults belong in the asset. `Cafe.gltf` now carries its own materials
  (brick `KHR_texture_transform` tiling + tinted floor); `ConvertToToon` clones the
  whole glTF `StandardMaterial`, so textures/`uv_transform` survive into the toon
  material and the shell looks right in the **editor** with cafe-sim stopped.
  `dress_cafe_shell` is deleted. Bevy 0.18 reads glTF samplers → `ImageAddressMode`
  and `KHR_texture_transform` → `StandardMaterial::uv_transform`; `baseColorFactor`
  is **linear** (`Color::linear_rgba`), and glTF defaults metallic/roughness to 1.0,
  so set `metallicFactor: 0.0` explicitly.
- A "have I processed this entity" latch goes in a `Local`, never a marker component
  on the level entity — the `PluginSchedule` is dropped on teardown so `Local`s die
  with it. Plugin-private *resources* are NOT swept by `run_teardown`.
- `Cafe.gltf` is hand-edited JSON now. Re-exporting from Blender will drop the
  materials unless they're authored there too. `Cafe.glb` sits alongside and is stale.
- Brick tiling lives in each wall material's `KHR_texture_transform.scale`: 35×6 on the
  long walls, 21×6 on the short ones (~1.7 tiles/m, square bricks on both). Integers so
  the pattern lands flush at the wall edges rather than wrapping mid-brick.

### Deferred findings (known, not yet fixed)

- **Archetype-diff sharing**: if two bindings' factories insert the same component, only the first records it; detaching the first strips it from under the second. A factory that *replaces* a pre-existing component records nothing.
- **No tracked-insert API** for plugin systems that must write to a level entity (rule 4 of the ownership contract), and **no detector** for violations. Deferred until a second real case appears; `dress_cafe_shell` was the only one and it's gone.
- **Object bake still runs inline in `Update`**, so Shift+B stalls the frame no matter how fast shading gets. Only the GPU path (async readback) fixes that — see `docs/GPU_OBJECT_BAKE_PLAN.md` stage 2, not started.
- **Uniform atlas cells waste a cuboid's budget**: a wall is 12 triangles, two of them big faces and ten slivers down its thickness, yet every cell is sized for the biggest. The cafe's front wall lands at ~2.7 texels/m where its floor gets 7.0. Fixing it properly means area-proportional cells (rectangle packing), not a uniform grid.
- **`PlacedPicker` picks through occluders** (`editor/object_placement.rs`): non-placed ray hits (terrain, walls) are skipped instead of terminating the ray — Delete/attach can act on hidden entities. Ray should stop at first hit of any kind.
- **Stale armed stamp**: `PlacementConfig.stamp_script` is never cleared on plugin unload; panel shows "No stamp" while the status line and placements still use the stale stamp, seeding bindings with *empty* props (schema lookup falls back to default).
- **Catalog highlight vs `config.kind` divergence** after registry-change rebuilds (`mesh_selector.rs` — `config.kind` only corrected when `manifest_changed`).
- **Edit mode + terrain-sculpt both active**: every LMB sculpt stroke also places an object (`handle_edit_mode_click` doesn't check the terrain-edit flag).
- Lower: dirty sets `drain()`ed before I/O (failed write loses dirtiness); `collect_plugin_level_data` not ordered after the Shift+S trigger (one-frame window); camera zoom eats a frame of leftover panel scroll (`camera.rs` reader not drained while inactive); `last_saved` always written 0; unresolvable `NeedsHydration` entities re-scanned every frame (perf only); reload-failure leaves plugin listed as loaded but inert (log-only); `AppliedPluginData` skips re-apply on same-world reload.

## Editor controls overhaul, phase 1 — October 2026

Multi-phase rework of the editor controls (user-driven; phases agreed up front).
Done in phase 1:

- **One bottom "Tool" tab** (`ui/tool_panel.rs`) replaced the Place/Terrain tabs;
  it dispatches to the active tool's settings body (`place.rs` / `terrain.rs`,
  headers stripped — the panel follows the tool, so their "switch to this tool"
  buttons were dead). Select shows nothing *by user decision* ("revisit later").
  `EditorTab::{Place,Terrain}` → `EditorTab::Tool`.
- **Selection/hover boxes are Select-tool-only**: `draw_selection_gizmos` and
  `update_hover` gained `run_if(in_state(EditorTool::Select))`; `clear_hover` runs
  on `OnExit(Select)` so a stale hover box can't linger. `Selection.hovered`'s only
  consumer is the gizmo drawer, so this was safe.
- **FlyLock** (`editor/camera.rs`): a button overlaid in the viewport tab's top-right
  corner locks the cursor and enables RMB-free WASD/QE flight (MacBook trackpad pain).
  Implementation: the viewport tab ORs it into `ViewportInput::look_active`, so the
  fly camera, `sync_look_cursor` and `pointer_free()` all work unchanged; `clicked_px`
  is suppressed while locked (frozen hidden cursor must not select/place); scroll uses
  `hovered || look` because the frozen egui pointer can sit over the overlay. While
  locked only a passive "Flying — Esc exits" text is painted — a live button could be
  re-clicked by a stray LMB at the frozen pointer position. Esc releases it *first* in
  `escape_cancels`' back-out chain; `release_fly_lock` also runs on `OnExit(Editor)`.
  The user explicitly chose a **button over double-click** (double-click's first click
  would fire tools/selection).
- `ActiveControls::rows(flying: bool)` takes the flag as an argument: `editor_ui`
  already holds `ResMut<FlyLock>` through `Panels`, and a `Res<FlyLock>` inside
  `ActiveControls` was a B0002 conflict — caught by
  `editor_systems_have_disjoint_parameters`, which is why that test exists.

Agreed for later phases (user-confirmed decisions):
- Terrain: move `brush_radius` out of `UserSettings` into `BrushSettings` (ABI bump),
  add intensity/radius/falloff settings + flatten/smooth modes.
- Select: hand-rolled XYZ translate arrows, rotation arcs while **Ctrl** held (Ctrl is
  free in Select; the Ctrl=lower conflict is Terrain-only). No third-party gizmo crate
  (bevy_egui 0.39 pinning). Drag state goes in its own resource, never in
  `ViewportInput` (rebuilt every egui pass); writes go through `PlacedEdited{Transform}`.

## Editor controls overhaul, phases 2+3 — October 2026

Both remaining phases landed together (terrain brush expansion + transform gizmo).

**Terrain brush (`terrain_edit.rs`)**
- `brush_radius` moved from `UserSettings` (engine-api) into `BrushSettings`;
  `ENGINE_SCRIPTS_ABI_VERSION` **5 → 6** — host and plugin dylibs must be rebuilt
  together (`cargo build --workspace`).
- `BrushSettings { mode, radius, strength, falloff }`. `BrushMode::{Sculpt, Flatten,
  Smooth}`. Falloff is "fraction of the radius that fades": weight 1 inside the flat
  core, smoothstep to 0 at the rim (`brush_weight`); default 1.0 ≈ the old full-radius
  fade. `strength` doubles as the per-second cap for flatten/smooth
  (`delta.clamp(±strength·weight·dt)`), so all modes share one meaning of strength.
- Flatten targets `TerrainBrushHit.flatten_target`, sampled under the cursor when the
  stroke begins (`!was_painting`) — a moving brush spreads one plateau instead of
  chasing its own output. Smooth averages a 3×3 from a **snapshot** of the brush
  region (+1 margin) taken before any write that frame. Unchanged tiles are skipped
  (no chunk dirtying when flatten has converged). The brush preview also draws a
  gizmo ring where the falloff starts.

**Transform gizmo (`editor/gizmo.rs`)**
- Select tool: world-axis XYZ arrows (translate); holding **Ctrl** swaps to rotation
  arcs live. Screen-constant sizing from camera distance. Hand-rolled math:
  `axis_ray_param` (closest-point between lines) for translate, plane-crossing +
  `signed_angle` for rotate. `EditorGizmoGroup` config group: line width 3,
  `depth_bias = -1` (handles draw through geometry).
- `GizmoInteraction { hovered, drag, cancel_requested }` resource. The update system
  is **chained before `select_on_click`**, which gained `.and(gizmo_pointer_free)` so
  a handle click never (de)selects. Drag writes `Transform` + `PlacedEdited` per
  frame (group member deltas stay correct); drag survives the ray dropping out
  (LMB-held is the authority); drag end on release keeps the transform; **Esc**
  sets `cancel_requested` (slotted into `escape_cancels` after fly lock, before
  grab) and the gizmo system restores the drag's start transform. `reset_gizmo` on
  `OnExit(Select)` so stale hover can't suppress clicks on re-entry.
- `ActiveControls::rows(flying, brush_mode)` — brush mode passed as an argument for
  the same B0002 reason as `flying` (Panels holds `ResMut<BrushSettings>`).
  Terrain hint rows are per-mode (`Ctx::TerrainSculpt/Flatten/Smooth` + shared
  `Ctx::Terrain` for `[ / ]`).

## Bevy 0.18 → 0.20 upgrade — October 2026

Plan + outcome record: `docs/BEVY_0_20_UPGRADE_PLAN.md` (§8 = Phase 1 / 0.19,
§9 = Phase 2+3 / 0.20). **All phases landed 2026-10-27** and were visually
verified by the user. The engine is on bevy 0.20.0 / avian3d 0.8.0 /
bevy_egui 0.43.1 / egui_dock 0.21.1 / egui 0.36.2 / wgpu+naga 30 / wesl 0.6.

- **The invariant for the editor chrome deps is "bevy_egui and egui_dock agree
  on the egui minor"**, not any specific version — that is what to check when
  bumping. The editor builds one root `egui::Ui` per pass
  (`Ui::new(ctx, id, UiBuilder::new())`, defaults to background layer +
  `content_rect`) and threads it through `Panel::top/bottom(..).show(&mut root)`
  and `DockArea::show_inside(&mut root)`. There is no `Context`-based panel API
  any more; `Window` still takes `&Context`.
- **Shaders are WESL.** `import bevy_pbr::render::forward_io::VertexOutput;`,
  module paths mirror crate file layout (`bevy_pbr::render::*`,
  `bevy_pbr::prepass::io`, `bevy_pbr::deferred::functions::deferred_output`),
  `@if(DEF)`/`@else` attach to exactly one declaration/import/statement (so a
  multi-statement conditional branch is written as repeated `@if` lines, the
  way `bevy_pbr/render/pbr.wesl` does), bind group index is
  `@group(constants::MATERIAL_BIND_GROUP)`. Plain WGSL with no directives still
  loads — `lightmap_bake.wgsl` stays `.wgsl` and the `naga` dev-dep must track
  wgpu's major (30 now) for its validation test. The authoritative reference
  for any shader question is the `.wesl` sources in the `bevy_pbr` crate under
  `~/.cargo/registry`, not memory.
- **Render passes are systems.** `post_process.rs`: `Core3d` schedule,
  `Core3dSystems::PostProcess`, `.after(tonemapping)`, pipeline specialized per
  view on `ExtractedView::target_format` via `Variants`/`Specializer` (model:
  `bevy_core_pipeline::fullscreen_material`). `baking/gpu.rs`: compute dispatch
  is a system in the surviving `RenderGraph` *schedule*,
  `RenderGraphSystems::Render`. `ExtractComponentPlugin::<T>` is a
  `bevy_render` alias that already names `RenderApp`; only the derive needs
  `#[extract_app(RenderApp)]`.
- **bevy_ui buttons (0.20 model):** `ui_widgets::Button` + picking
  `Hovered::default()` on the entity (Hovered is only maintained where it is
  present; nodes are pickable by default, no `Pickable` needed). Handlers poll
  `Query<&Marker, Added<Pressed>>` — fires on pointer-down, the same timing the
  old `Changed<Interaction>`/`Pressed` pattern had. `update_interactive_visuals`
  (engine `src/ui/widgets.rs`, duplicated in cafe-sim `ui.rs`) recolours only
  changed entities (`Changed<Hovered>` ∪ `Added<Pressed>` ∪
  `RemovedComponents<Pressed>`) because UI rendering is retained. The old
  `Interaction` enum and `ui::widget::Button` are deprecated aliases being
  removed after 0.20 — never reintroduce them.
- `Tonemapping::Linear` on the game camera, not `None` (0.20 made `None` a
  full passthrough that drops the negative-channel clamp ahead of the 5-band
  quantiser). `#![recursion_limit = "256"]` on `game_engine_lib` and
  `engine_api` is load-bearing (trait-solver overflow, scheduled to become a
  hard error). `ENGINE_SCRIPTS_ABI_VERSION` is 7.
- Behaviour deliberately *not* changed in the migration: button activation
  stays press-down (bevy's own `ui_widgets::Button` convention is release via
  `Activate`); `clear_just_pressed(MouseButton::Left)` calls in cafe-sim
  handlers were left in place.
- **The tree is clippy-clean as of 2026-10-27** (`cargo clippy --workspace
  --all-targets`, 0 warnings) — keep it that way. `too_many_arguments` and
  `type_complexity` are allowed **crate-wide** in `src/lib.rs` and
  `plugins/cafe-sim/src/lib.rs` with a comment explaining why (Bevy system
  signatures *are* their dependency lists); don't re-add per-function
  `#[allow]`s for them, 23 of those were removed. Two deliberate local allows
  remain, both documented at the site: `excessive_precision` on the f16
  packing test (the long literals are exact binary boundary values and the
  digits are the documentation) and `dead_code` on `storage/native.rs` +
  `StorageError` (the native half of a platform abstraction whose only live
  callers are wasm-gated — not actually dead).
- **`src/game/gfx/outline.rs` is an inert stub.** It was built on
  `bevy_mod_outline`; that dep is gone and every `OutlineVolume` insert is
  commented out, so `update_outline` (registered in `gfx/mod.rs`, runs every
  frame) walks entity hierarchies to no effect and `Shift+O` toggles a setting
  nothing reads. Flagged to the user 2026-10-27, left in place as scaffolding.
  Delete the module and its two registrations if no replacement is coming.
- **Process:** this worktree gets reset between sessions. Uncommitted work is
  destroyed — Phase 2 had to be redone once for exactly this reason. Commit
  each chunk as soon as it is green.

## WASM parity: toon shader crashed WebGPU on mesh placement — October 2026

Symptom: web build opened the editor and terrain editing worked, but placing a mesh
crashed to black with `[Invalid ShaderModule] … While validating fragment stage …
CreateRenderPipeline("pbr_opaque_mesh_pipeline")`. Terrain survived because its
material never defines `VERTEX_UVS_A`; the first placed mesh compiled the
`@if(VERTEX_UVS_A)` base-colour-texture branch of `toon_lighting.wesl` for the
first time.

Root cause: that branch tested `pbr_input.material.flags`. `pbr_input` is a `var`
initialised from `pbr_input_from_vertex_output(in, is_front, …)`, i.e. from
non-uniform fragment inputs, and WGSL's uniformity analysis keeps a `var`
dependent on its old value across partial field assignments. So the `if` was
non-uniform control flow and the `textureSampleBias` inside it is a hard error in
Tint: `'textureSampleBias' must only be called from uniform control flow …
builtin 'front' of 'fragment' may be non-uniform`. naga never flags it:
`naga/src/valid/analyzer.rs` has `const DISABLE_UNIFORMITY_REQ_FOR_FRAGMENT_STAGE:
bool = true;` (still so in naga 30), so **native can never catch this class of
bug** — the earlier note above about naga catching the `fwidth` uniformity rule is
wrong for fragment stages. Fix: `let material_flags = pbr_bindings::material.flags;`
and branch on that — exactly what bevy's `pbr_fragment.wesl` does.

Reproduced the Tint error and verified the fix without the app, using headless
Chrome over the DevTools protocol (plain `--dump-dom` snapshots the DOM before the
WebGPU promises settle, and `navigator.gpu` is undefined on `about:blank` — you
must navigate to a `file://` page first):

```
chrome --headless=new --user-data-dir=/tmp/p --remote-debugging-port=9333 \
       --enable-unsafe-webgpu --ignore-gpu-blocklist about:blank
# node ≥22 (built-in WebSocket): GET http://127.0.0.1:9333/json → page.webSocketDebuggerUrl,
# Page.navigate {url:"file:///tmp/blank.html"}, then Runtime.evaluate with
# awaitPromise:true on an async IIFE that does requestAdapter → requestDevice →
# createShaderModule({code}) → getCompilationInfo() and returns the messages.
```

Feed it a reduced WGSL that mirrors the suspicious structure (the full composed
WESL isn't dumpable from bevy). The real adapter works in headless mode on macOS;
the swiftshader flags do not.
