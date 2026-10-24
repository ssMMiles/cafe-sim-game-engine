# Editor Controls Refinement — Implementation Plan

Three changes, plus a follow-on:

1. **Contextual controls HUD** — show only the controls that apply right now.
2. **Rotation + grab** — rotate during placement, and move/rotate/rescale
   already-placed entities.
3. **Plugin lifecycle follows `EditorMode`** — gameplay schedules stop running
   in editor mode.
4. **Cafe shell becomes a level entity** — enabled by (3), which otherwise
   removes the shell from editor mode entirely.

Implement in order 1 → 2 → 3 → 4. Steps 1 and 2 are self-contained; step 3 is
the only one touching the plugin ABI; step 4 depends on step 3.

Decisions already settled (do not relitigate):

- Switching back to the editor **resets** the running gameplay session. Next
  entry to gameplay starts from the plugin's own main menu.
- Entering a world starts in `EditorMode::Editor`, as today. No per-project
  start mode.
- Scroll modifiers: plain = cycle panel, Shift = scale, Alt = rotate.
  `R` = grab. Controls panel sits top-left under the FPS counter.

---

## Step 1 — Contextual controls HUD

New file `src/editor/controls.rs`. Nothing else in the editor changes.

### Context

```rust
#[derive(PartialEq, Eq, Clone, Copy, Default)]
pub struct ControlContext {
    edit_mode: bool,
    terrain_edit: bool,
    catalog_open: bool,
    scripts_panel_open: bool,
    carrying: bool,      // wired up in step 2; false until then
}
```

Derived each frame from `UserSettings`, `MeshSelectorOpen`, `ScriptPanelOpen`,
and (step 2) `GrabbedEntity`.

### Table

One `static` table of rows, each tagged with the contexts it belongs to:

```rust
enum Ctx { Base, Edit, Terrain, Catalog, Scripts, Carrying }
struct ControlRow { ctx: Ctx, keys: &'static str, action: &'static str }
```

Rows:

| Ctx | Keys | Action |
|---|---|---|
| Base | `F1` | Editor / Gameplay |
| Base | `Shift+E` | Edit mode |
| Base | `Shift+G` | Terrain edit |
| Base | `Shift+S` | Save world |
| Base | `Shift+B` | Bake lightmaps |
| Base | `Shift+R` | Reload plugins |
| Edit | `Tab` | Placement catalog |
| Edit | `K` | Scripts panel |
| Edit | `LMB` | Place |
| Edit | `R` | Grab object |
| Edit | `Delete` | Delete object |
| Edit | `Shift+Scroll` | Scale |
| Edit | `Alt+Scroll` | Rotate |
| Carrying | `LMB` | Drop here |
| Carrying | `R` | Cancel (restore) |
| Carrying | `Delete` | Destroy |
| Carrying | `Shift/Alt+Scroll` | Scale / Rotate |
| Catalog | `Scroll` | Cycle meshes |
| Scripts | `Scroll` | Cycle scripts |
| Scripts | `Enter` / `Backspace` | Attach / detach |
| Terrain | `LMB` / `RMB` | Raise / lower |
| Terrain | `[` / `]` | Brush size |

### Systems

- `setup_controls_panel` — spawn under `anchors.top_left`, styled like the
  existing HUD panels (`theme.bg_panel`, `size_label`, `pad_md`). Registered
  alongside `setup_debug_hud` on `OnEnter(AppState::InGame)` (gated to editor
  mode) and `OnEnter(EditorMode::Editor)`.
- `update_controls_panel` — `Local<Option<ControlContext>>`; when the computed
  context differs, `despawn_related::<Children>()` and respawn the matching
  rows. Same rebuild pattern as `update_keybinds_display`.
- `cleanup_controls_panel` — added to the existing `OnExit(EditorMode::Editor)`
  tuple in `src/editor/mod.rs`.

Register `update_controls_panel` in `EditorSet::InGame` so it keeps working
while paused.

