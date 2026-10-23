# Level Save System — Initial Design

Status: **approved 2026-10-21**, v1 scope. Covers terrain, placed meshes,
lights, plugin requirements, and script bindings between plugins and placed
entities. Open questions resolved in §7. Phases 0–2 (§6) are implemented;
Phase 3 (cafe-sim migration) is the next work.

## 1. What exists today

| Concern | Where | Persisted? |
|---|---|---|
| Terrain heights | `worlds/{id}/world.meta` + `chunks/chunk_X_Y.terrain` (rkyv, magic+version+crc frame) | Yes, Shift+S → `SaveTerrainMessage` |
| Placed meshes | `chunks/chunk_X_Y.objects` → `PlacedObjectRecord { id, asset_path, position, rotation, scale }` | Yes, same message, dirty-chunk based |
| Lightmaps | `chunks/chunk_X_Y_lightmap.png`, `objects/obj_{id}_lightmap.png` | Yes, on bake |
| Sun / ambient | Hard-coded in `game::world::setup` | **No** |
| Point / spot lights | Only spawned by plugin code (cafe-sim) | **No** |
| Which plugins a project uses | `projects/{id}/project.toml` | Yes (per project, not per world) |
| Plugin ↔ entity association | None. Plugins spawn their own `ScriptOwned` entities | **No** |
| Plugin-private game state | cafe-sim `projects/cafe-sim/saves/slotN.bin` | Plugin-private, bypasses engine |

Constraints the design must respect:

- Plugins are dylibs (`DynamicPlugin::setup/teardown`). Component `TypeId`s
  declared inside a dylib change on every rebuild, so the host can never name
  a plugin component. Old dylib pages are `mem::forget`'d (never unmapped), so
  calling drop fns of old components is safe.
- `engine-api` types are shared across the boundary with stable `TypeId`s
  (`AutoCollider` already proves this works). Any new shared type lives there.
- Streaming is chunk-based and async. Chunk entities can spawn before, after,
  or between plugin loads / hot-reloads / pause-menu add/remove.
- wasm has no dylibs. Everything plugin-related must degrade to a no-op there.
- The codebase prefers rkyv for binary data and avoids serde.

## 2. Core decisions

1. **A level is the existing world directory.** `worlds/{id}/` stays the unit
   of save; project manifests keep referencing worlds by id. No new
   top-level container.
2. **One record type per placed thing.** `.objects` files become a list of
   `PlacedEntityRecord`s with a `kind` (mesh, point light, spot light, empty
   anchor) and a list of `scripts`. Lights stream with their chunk like meshes.
3. **Global environment lives in `world.meta`.** Sun, ambient, required
   plugins, and per-plugin level blobs are added to `WorldMetadata` (format
   version 3). One file, one writer, already loaded by bootstrap.
4. **Script bindings are data, hydration is a host job.** A placed entity
   carries `ScriptBindings` (engine-api, stable). Plugins register named
   *script factories*. The host calls the factory to insert plugin components
   onto the entity, records which `ComponentId`s were added, and removes them
   on plugin unload/reload. Plugins keep the ergonomic "my component is on the
   mesh entity" model that cafe-sim already uses.
5. **Props are a typed property bag, not opaque bytes.** The editor can show
   and later edit them; plugins read them in the factory.
6. **All rkyv.** No new serialization dependency; reuse the existing frame
   helpers and the per-version parse pattern.

## 3. On-disk format (v3)

```
worlds/{id}/
  world.meta                   # v3: terrain + chunk index + environment + plugins
  chunks/chunk_X_Y.terrain     # unchanged
  chunks/chunk_X_Y.objects     # v3: PlacedEntityRecord list (meshes, lights, scripts)
  chunks/chunk_X_Y_lightmap.png
  objects/obj_{id}_lightmap.png
```

### 3.1 `world.meta` additions

```rust
pub struct WorldMetadata {
    // existing: version, world_id, terrain_config, chunks_x, chunks_y, chunk_index, last_saved
    pub environment: EnvironmentData,
    /// Plugin ids this level's bindings reference. Diagnostic only — the
    /// project manifest remains the single authority on what loads, and a
    /// missing plugin is warned about, never auto-added. See §4.8.
    pub required_plugins: Vec<String>,
    /// Opaque per-plugin level data (grid config, catalog version, etc.).
    pub plugin_data: Vec<PluginLevelData>,
}

pub struct EnvironmentData {
    pub sun_rotation: [f32; 4],      // quaternion xyzw; drives BakedSun + DirectionalLight
    pub sun_color: [f32; 3],
    pub sun_illuminance: f32,
    pub ambient_color: [f32; 3],
    pub ambient_brightness: f32,
}

pub struct PluginLevelData {
    pub plugin: String,
    pub data: Vec<u8>,
}
```

### 3.2 `.objects` record

```rust
pub struct PlacedEntityRecord {
    pub id: u64,                  // stable per world; already allocated by PlacedObjectRegistry
    pub name: String,             // editor label, may be empty
    pub position: [f32; 3],
    pub rotation: [f32; 4],
    pub scale: [f32; 3],
    pub kind: PlacedKind,
    pub scripts: Vec<ScriptBindingRecord>,
}

pub enum PlacedKind {
    Mesh { asset_path: String, collider: ColliderMode },   // ColliderMode: Trimesh | None
    PointLight { color: [f32; 3], intensity: f32, range: f32, shadows: bool },
    SpotLight  { color: [f32; 3], intensity: f32, range: f32, inner: f32, outer: f32, shadows: bool },
    Empty,                                                  // script-only anchor / spawn point
}

pub struct ScriptBindingRecord {
    pub plugin: String,           // plugin id, e.g. "cafe-sim"
    pub script: String,           // factory name within the plugin, e.g. "coffee_machine"
    pub props: Vec<(String, PropValue)>,
}

pub enum PropValue {
    Bool(bool), Int(i64), Float(f32), Str(String),
    Vec3([f32; 3]), Color([f32; 4]),
    EntityRef(u64),               // another PlacedEntityRecord.id in this world
}
```

### 3.3 Versioning and migration

- `FORMAT_VERSION` (terrain/meta) and `CHUNK_OBJECTS_VERSION` both go 2 → 3.
- Parsers dispatch on the version in the frame header: v2 payloads are read
  with the v2 archived types and upgraded in memory (`kind = Mesh`,
  `scripts = []`, `environment = defaults`). Existing `cafe` and `default`
  worlds load unchanged and are rewritten as v3 on next save.
- Keep v2 structs in a `legacy` module alongside the current ones so the
  upgrade path stays compilable and testable.

## 4. Runtime model

### 4.1 Shared types in `engine-api`

```rust
/// Authored bindings. Host component on every placed entity that has scripts.
#[derive(Component)]
pub struct ScriptBindings(pub Vec<ScriptBinding>);     // mirrors ScriptBindingRecord

/// Registered by plugins in `setup`.
pub struct ScriptSchema {
    pub name: &'static str,
    pub props: &'static [PropDef],                      // name + PropValue default
    pub default_mesh: Option<&'static str>,             // lets the editor offer "presets"
}

pub type ScriptHydrateFn = fn(&mut EntityWorldMut, &ScriptBinding);

pub struct LevelDataHooks {
    pub save: fn(&mut World) -> Option<Vec<u8>>,
    pub load: fn(&mut World, &[u8]),
}

#[derive(Resource, Default)]
pub struct ScriptRegistry { /* (plugin id, script name) -> (schema, hydrate fn); plugin id -> hooks */ }

impl ScriptRegistry {
    pub fn register_script(&mut self, plugin: &'static str, schema: ScriptSchema, hydrate: ScriptHydrateFn);
    pub fn register_level_hooks(&mut self, plugin: &'static str, hooks: LevelDataHooks);
}
```

Plain `fn` pointers rather than boxed closures: nothing to capture (plugins use
resources), and nothing with a vtable to keep alive. Bump
`ENGINE_SCRIPTS_ABI_VERSION` when this lands.

Plugin side, e.g. cafe-sim:

```rust
fn setup(&self, world: &mut World, schedule: &mut Schedule) {
    let mut reg = world.resource_mut::<ScriptRegistry>();
    reg.register_script(PLUGIN_NAME, ScriptSchema {
        name: "coffee_machine",
        props: &[],
        default_mesh: Some("cafe-sim/machine.gltf#Scene0"),
    }, |e, _b| { e.insert((CoffeeMachine, AutoCollider)); });
    // ...
}
```

### 4.2 Hydration (host, `script_host.rs`)

Host-only component, not shared with plugins:

```rust
#[derive(Component, Default)]
struct HydratedScripts(Vec<HydratedScript>);
struct HydratedScript { plugin: String, script: String, component_ids: Vec<ComponentId> }
```