Note: the top-right settings panel (`update_keybinds_display`) still renders a
`[key]` column that now duplicates several of these rows. Leave it for now;
trimming it to name + value is a separate cosmetic change.

---

## Step 2 — Placement rotation and grab

### 2a. Shared scroll helper

All three scroll consumers (`adjust_placement_scale`, `scroll_mesh_selector`,
`scroll_script_panel`) re-implement the same event summing and modifier
filtering, and the panel ones only skip on Shift — they would also fire on
Alt+Scroll once rotation exists.

New `src/editor/input.rs`:

```rust
pub enum ScrollMod { None, Shift, Alt }
pub fn scroll_modifier(input: &ButtonInput<KeyCode>) -> ScrollMod;
pub fn scroll_delta(events: &mut MessageReader<MouseWheel>) -> f32;
```

`scroll_delta` normalises `MouseScrollUnit::Pixel` (keep each call site's
existing divisor: 53.0 for placement, 40.0 for panels — pass it as an arg).
Each system reads its own `MessageReader`, so there is no consumption conflict;
they just each bail unless `scroll_modifier` matches what they want.

Update all three systems to use it, and make the panel systems require
`ScrollMod::None`.

### 2b. Rotation during placement

- `PlacementConfig` (`src/game/world/objects.rs:365`) gains
  `pub rotation: Quat`, defaulting to `Quat::IDENTITY`.
- `handle_edit_mode_click` builds its `Transform` with
  `.with_rotation(config.rotation)`.
- `setup_placement_preview` applies the rotation to the preview transform.
- New `adjust_placement_rotation` system in `object_placement.rs`: on
  `ScrollMod::Alt`, yaw by 15° per notch
  (`Quat::from_rotation_y(delta * 15f32.to_radians())`, pre-multiplied into
  `config.rotation`). Mutate through `bypass_change_detection()` and write
  straight to the preview transform, exactly as `adjust_placement_scale` does,
  so the preview scene is not reloaded on every notch.
- Register in `EditorSet::Active` next to `adjust_placement_scale`.

Rotation applies to every kind. It is meaningless for point lights but
harmless, and it is load-bearing for spot lights, whose cone direction comes
from the transform.

No persistence change: `PlacedEntityRecord` already stores a quaternion
(`src/game/world/objects.rs:63`) and `spawn_entity_record` already applies it.

### 2c. Grab

New file `src/editor/grab.rs`.

```rust
#[derive(Resource, Default)]
pub struct GrabbedEntity(pub Option<GrabPayload>);

pub struct GrabPayload {
    record: PlacedEntityRecord,   // id, name, kind, transform, scripts
    origin_chunk: (i32, i32),
    previous_config: PlacementConfig,
}
```

**`grab_hovered_object`** — `R` pressed, edit mode on, nothing already
carried:

1. `PlacedPicker::pick()` → entity; read `PlacedObject`, `Transform`, and
   `Option<&ScriptBindings>`.
2. Rebuild a `PlacedEntityRecord` from those (preserving `id`, `name`, and the
   full binding list with props).
3. Despawn the entity, `registry.by_id.remove(&id)`,
   `registry.dirty_chunks.insert(origin_chunk)`.
4. Stash the current `PlacementConfig` in the payload, then overwrite it from
   the record (kind, scale, rotation) so the preview shows the real object.
   This is a *real* change, so `rebuild_preview_on_config_change` fires and
   rebuilds the preview mesh — which is what we want here.

**`place_grabbed_object`** — LMB while carrying. Must run **before**
`handle_edit_mode_click` and `clear_just_pressed(MouseButton::Left)` so the
same click does not also place a fresh object. Takes the preview transform,
writes it into the record, computes the new chunk, calls `spawn_entity_record`
with the **original id** (never `alloc_id`), marks both old and new chunks
dirty, restores `previous_config`, clears the resource.

**`cancel_grab`** — `R` while carrying: respawn the record at its original
transform in its original chunk, restore config, clear.

**`destroy_grabbed_object`** — `Delete` while carrying: drop the payload
without respawning; the origin chunk is already dirty.