Exclusive system `hydrate_script_bindings`, run right after `sync_plugins`
inside `run_scripts` (so a reload in the same frame is handled before
hydration):

1. Query entities with `ScriptBindings` and `NeedsHydration` marker
   (inserted at spawn and whenever bindings change or the set of loaded
   plugins changes).
2. For each binding not already present in `HydratedScripts`: look up
   `(plugin, script)` in `ScriptRegistry`. Missing plugin → skip, leave marker,
   log once per (plugin, script). Found → snapshot
   `entity.archetype().components()`, call the factory, diff the component set,
   push a `HydratedScript` with the added ids.
3. Remove `NeedsHydration` once every binding is either hydrated or belongs to
   a plugin that is not in the active project.

Dehydration, called from `unload_one` and `reload_plugin` *before*
`run_teardown`: for every `HydratedScripts` entry whose plugin matches, call
`EntityWorldMut::remove_by_ids(&component_ids)`, drop the entry, reinsert
`NeedsHydration`. Then remove the plugin's registry entries. After a successful
reload the new dylib re-registers factories and hydration re-runs with the new
`ComponentId`s.

Why diff instead of a plugin-supplied "detach" fn: it is automatic, cannot
drift from the factory, and also catches host components the factory inserted
(`AutoCollider`, `Interactable`), which is the correct thing to undo. Caveat to
document for plugin authors: anything the factory *spawns* (children, UI) must
still be `ScriptOwned` so teardown sweeps it.

Both ECS calls used here exist in bevy_ecs 0.18 (`EntityWorldMut::archetype`,
`remove_by_id` / `remove_by_ids`).

### 4.3 Plugin level data

- On save: exclusive `collect_plugin_level_data` runs before `save_terrain`,
  calls each loaded plugin's `save` hook, writes results into
  `TerrainWorld.metadata.plugin_data`. `save_terrain` then snapshots metadata
  as it does now. `save_terrain` must always write `world.meta`, even when no
  chunks are dirty (environment/plugin edits alone must persist).
- On load: `apply_plugin_level_data` runs when both `TerrainWorld` exists and a
  plugin is loaded, tracked by a `HashSet<plugin id>` reset in
  `world::destroy` and `switch_world`. Handles both orderings (plugins first or
  world first) and re-applies after hot-reload.

### 4.4 Environment

- `world::setup` keeps spawning the sun entity with defaults; a new
  `apply_environment` system with `run_if(resource_added::<TerrainWorld>)`
  overwrites sun transform/light and `GlobalAmbientLight` from
  `metadata.environment`. The bake already reads `BakedSun`'s transform, so
  baked lighting follows the saved sun automatically.
- Editing the sun writes back into `TerrainWorld.metadata.environment` and
  sets a `meta_dirty` flag (or simply rely on always-write on save).

### 4.5 Spawning placed entities

`spawn_chunk_objects` / `spawn_placed_object` match on `kind`:

- `Mesh` → existing path (`SceneRoot`, `ConvertToToon`, collider per
  `ColliderMode`).
- `PointLight` / `SpotLight` → light component + a small editor-only gizmo
  mesh child (reuse the yellow sun cone approach), `NotShadowCaster`. Gizmo
  visibility follows `EditorMode`.
- `Empty` → transform only.

All kinds get `GameEntity`, `PlacedObject`, and `ScriptBindings` +
`NeedsHydration` when `scripts` is non-empty. Deletion, dirty tracking, and
`despawn_chunk_objects` are unchanged because they key off `PlacedObject`.

### 4.6 Save trigger

Rename `SaveTerrainMessage` → `SaveLevelMessage` (Shift+S unchanged). Listeners
in order: `collect_plugin_level_data` → `save_terrain` (writes chunks + meta) →
`save_placed_objects`. Keep `world.meta` single-writer (terrain task).

### 4.7 wasm

`ScriptRegistry` exists but stays empty; entities keep `NeedsHydration`
forever at zero cost. Format changes ride the existing OPFS paths unchanged.

### 4.8 Missing plugins — warn, never auto-add

A level may reference plugins the active project does not load. That is a
reported condition, not a repaired one; `project.toml` stays the only thing
that decides what loads, and the user adds a plugin via the pause menu if they
want it.