**`release_grab_on_exit`** — restores the carried object at its original
transform. Runs on `OnExit(EditorMode::Editor)` (added to the existing cleanup
tuple) *and* from a guard inside `cleanup_placement_preview` when `edit_mode`
goes false. Without this, leaving edit mode while carrying silently destroys
the object.

Scale and rotate work while carrying because they mutate `PlacementConfig`,
which the place step reads back.

**Known limitation to state in the module docs:** position follows the
crosshair while carrying, so there is no "rotate in place without moving".
Acceptable for now; a proper transform gizmo is the eventual answer.

Register `GrabbedEntity`, add the systems to `EditorSet::Active` gated on
`edit_mode_active`, and wire `carrying` into the step 1 `ControlContext`.

---

## Step 3 — Plugin lifecycle follows `EditorMode`

The root cause: `run_scripts` (`src/script_host.rs:677`) ticks every loaded
plugin's schedule whenever `AppState::InGame`, regardless of editor mode. So
cafe-sim's Escape handler fights the engine pause menu, its Tab fights the
placement catalog, and its cursor manager fights the editor's cursor lock via
`InputLock`.

Plugins cannot simply be unloaded in editor mode, because `setup` does two
jobs at once: it registers scripts/schemas/level-hooks that the editor needs
(scripts panel, catalog presets, level-data save), *and* it bootstraps
gameplay (resources, owned entities, schedule). Split those two jobs.

### 3a. ABI change (`crates/engine-api/src/lib.rs`)

```rust
pub trait DynamicPlugin {
    /// Called on every load and hot-reload, in BOTH modes. Register scripts
    /// and level hooks here. Must not spawn entities or insert gameplay
    /// resources — nothing here is undone until the plugin fully unloads.
    fn register(&self, _world: &mut World, _registry: &mut ScriptRegistry) {}

    /// Called when the plugin becomes active (entering gameplay mode).
    fn setup(&self, world: &mut World, schedule: &mut Schedule);

    /// Called when the plugin becomes inactive, and before a hot-reload of an
    /// active plugin.
    fn teardown(&self, _world: &mut World) {}
}
```

`register` takes `&mut ScriptRegistry` directly rather than `&mut World` so the
"registration only" contract is structural, not just documented. (If borrow
friction appears — e.g. a plugin wanting `AssetServer` during registration —
fall back to `&mut World` and enforce the contract in docs.)

- `add!` macro gains a `plugin_register` FFI entry point, same
  `catch_unwind` + error-code shape as the others.
- `register` has a default no-op body so `example2` (which registers nothing)
  compiles unchanged.
- `ENGINE_SCRIPTS_ABI_VERSION` → **4**.
- The macro's `plugin_teardown` shim keeps its `ScriptOwned` auto-despawn. That
  now runs on every deactivation, which is exactly the reset behaviour we want.

### 3b. Host changes (`src/script_host.rs`)

Add `active: bool` to `LoadedPlugin`.

| Operation | Sequence |
|---|---|
| `try_load_plugin` | prepare → `plugin_register` → if mode is Gameplay, `activate` |
| `activate(id)` | `plugin_setup` → `Schedules::insert` → clear this plugin's `AppliedPluginData` entry (so level data re-applies *after* setup overwrites resources with defaults) → `active = true` |
| `deactivate(id)` | `plugin_teardown` → `Schedules::remove` → drop `AppliedPluginData` entry → release its `InputLock` holder → `active = false` |
| `reload_plugin` | `dehydrate_plugin` → if active, `teardown` → `plugin_register` → if active, `setup` |
| `unload_one` | if active, `deactivate` → `ScriptRegistry::remove_plugin` → forget library |

- `run_scripts`: run only `active` plugins' schedules. `sync_plugins` and
  `hydrate_script_bindings` keep running in both modes.
- **Hydration is not touched by activation.** Bindings hydrate and stay
  hydrated in both modes; only reload dehydrates (components hold fn pointers
  into the outgoing dylib). This keeps placed plugin objects correct in the
  editor and is why the shell in step 4 works.