- On `TerrainWorld` insertion, diff `metadata.required_plugins` against
  `ActiveProject.plugins` and emit one warning naming the missing ids.
- `hydrate_script_bindings` skips bindings whose `(plugin, script)` is absent
  from the registry, logging once per pair, and leaves `NeedsHydration` in
  place. Adding the plugin later through the pause menu therefore hydrates the
  existing entities live, with no reload of the world.
- **Unhydrated bindings must survive a save round-trip.** Otherwise opening a
  level without its plugin and pressing Shift+S silently strips every binding.
  This falls out of the design as long as `save_placed_objects` rebuilds
  records from the `ScriptBindings` host component — which is present whether
  or not hydration ran — and never from the plugin components themselves.
  Worth an explicit regression test in Phase 1.

## 5. Editor workflow (v1)

- **Lights**: mesh selector grows two built-in entries, "Point Light" and
  "Spot Light". LMB places a light record with sensible defaults. Delete works
  as for meshes.
- **Scripts panel** (`editor/script_binding.rs`): a key (proposed `B`) in edit
  mode opens a panel listing `ScriptRegistry` entries as `plugin/script`.
  Scroll to select; Enter attaches to the hovered `PlacedObject` with schema
  defaults; Delete detaches. Marks the chunk dirty.
- **Stamp mode**: with a script selected and no hovered object, LMB placement
  attaches the binding to the new mesh. If the schema has `default_mesh`, the
  selector offers it as a preset that picks mesh + binding together. This is
  how cafe-sim's catalog maps onto the engine editor.
- **Sun / ambient**: reuse the pause-menu `SliderSetting` sliders for sun yaw,
  pitch, and ambient brightness. Optional in v1.
- Property editing UI is v2. v1 writes defaults only.

## 6. Phased plan

**Phase 0 — formats** — **done** (11 lib tests passing)
- `objects.rs`: `PlacedEntityRecord` / `PlacedKind` / `ColliderMode` /
  `ScriptBindingRecord` / `ScriptProp` / `PropValue`; `ChunkObjectsFile.objects`
  → `.entities`; `legacy` module reads v2 and upgrades.
- `terrain/storage.rs`: `EnvironmentData`, `required_plugins`, `plugin_data`;
  `FORMAT_VERSION` split into `CHUNK_FORMAT_VERSION` (still 2 — the chunk
  payload did not change) and `WORLD_FORMAT_VERSION` (3); `legacy::WorldMetadataV2`.
- `world/mod.rs`: `apply_environment` (sun + ambient from metadata),
  `warn_missing_plugins`, `SunDebugMarker`.
- `terrain/save.rs`: an empty dirty set no longer early-returns, so
  environment / plugin edits persist on their own.
- `spawn_placed_object` + `spawn_chunk_objects`'s duplicated spawn logic
  collapsed into one `spawn_entity_record`; dirty-marking moved to the editor
  caller so streaming can't dirty a chunk by loading it.

Deviations from the design above, both deliberate:
- Light markers are **immediate-mode `Gizmos`** (`draw_light_gizmos`), not
  child mesh entities. The baker walks mesh descendants of every
  `PlacedObject` (`collect_shadow_hulls`, `bake_object_lightmaps`), so a gizmo
  *mesh* would have cast baked shadows and consumed a lightmap atlas. Gizmos
  are invisible to those queries, so no baking changes were needed at all.
- Props serialize as `Vec<ScriptProp { key, value }>` rather than
  `Vec<(String, PropValue)>` — a named struct reads better and avoids rkyv
  tuple-in-vec friction.

**Phase 1 — registry + hydration** — **done** (22 workspace tests passing)
- `crates/engine-api/src/script.rs`: `ScriptBinding`, `ScriptProp`,
  `PropValue`, `ScriptBindings`, `PropDef`/`PropDefault`, `ScriptSchema`,
  `ScriptHydrateFn`, `LevelDataHooks`, `ScriptRegistry`.
  `ENGINE_SCRIPTS_ABI_VERSION` 2 → 3; engine-api gained an rkyv dependency.
- `script_host.rs`: `NeedsHydration`, `HydratedScripts`,
  `hydrate_script_bindings` (archetype diff), `dehydrate_plugin` (wired into
  both `unload_one` and `reload_plugin`, before teardown),
  `apply_plugin_level_data`, `collect_plugin_level_data`.
- `plugins/example`: registers a `spinner` script plus level-data hooks.