- `apply_plugin_level_data` / `collect_plugin_level_data` iterate **active**
  plugins only. `collect` only overwrites slots for plugins it visits, so
  saving from editor mode preserves existing blobs rather than dropping them.
  Corollary: you cannot save gameplay progress from editor mode, which is
  correct.

New exclusive systems, registered in `ScriptHostPlugin::build`:

```rust
app.add_systems(OnEnter(EditorMode::Gameplay), activate_all_plugins);
app.add_systems(OnExit(EditorMode::Gameplay), deactivate_all_plugins);
```

Both no-op outside `AppState::InGame` (no plugins are loaded then anyway, so
the `OnEnter(Editor)` that fires at startup is harmless).

### 3c. `InputLock` (`crates/engine-api/src/lib.rs`)

`InputLock` holders are `&'static str` pointing into dylib pages, so the host
cannot unlock by identity. Add:

```rust
pub fn unlock_id(&mut self, holder: &str)  // HashSet<&'static str> + Borrow<str>
pub fn clear(&mut self)
```

`deactivate` calls `unlock_id(plugin_id)`. The mode transition additionally
calls `clear()` and re-locks the cursor
(`CursorGrabMode::Locked`, `visible = false`), since the editor is FPS-style —
otherwise a plugin that deactivated while holding a menu-open cursor leaves the
editor with a free cursor.

### 3d. Plugin updates

- **cafe-sim** (`plugins/cafe-sim/src/lib.rs`): no `register` body needed today
  — it registers no scripts yet — until step 4 adds `cafe-shell`. Everything
  else stays in `setup`.
- **example** (`plugins/example/src/lib.rs`): move `register_script` and
  `register_level_hooks` out of `setup` into `register`. The `ScriptCounter`
  entity, `ExampleLevelData` init, and `schedule.add_systems` stay in `setup`.
- **example2**: unchanged (inherits the default `register`).

### 3e. Tests (`src/script_host.rs` test module)

Extend the existing harness:

- Loading in editor mode populates `ScriptRegistry` and installs **no**
  schedule.
- Activate → deactivate removes exactly the plugin's `ScriptOwned` entities and
  leaves registry entries intact.
- Hydrated bindings survive an activate/deactivate cycle (the explicit
  contract from 3b).
- A reload while inactive re-registers and stays inactive.

### 3f. Docs

Update `CLAUDE.md` (the "Dynamic Plugins & Scripts" section and the ABI version
note) and `docs/LEVEL_SAVE_DESIGN.md` where it describes the plugin lifecycle.

---

## Step 4 — Cafe shell as a level entity

After step 3, cafe-sim's schedule does not run in editor mode, so
`spawn_cafe_scene` never fires there and the shell is absent from the editor —
including from `Shift+B` bakes, where it currently occludes. Fix by making the
shell a placed level entity that the plugin recognises.

### 4a. The marker

Register a `cafe-shell` script in cafe-sim's new `register`:

```rust
ScriptSchema::new("cafe-shell").with_default_mesh("cafe-sim/Cafe.gltf#Scene0")
```

`default_mesh` already makes the editor's placement catalog offer a one-click
"mesh + binding" preset (`src/editor/mesh_selector.rs:85`), so this needs no
editor changes at all. Place it with Tab → `cafe-sim/cafe-shell` → LMB, then
`Shift+S`.

`hydrate_cafe_shell` inserts a plugin-private `CafeShell` marker. The host's
archetype diff removes it on unload, as with any other binding.

Delete `spawn_cafe_scene`'s call from `apply_phase_transitions`.

### 4b. Materials

`Cafe.gltf` ships geometry only, so the placed path's `ConvertToToon` gives
every surface a plain white toon material. The named-node logic (`Floor`,
`*Wall` + brick texture) currently lives in `on_cafe_scene_ready`.

Convert it to a plugin system keyed off `CafeShell` that runs once the scene is
ready. It runs in gameplay mode only, so **the shell is white in the editor**.
That is purely cosmetic: the bakers use a single scalar `terrain_albedo` for
bounce (`src/game/world/baking/raycast.rs:383`) and never sample object
materials, so bake output is unaffected.

**Critical:** the rewritten system must **preserve the existing lightmap
handle** rather than reaching for `DefaultToonLightmap`. By the time it runs,
`apply_object_lightmaps` has already swapped each sub-mesh's baked lightmap
into its toon material; constructing fresh `ExtendedMaterial`s with the default
white lightmap (as `on_cafe_scene_ready` does today) would throw the bake away.
Read `extension.lightmap` off the current material and carry it over.

### 4c. Grid anchor

`grid.rs` hardcodes the cafe to world origin — `cell_center`, `world_to_cell`,
`camera_floor_hit`, `PLAYER_SPAWN`, `draw_floor_grid` — across 31 call sites in
`cafe_scene.rs`, `customer.rs`, `delivery.rs`, `lib.rs`, `placement.rs`,
`staff_agent.rs`. A shell placed anywhere other than the origin would desync
from its own furniture grid.

Add a `CafeAnchor` resource (translation only), populated from the `CafeShell`
entity's `GlobalTransform` when gameplay starts:

- No shell placed in the level → warn once, fall back to `Vec3::ZERO`, behave
  exactly as today.
- Shell placed with a non-identity rotation → warn that rotation is ignored.
  A rotated grid would mean rotating piece placement, queue cells, and staff
  pathing; out of scope.

Thread the anchor through the five grid functions. `FLOOR_Y` usages mostly stay
as-is; only the ones defining the grid plane become `anchor.y`.

### 4d. Collider

`spawn_entity_record` attaches `RigidBody::Static` +
`ColliderConstructorHierarchy(TrimeshFromMesh)` for `ColliderMode::Trimesh`,
which is the default for `PlacedKind::mesh` — so the shell keeps its collision
and its `AutoCollider` call in `cafe_scene.rs` goes away with the old spawn
path.

Static ancestry also means `baking/bvh.rs` picks it up for shadow casting in
every bake, which is strictly better than today (where it only occluded if the
plugin happened to be running).

---

## Verification

Per step: `cargo check --workspace`, `cargo clippy --workspace`,
`cargo test --workspace`.

`cargo run` does **not** build the plugin dylibs — run
`cargo build --workspace` before any manual test of steps 3 and 4, or the
loader silently finds no dylib.

Manual checks to request (visual app, cannot self-verify):

1. **Step 1** — toggle edit mode, Tab, K, Shift+G in turn; the controls panel
   should swap its rows each time and never show stale ones.
2. **Step 2** — Alt+Scroll rotates the preview; place, grab with `R`, rotate,
   drop; `Shift+S`, reload the world, confirm rotation and script bindings
   survived. Grab then `F1` out — the object should come back, not vanish.
3. **Step 3** — in editor mode: no cafe HUD, Escape opens the *engine* pause
   menu, Tab opens only the placement catalog. `F1` into gameplay brings up the
   cafe main menu; `F1` back tears it all down; `F1` again starts fresh.
   Shift+R hot-reload should work in both modes.
4. **Step 4** — place the shell, `Shift+S`, `Shift+B`; confirm the shell casts
   baked shadows and that entering gameplay applies the floor/wall materials
   *without* losing the bake (walls should look lit, not flat).

## Risks

- **Level-data ordering (3b).** If `apply_plugin_level_data` runs before
  `setup` on activation, cafe-sim's unconditional `insert_resource(…::default())`
  calls wipe the loaded save. The `AppliedPluginData` clear must happen inside
  `activate`, after `plugin_setup` returns.
- **Click double-handling (2c).** `place_grabbed_object` must consume the LMB
  press before `handle_edit_mode_click` sees it, or dropping a grabbed object
  also places a new one on top of it.
- **Lightmap loss (4b).** Covered above; the single most likely regression in
  step 4, and invisible in `cargo check`.
- **Grab leak (2c).** Every exit path out of edit/editor mode while carrying
  must restore the object.