Decisions made during implementation:
- `ScriptBinding` is **one type**, not a serialized record plus a runtime
  mirror. It lives in `engine-api` with rkyv derives and the engine's
  `.objects` format uses it verbatim, so the two shapes cannot drift. The
  `ScriptBindingRecord` name from §3.2 is gone — it is just `ScriptBinding`.
- Bindings live in the `ScriptBindings` component, **not** on `PlacedObject`.
  `PlacedObject::to_record` takes them as an argument, keeping one source of
  truth that the Phase 2 editor can mutate directly.
- `required_plugins` is written as a **union** with the stored list, never a
  replacement. Only loaded chunks have entities, so replacing would silently
  drop a dependency whose bindings sit in a streamed-out chunk. The cost is a
  possible stale entry (a spurious warning) after deleting the last binding
  for a plugin.
- An unresolvable binding warns **once** per `(plugin, script)` via
  `ReportedMissingScripts`, distinguishing "plugin not loaded" from "plugin
  loaded but exports no such script". Cleared per-plugin on unload.
- A panicking factory is caught, and the partial archetype diff is still
  recorded so unload cleans up whatever it managed to insert.

Not yet exercisable at runtime: there is no way to *author* a binding until
Phase 2, so the `spinner` script is covered by unit tests only. The level-data
hooks are fully exercisable now (Shift+S, then reload the world).

**Phase 2 — editor** — **done** (sun sliders deferred; everything else landed)
- `mesh_selector.rs`: `CatalogEntry { name, kind: PlacedKind, preset_script }`.
  The strip merges manifest meshes, two built-in lights, and one preset per
  plugin script with a `default_mesh`. Rebuilt on registry change; selection
  is index-based (`MeshButton::index`). `PlacementConfig` is now
  `{ kind: PlacedKind, scale, stamp_script }`.
- `object_placement.rs`: kind-aware preview (immediate-mode gizmo for lights,
  lifted 2.5 units off the terrain hit), stamp-on-place with schema defaults,
  and `PlacedPicker` — a `SystemParam` that raycasts meshes first and falls
  back to a ray/sphere test for mesh-less kinds, so Delete and script
  attach work on lights.
- `script_binding.rs` (new): **K** toggles the scripts panel. Cards are
  `plugin/script` plus a leading "No stamp". Scroll arms stamp mode;
  **Enter** attaches to the picked entity, **Backspace** detaches (on
  "No stamp": detaches all). A per-frame status line shows the picked
  entity's bindings and `(awaiting plugin)` when unhydrated.
- `script_host::detach_binding`: strips one binding's hydrated components,
  queued from the editor via `commands.queue`.

Deviations from §5, all deliberate:
- Panel key is **K**, not B — cafe-sim already binds plain B to its shop.
- Detach is **Backspace**, not Delete — Delete already deletes the whole
  placed entity in edit mode, and overloading it modally was confusing.
- Stamp mode is **sticky** across mesh changes: selecting a preset arms it,
  selecting a plain mesh leaves it, only the scripts panel clears it. The
  status line always shows the armed stamp so this isn't hidden state.
- Sun / ambient sliders were listed as optional and are **not** in; the
  environment round-trips through `world.meta` but is only editable by hand.

**Phase 3 — cafe-sim as proving ground**
- Register `countertop`, `coffee_machine`, `garbage_can`, `decoration`,
  `cafe_shell` (material override for Cafe.gltf) scripts; cafe lights and the
  shell become placed entities in the `cafe` world.
- Grid config moves to plugin level data. Slot saves shrink to runtime state
  (balance, day, staff) and reference pieces by `PlacedObject.id` instead of
  re-spawning them.

## 7. Decisions (settled 2026-10-21)

1. **Script components go directly on the placed entity**, tracked by
   `ComponentId` diffing (§4.2) — not a `ScriptOwned` child entity per
   binding. Keeps plugin queries and raycast hits pointing at the thing the
   user actually placed, which is what cafe-sim already assumes.
2. **Props are a typed property bag** (`PropValue`), not opaque
   plugin-serialized bytes, so the editor can display and later edit them.
3. **`.objects` keeps its extension**, now carrying the generalised
   `PlacedEntityRecord`. Version dispatch in the frame header handles the
   v2 → v3 upgrade, so no file renaming or migration pass is needed.
4. **`required_plugins` warns only** — missing plugins are never auto-added
   to the project manifest. See §4.8.
