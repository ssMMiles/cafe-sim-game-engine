# Docked Editor UI — Design

Status: **approved 2026-10-24** (§7 decisions settled: organisational groups, no engine pause and no settings window in the editor, no layout persistence in v1, Ctrl+LMB lowers terrain, fly only while right-dragging). All five phases implemented 2026-10-24 (see §5 notes). Turns the editor from
"a game with edit keybinds" into a docked editor: a 3D viewport tab with egui
panels around it (outliner, inspector, placement catalog, menu bar), a free
OS cursor, an editor fly camera, click-to-select, and in-place property
editing. Gameplay mode is untouched: full-window render, plugin-owned cursor
and UI.

Supersedes parts of `EDITOR_CONTROLS_PLAN.md`: §3c (re-lock the cursor
because "the editor is FPS-style"), the RMB = lower-terrain row, and §2c's
"position follows the crosshair" limitation. Everything else there stands.

---

## 1. What exists today, and what the design leans on

Facts verified against the code on 2026-10-24.

| Concern | Today | Consequence for this design |
|---|---|---|
| 3D output | `Camera3d` renders to a 1920×1080 `Rgba16Float` `Image` (`gfx/camera.rs:48-74`); a `Sprite` on layer 1 stretches it to the window (`resize_render_texture_sprite`); a `Camera2d` (`IsDefaultUiCamera`, layer 1) presents sprite + bevy_ui | The texture is already a bindable image. egui can draw it directly; no render-pipeline work |
| Editor camera | There is none. `camera_follow` places `PlayerCamera` from the `Player` entity; WASD moves the player (walking physics in `FloorBased`, direct translation in `FreeCam`) | Needs an editor fly controller that drives the camera transform and leaves the player alone |
| Streaming / bake centre | Both centred on `Player` (`streaming.rs:149`, `baking/mod.rs:375`). LOD and sky follow `PlayerCamera` | Flying away from the player would stop chunks loading; needs a "world focus" that is the camera in editor mode |
| Cursor | Locked + hidden in editor (`simulation.rs:22`, `editor/mod.rs:295`, pause menu). Only `cafe-sim` touches it in gameplay | Editor must own a free cursor; lock only while right-drag looking |
| Picking | Every editor tool casts `Ray3d::new(cam.translation, cam.forward())` (`PlacedPicker`, `update_placement_preview`, `raycast_terrain_hit`, `check_interactions`) | Replace with one per-frame "cursor ray" resource |
| Input | Every binding reads `ButtonInput<KeyCode>` / `ButtonInput<MouseButton>` / `MouseWheel` raw; nothing knows about UI. Scroll is shared by modifier only (`editor/input.rs`) | Gate on "viewport hovered" and "egui wants keyboard" |
| Editor UI | bevy_ui panels on `UiAnchors`: controls, FPS/position HUD, Tab catalog strip, K scripts strip; both strips centre on `window.width()` | All move to egui over the phases; strips stay functional (if misplaced) until replaced |
| Pause menu | bevy_ui, spawns only in `EditorMode::Editor`; holds Settings, Switch/Add world, Add/Remove plugin, Exit | Becomes a menu bar + windows in Phase 4; interim handling in §4.8 |
| Data model | `PlacedObject { id, chunk, name, kind }` + `Transform` + `ScriptBindings` are the live truth; `to_record` rebuilds the file record at save. `chunk` is a stored field, never recomputed. Dirty tracking is `registry.dirty_chunks.insert(..)` by hand | In-place editing is cheap, but every edit must go through one helper that fixes `chunk` and dirties |
| Scripts | `hydrate_script_bindings` skips pairs already in `HydratedScripts`; no `Changed<ScriptBindings>` reaction. Schema = key + type + default only | Prop edits must `detach_binding` + `NeedsHydration`; widgets inferred from `PropValue` variant |
| Selection / names | No selection concept, no `Name` component, name duplicated into `Interactable.name` on root and mesh children | Introduce `Selection`; rename must update the copies |
| Outliner scope | Only loaded chunks have entities; unloaded chunks exist only on disk | v1 outliner lists loaded chunks and says so |

### 1.1 Dependency facts

The latest crates do **not** fit Bevy 0.18. Verified by resolving a scratch
project against the registry:

| Crate | Version | Why |
|---|---|---|
| `bevy_egui` | **0.39** (resolves 0.39.1) | 0.39.0 = "Update to Bevy 0.18"; 0.40 → Bevy 0.19, 0.43 → Bevy 0.20 |
| `egui_dock` | **0.18** | Only line on egui 0.33, which bevy_egui 0.39 uses |
| `egui` | 0.33 (transitive) | Use `bevy_egui::egui`; never add `egui` directly |
| `bevy-inspector-egui` | 0.36 would fit | Not planned; we have typed props, not reflection |

bevy_egui 0.39 facts that shape the design (all read from source):

- UI systems run in the `EguiPrimaryContextPass` schedule, executed during
  `PostUpdate`. Anything the UI publishes for `Update` systems arrives next
  frame.
- `EguiContexts::ctx_mut()` returns `Result`; errors if there is not exactly
  one `PrimaryEguiContext`.
- Default `auto_create_primary_context` puts the context on the *first*
  `Added<Camera>` — the main-menu `Camera2d` — and the in-game cameras spawn
  in one frame. We disable auto-creation and tag the in-game `Camera2d`
  ourselves.
- User textures bind the image's own view + sampler with
  `Float { filterable: true }`; `Rgba16Float` is filterable; the shader
  treats the texture as linear and converts to sRGB — same result as the
  sprite path. No format change needed.
- Input is *read*, not consumed (`absorb_bevy_input_system` is off by
  default and stays off). `ButtonInput<_>` keeps working.
- `EguiWantsInput` / `egui_wants_any_pointer_input` are useless for a
  whole-window dock: the pointer is always "over an egui area". Pointer
  gating must come from the viewport tab's own `Response`.
- Pointer position comes only from `CursorMoved`, so a `Locked` cursor
  freezes egui's pointer. Fine for right-drag look as long as we release
  cleanly.
- `UiRenderOrder` orders egui against bevy_ui per camera; default is egui on
  top.
- egui_dock wraps every tab body in `ScrollArea` + `Frame(inner_margin)`;
  the viewport tab must opt out (`clear_background = false`,
  `scroll_bars = [false, false]`, zero `tab_body.inner_margin`).
  `TabViewer::on_rect_changed` fires when a body rect changes.
- `ctx.pixels_per_point()` = window scale × `EguiContextSettings.scale_factor`;
  physical size of a rect = `rect.size() * ppp`.

---

## 2. Core decisions

1. **egui for editor chrome, bevy_ui for player-facing UI.** Everything
   under `src/editor/` that draws moves to egui over Phases 1–4. `src/ui/`
   (theme, widgets) stays for the main menu, gameplay pause menu, crosshair,
   and plugins. Nothing egui-shaped crosses the plugin ABI.
2. **One 3D camera entity, two drivers.** `PlayerCamera` keeps its render
   stack (HDR target, prepasses, edge detection, fog, FXAA). In
   `EditorMode::Editor` an `editor_fly_camera` system writes its transform
   and `camera_follow`, `apply_freecam_movement`, and `character_physics`
   do not run. In Gameplay nothing changes. Switching modes already resets
   the gameplay session (plugins tear down), so camera continuity is not
   expected.
3. **The viewport is an egui tab showing the existing render texture.** The
   texture is resized to the tab's physical rect in editor mode and
   restored to 1920×1080 (sprite shown, full window) for gameplay. The
   sprite path is kept, not deleted: it is the gameplay presentation and
   `cafe-sim` projects world→screen assuming it fills the window.
4. **`ViewportInput` is the only pointer authority for editor tools.**
   Written each egui pass from the viewport tab's `Response`: hovered,
   cursor position in texture pixels, look-drag active, scroll delta. Every
   editor system that today reads mouse buttons or wheel gates on it.
   Keyboard bindings gate on `!EguiWantsInput::wants_any_keyboard_input()`
   so typing in a text field never places, deletes, or saves.
5. **One cursor ray per frame.** `EditorRay(Option<Ray3d>)` is computed once
   in `Update` from `ViewportInput.cursor_px` via `Camera::viewport_to_world`
   and consumed by the picker, placement preview, terrain brush, and
   interaction hover. The crosshair is gone from editor mode.
6. **Explicit tools instead of two booleans.** `EditorTool { Select, Place,
   Terrain }` is a Bevy `State` (OnExit cleanup replaces the ad-hoc
   `release_grab_on_edit_mode_exit` / `clear_editor_sub_modes` dance).
   `UserSettings.edit_mode` / `terrain_edit_mode` are removed (engine-api
   change → ABI 5; no plugin reads them).
7. **Selection is a resource, single entity in v1.** Set by viewport click
   (Select tool) and the outliner; validated every frame; highlighted with
   gizmos. Multi-select and transform gizmos are explicitly deferred.
8. **Every mutation of a placed entity goes through one commit path.**
   `PlacedEdited { entity, kind }` message → `commit_placed_edits` recomputes
   `PlacedObject.chunk` from the transform, dirties old + new chunk, syncs
   `Interactable.name`, and flags lighting as stale. Script prop edits
   additionally re-hydrate. This is also the seam a future undo stack hooks.
9. **Grouping is organisational, not a transform hierarchy.** A new
   `parent: Option<u64>` on the record (objects v5) nests entities under
   `PlacedKind::Group` entities in the outliner. No ECS parenting: children
   keep world transforms, stream with their own chunk, and "move the group"
   is an editor op applying a delta to loaded descendants. Rationale in §7.
10. **No new persistence beyond objects v5 and an editor layout file.** The
    dock layout is saved with `egui_dock`'s `serde` feature to
    `projects/{id}/editor_layout.ron`-style JSON via the storage layer; it
    is editor state, not level data, and losing it is harmless.

---

## 3. Layout

```
┌ menu bar: File  Edit  View  Tools  Plugins  Help ─────────────────────────┐
│ toolbar: [Select] [Place] [Terrain]   | Save | Bake |  world: cafe  mode ▸ │
├────────────┬──────────────────────────────────────────────┬────────────────┤
│ Outliner   │                                              │ Inspector      │
│  search    │              Viewport (tab)                  │  name, id      │
│  ▸ Group A │                                              │  transform     │
│    Lamp 12 │   right-drag: look  WASD/QE: fly             │  kind props    │
│    Rock 7  │   click: select     scroll: dolly            │  scripts       │
│  Chair 3   │                                              │  lighting state│
├────────────┴───────────────┬──────────────────────────────┴────────────────┤
│ Place (catalog) / Terrain brush / Scripts (context tabs)                   │
├────────────────────────────────────────────────────────────────────────────┤
│ status: FPS  cam xyz  chunk  | 3 dirty chunks | lighting stale | hints     │
└────────────────────────────────────────────────────────────────────────────┘
```

Default `DockState`: root = Viewport; `split_left(root, 0.18, [Outliner])`;
`split_right(viewport, 0.78, [Inspector])`; `split_below(viewport, 0.75,
[Place, Terrain, Scripts])`. Menu bar, toolbar, and status bar are
`TopBottomPanel`s drawn *before* `DockArea::show` (it fills the remaining
`CentralPanel`). `View ▸ Reset layout` restores the default.

All editor UI is drawn only when `AppState::InGame && EditorMode::Editor`.
In Gameplay the egui context exists but draws nothing.

---

## 4. Runtime model

### 4.1 Module layout

```
src/editor/
  mod.rs            EditorPlugin: EditorMode, EditorTool, EditorSet, wiring
  tool.rs           EditorTool state + hotkeys + OnExit cleanups
  camera.rs         editor fly camera, WorldFocus, cursor lock while looking
  picking.rs        ViewportInput → EditorRay; PlacedPicker moves here
  selection.rs      Selection resource, click-select, highlight gizmos, focus
  edits.rs          PlacedEdited message, commit_placed_edits, respawn helper
  ui/
    mod.rs          EditorUiPlugin: bevy_egui setup, DockState, TabViewer, layout persistence
    viewport.rs     viewport tab: image, Response → ViewportInput, render-target resize
    menu.rs         menu bar, toolbar, settings window, world/plugin dialogs
    outliner.rs
    inspector.rs    + prop widgets for PropValue
    place.rs        catalog panel (replaces mesh_selector strip)
    terrain.rs      brush panel (size, strength)
    status.rs       status bar (replaces debug_hud)
  object_placement.rs, grab.rs, terrain_edit.rs, script_binding.rs, ...  (logic stays, input rewired)
```

`src/ui/` is untouched except for deleting editor-only widgets once nothing
uses them.

### 4.2 egui bootstrap (`ui/mod.rs`)

```rust
app.add_plugins(EguiPlugin::default());
app.insert_resource(EguiGlobalSettings {
    auto_create_primary_context: false,
    enable_absorb_bevy_input_system: false,   // default; stated for clarity
    ..default()
});
```

`setup_camera` adds `PrimaryEguiContext` to the in-game `Camera2d` (the one
with `IsDefaultUiCamera`). The main-menu camera gets nothing, so
`ctx_mut()` errors there and UI systems return early. `UiRenderOrder` is
set to `BevyUiAboveEgui` so the interim pause overlay (§4.8), the crosshair
in gameplay, and plugin UI are never hidden by egui.

All egui systems: `EguiPrimaryContextPass`, run_if `in_state(InGame) &&
in_state(EditorMode::Editor)`, chained in one fixed order:
`menu → toolbar → status → dock(viewport, outliner, inspector, bottom tabs)`.

The `TabViewer` is a struct built per pass from `SystemParam`s (queries,
resources). Each tab's `ui` fn lives in its own file and takes `&mut Ui`
plus the params it needs.

### 4.3 Viewport tab (`ui/viewport.rs`)

```rust
#[derive(Resource, Default)]
pub struct ViewportInput {
    /// Pointer is over the viewport image and no egui widget is being dragged.
    pub hovered: bool,
    /// Cursor position in render-texture pixels (origin top-left), if hovered
    /// or looking.
    pub cursor_px: Option<Vec2>,
    /// Right-button drag in progress (fly-look). Cursor is locked meanwhile.
    pub look_active: bool,
    /// Primary click this pass, in texture pixels.
    pub clicked_px: Option<Vec2>,
    /// Scroll delta over the viewport this pass (points).
    pub scroll: Vec2,
    /// Physical size of the tab body; what the render target is resized to.
    pub size_px: UVec2,
}
```

Per pass, inside `TabViewer::ui` for `Tab::Viewport`:

1. `rect = ui.available_rect_before_wrap()`; `ppp = ctx.pixels_per_point()`;
   `size_px = (rect.size() * ppp).round()` clamped to ≥ 1.
2. Resize: if `size_px != image.size()`, `images.get_mut(target).resize(extent)`.
   Bevy's camera system observes the asset modification and updates the
   camera's target info; the post-process node rebuilds bind groups per
   frame (`post_process.rs:107-119`) and the shader reads
   `textureDimensions`, so nothing else needs to know. Splitter drags cause
   a resize per frame; the `Image` CPU buffer is `w*h*8` bytes — fine on
   native, and we quantise `size_px` to multiples of 8 to cut churn.
3. Draw: `ui.add(Image::from_texture(SizedTexture::new(tex_id, rect.size()))
   .fit_to_exact_size(rect.size()).sense(Sense::click_and_drag()))`.
   `tex_id` is registered once per in-game session with
   `EguiContexts::add_image(EguiTextureHandle::Weak(id))` (weak: freed with
   the asset when the game exits).
4. Fill `ViewportInput` from the `Response`: `hovered()`, `hover_pos()`
   (→ `(p - rect.min) * ppp`), `dragged_by(Secondary)`,
   `clicked_by(Primary)`, `ui.input(|i| i.smooth_scroll_delta)` when hovered.
5. `TabViewer` overrides: `clear_background → false`, `scroll_bars →
   [false,false]`; style `tab_body.inner_margin = Margin::ZERO` for all
   tabs (other tabs add their own `Frame`).

**Optional pixel-art scale.** `EditorSettings.viewport_scale: f32`
(default 1.0). The texture is sized `size_px * scale`; egui upsamples with
the image's existing nearest sampler. Gives the game's chunky look in the
editor at 0.5 without any shader work.

**Mode transitions.** `OnEnter(EditorMode::Editor)`: hide
`RenderTextureSprite`. `OnExit(EditorMode::Editor)`: resize the image back
to `RENDER_TEXTURE_WIDTH × HEIGHT`, show the sprite. `resize_render_texture_sprite`
gains `run_if(in_state(EditorMode::Gameplay))`.

### 4.4 Cursor and fly camera (`camera.rs`)

Cursor policy by state (replaces the six lock sites in editor paths):

| State | Cursor |
|---|---|
| InGame + Editor, idle | free, visible |
| InGame + Editor, `look_active` | `Locked`, hidden; on release, restore and `set_cursor_position` back to where the drag began |
| InGame + Editor + Paused (interim, §4.8) | free, visible |
| InGame + Gameplay | untouched by the engine; the plugin owns it (as today) |

`lock_cursor_for_editor` (`editor/mod.rs:295`) becomes
`free_cursor_for_editor`. `simulation.rs:22` no longer locks on `OnEnter(InGame)`
(editor is the default mode); gameplay plugins already lock on their own.
The pause menu's `handle_cursor_lock` stays for gameplay-mode semantics but
is skipped in editor mode.

Fly controller (`editor_fly_camera`, `EditorSet::Active`):

- Look: while `ViewportInput.look_active`, apply `MouseMotion` deltas ×
  `mouse_sensitivity` to `PlayerCamera.yaw/pitch` (same fields, same clamp,
  so Gameplay's `handle_camera_input` picks up where it left off).
- Move: only while looking (Unity/Unreal convention, so W/A/S/D/Q/E are
  free for tool hotkeys otherwise). WASD planar relative to view, Q/E
  down/up, Shift ×4. Speed in m/s × `delta_secs` (the current freecam is
  per-frame and frame-rate dependent — fixed as a side effect). Scroll
  while looking scales fly speed; scroll while hovering idle dollies along
  the view ray.
- Focus: `F` or outliner double-click frames `Selection` (distance from the
  selection's AABB radius).
- `Shift+M` / `Shift+V` (`MovementMode`/`CameraView`) become gameplay-only.

`WorldFocus(Vec3)` resource, updated first in `Update` each frame: camera
translation in Editor, `Player` translation in Gameplay. `update_streaming_ring`
and the bake trigger read it instead of `Player`. LOD and sky already follow
the camera.

Systems that must stop running in Editor: `camera_follow`,
`apply_freecam_movement`, `handle_camera_input`, `adjust_camera_distance`
(`GfxSet::Running`/`InGame` gain `in_state(EditorMode::Gameplay)` for these
four), `character_physics` and `check_interactions`/`detect_interactable_clicks`/
`show_interaction_text` (`SimSet::Running` registrations gain the same
gate; `update_interaction_info_display` is replaced by the inspector/status
bar). The player stands where it was, visible, and is where Gameplay
resumes.

### 4.5 Picking (`picking.rs`)

```rust
#[derive(Resource, Default)]
pub struct EditorRay(pub Option<Ray3d>);

fn compute_editor_ray(vp: Res<ViewportInput>, cam: Single<(&Camera, &GlobalTransform), With<PlayerCamera>>, mut out: ResMut<EditorRay>) {
    out.0 = vp.cursor_px.and_then(|px| cam.0.viewport_to_world(cam.1, px).ok());
}
```

`Camera::logical_viewport_size` for an image target is the image size (scale
factor 1), so texture pixels are the right space. `compute_editor_ray` runs
first in `EditorSet::Active`; `PlacedPicker::pick`, `update_placement_preview`,
`raycast_terrain_hit`, and the editor hover all take `Res<EditorRay>`
instead of building their own ray. `PlacedPicker` keeps its mesh-then-sphere
logic. The `.unwrap()` on `Dir3::new` in `terrain_edit.rs:78` disappears
with it.

Pure helper `cursor_to_texture_px(hover_pos, rect, ppp)` and the
`viewport_to_world` round-trip get unit tests.

### 4.6 Input gating

Two run conditions in `editor/mod.rs`:

```rust
fn viewport_pointer_free(vp: Res<ViewportInput>) -> bool { vp.hovered || vp.look_active }
fn keyboard_free(w: Res<EguiWantsInput>) -> bool { !w.wants_any_keyboard_input() }
```

- Every editor system that reads `ButtonInput<MouseButton>` or `MouseWheel`
  gets `.run_if(viewport_pointer_free)`: placement click, grab drop, terrain
  brush press/release, scroll helpers. The shared scroll helper
  (`editor/input.rs`) reads `ViewportInput.scroll` rather than `MouseWheel`,
  so egui scroll areas and the viewport never fight.
- Every keyboard binding gets `.run_if(keyboard_free)`, including the
  `EditorSet::InGame` toggles (Shift+S/B/L/P, Tab, K, Delete, R, `[` `]`).
  F1 and Escape are exempt.
- `handle_pause_input` (Escape) in editor mode is redefined in Phase 4.

Terrain brush buttons change: **LMB raise, Ctrl+LMB lower** (RMB is
look). Brush size/strength also get sliders in the Terrain panel.

### 4.7 Selection (`selection.rs`)

```rust
#[derive(Resource, Default)]
pub struct Selection { pub primary: Option<Entity>, pub hovered: Option<Entity> }
```

- `select_on_click`: Select tool + `ViewportInput.clicked_px` → `PlacedPicker::pick`
  → `primary` (click on nothing clears). Escape clears when no panel has
  focus.
- `validate_selection`: drops entities that no longer exist (chunk unload,
  delete). Grab/respawn paths re-point `primary` at the respawned entity.
- `draw_selection_gizmos`: wireframe AABB from the root's mesh descendants'
  `Aabb` × `GlobalTransform` (lights/empties: their existing gizmo, drawn
  brighter). Hover gets a dimmer box. Runs in `EditorSet::InGame` so it
  shows while paused.
- Outliner click sets `primary`; inspector edits `primary`; `Delete` with
  the viewport or outliner focused deletes `primary` (today: whatever is
  under the crosshair).

### 4.8 Pause menu, interim

Until Phase 4 replaces it, Escape still pauses and spawns the bevy_ui editor
pause menu. With `BevyUiAboveEgui` the translucent overlay draws over the
dock. While `SimulationState::Paused` in editor mode the egui systems skip
`DockArea::show` (panels vanish; the overlay's background colour shows).
This is ugly for a few phases and acceptable; it avoids bevy_ui buttons and
egui widgets both receiving the same click.

### 4.9 Edits (`edits.rs`)

```rust
#[derive(Message)]
pub struct PlacedEdited { pub entity: Entity, pub what: EditKind }
pub enum EditKind { Transform, Name, Kind, Scripts, Parent }

pub fn commit_placed_edits(/* reader, Query<(&mut PlacedObject, &Transform, Option<&mut Interactable>, Option<&Children>)>, registry, children Interactables, BakeState */)
```

Per message:

| `what` | Action |
|---|---|
| any | `registry.dirty_chunks.insert(obj.chunk)` (new helper `PlacedObjectRegistry::mark_dirty`) |
| Transform | `new = world_pos_to_chunk(t.x, t.z)`; if `new != obj.chunk`: dirty both, set `obj.chunk = new`. Set `LightingState::Stale` |
| Name | copy to root `Interactable.name` and every descendant `Interactable` (same walk as `propagate_to_mesh_children`) |
| Kind (light params) | `LightingState::Stale` (baker and gizmo already read `PlacedObject.kind` live) |
| Kind (mesh asset / collider mode) | respawn in place: `to_record` → despawn → `spawn_entity_record` with the same id (grab's existing path, factored out as `respawn_placed`); selection follows |
| Scripts | for each changed binding: `commands.queue(detach_binding(..))`, insert `NeedsHydration`. `required_plugins` is recomputed at save as today |
| Parent | dirty only (Phase 5) |

Chunk pinning: dirty chunks never unload (`streaming.rs:184-205`), so a long
editing session pins chunks until Shift+S. Unchanged from today; the status
bar shows the dirty count so it is visible.

`LightingState { Clean, Stale }` resource: set `Stale` by transform/light/
kind edits, placement, delete; `Clean` after a bake. Shown in the status bar
and inspector ("Lighting out of date — Bake (Shift+B)"). Nothing is
invalidated automatically; the bake is still manual.

### 4.10 Outliner (`ui/outliner.rs`)

- Source: `Query<(Entity, &PlacedObject)>` — loaded chunks only. Header:
  "N entities · M loaded chunks" with a tooltip that unloaded chunks are not
  listed. Whole-world indexing (scanning `chunks/*.objects`) is deferred;
  the per-chunk file layout makes it straightforward later.
- Rows: icon by kind, display name (`name` or `kind.label()`), id, script
  badge count. Sorted by name then id; a text filter matches name, kind,
  id, script name.
- Interaction: click selects; double-click or `F` focuses the camera;
  `F2`/double-click on name edits inline (→ `PlacedEdited::Name`); right-click
  context menu: Focus, Rename, Duplicate (new id, same chunk logic as
  placement), Delete.
- Group view (Phase 5): `CollapsingHeader` per group, egui `dnd_drag_source`
  / `dnd_drop_zone` to reparent, "Group selection" action.
- Rebuilt every pass (immediate mode); the query is a few hundred rows at
  most, no caching.

### 4.11 Inspector (`ui/inspector.rs`)

Sections for `Selection.primary`:

1. **Header**: name (editable), id, kind label, chunk, "Focus" button.
2. **Transform**: position / rotation (Euler degrees, converted to `Quat`
   on commit) / scale, as `DragValue` rows. `changed()` → write `Transform`
   and send `PlacedEdited::Transform` every frame while dragging; the commit
   system is idempotent so this is cheap. Scale is hidden for lights/empties
   (`effective_scale` rule). A locked-aspect toggle for uniform scale.
3. **Kind**:
   - Mesh: asset path (read-only label + "Change…" picking from the
     catalog), collider mode combo → respawn.
   - PointLight / SpotLight: colour (`color_edit_button_rgb` on linear
     values — egui's editor works in its own `Rgba` linear space, so
     convert explicitly and test the round-trip), intensity (lightmap
     units, tooltip says so), range, inner/outer angle in degrees for spot.
     The `shadows` flag is shown disabled with "unused".
   - Empty: nothing.
4. **Scripts**: one collapsible per binding: plugin · script, "Detach",
   then props. Widget by `PropValue` variant:

   | Variant | Widget |
   |---|---|
   | Bool | checkbox |
   | Int | `DragValue` (i64) |
   | Float | `DragValue` speed 0.01 |
   | Str | single-line `TextEdit` |
   | Vec3 | three `DragValue`s |
   | Color | `color_edit_button_rgba_unmultiplied` (linear RGBA, convert) |
   | EntityRef | id `DragValue` + resolved name via `registry.by_id` ("unloaded" if absent) + "Pick" button: next viewport click assigns |

   Script props commit on `drag_stopped() || lost_focus() || changed()`
   for non-drag widgets — never per frame, because the commit re-hydrates
   the binding and resets the plugin's runtime state. If the schema is
   unavailable (plugin not loaded) props are still editable by variant,
   with a warning row. "Attach script…" opens a combo of
   `ScriptRegistry::scripts()` sorted by plugin then name; attach seeds
   `schema.default_props()` as the K panel does today.
5. **Lighting**: per-object lightmap presence (file exists for this id),
   `LightingState`, Bake button.

Nothing selected: the inspector shows world environment (sun direction,
sun/ambient colour and intensity in lightmap units) editing `EnvironmentData`
— the same path `apply_environment` already consumes — plus the "Bake" button.

### 4.12 Place panel (`ui/place.rs`)

Replaces the Tab strip. Grid of catalog entries (`MeshCatalog`: meshes,
lights, plugin presets), filter box, selected entry highlighted. Selecting
sets `PlacementConfig` (kind, `stamp_script`) and switches to
`EditorTool::Place`. Scale/rotation `DragValue`s mirror Shift/Alt+scroll.
`MeshCatalog`, `PlacementConfig`, preview and click-to-place logic are
reused; only `mesh_selector.rs`'s bevy_ui spawning and scroll-cycling go.

### 4.13 Menu bar, toolbar, status bar (`ui/menu.rs`, `ui/status.rs`)

- **File**: Save (Shift+S), Bake (Shift+B), Switch world ▸ (list from
  `ActiveProject`), New world…, Exit to main menu.
- **Edit**: Delete, Duplicate, Focus selection, Deselect.
- **View**: Baked lighting (Shift+L), Physics gizmos (Shift+P), Edge
  detection (F2), Light gizmos, Reset layout, Viewport scale.
- **Tools**: Select / Place / Terrain (also the toolbar; hotkeys `1 2 3`
  while the viewport has focus; Shift+E / Shift+G keep working as toggles
  for muscle memory).
- **Plugins**: loaded list with Remove, Add…, Reload all (Shift+R).
- **Status bar**: FPS, camera position, current chunk, tool hint
  (contents of today's `ControlsCache` rows for the active tool), dirty
  chunk count, lighting state, hover target name.

With these in place the editor pause menu is retired: `setup_editor_pause_menu`
and its Settings/AddWorld/AddPlugin sub-screens stop being spawned in
editor mode; their logic (world switch, plugin add/remove, settings) is
called from egui. Escape in editor: cancel grab → deselect → nothing. The
`SimulationState::Paused` state remains for gameplay plugins. `handle_pause_input`
runs only in Gameplay from then on; the engine no longer pauses in the
editor at all. The `AddWorld` text input and `PauseMenuState` sub-states
become gameplay-unused and can be deleted with the bevy_ui pause menu once
no gameplay path needs them (cafe-sim has its own pause menu).

### 4.14 Layout persistence

`DockState<Tab>` with `egui_dock` feature `serde` and `Tab` deriving
`Serialize/Deserialize`. Saved via the storage layer to
`projects/{id}/editor_layout.json` on `OnExit(EditorMode::Editor)` and
`OnExit(InGame)`; loaded on `OnEnter`. Missing or unparsable → default
layout. This adds a `serde_json` dev-facing dependency; the codebase avoids
serde for *level* data, which this is not. If that is unwelcome, the
fallback is to skip persistence in v1 (reset to default each run).

### 4.15 WASM

bevy_egui and egui_dock support wasm32 (verified in bevy_egui's
`Cargo.toml` and docs). Layout persistence uses the existing OPFS path.
Nothing in this design touches dylibs; the editor UI compiles identically
on web.

---

## 5. Phased plan

Each phase ends with `cargo build --workspace`, `cargo clippy`, tests, and
a visual check by you. Phases 1–3 deliver the asked-for outliner and
inspector; 4 finishes the migration off bevy_ui; 5 adds grouping.

### Phase 1 — Shell, viewport, camera, picking, selection

1. Add `bevy_egui = "0.39"`, `egui_dock = "0.18"`. `EditorUiPlugin` with
   the bootstrap in §4.2; `PrimaryEguiContext` on the in-game `Camera2d`.
2. `DockState` with Viewport + empty placeholder tabs; viewport tab per
   §4.3 including render-target resize and the sprite hide/show on mode
   transitions. Verify colour parity with the sprite path (same tonemapped
   HDR texture, egui's linear→sRGB conversion).
3. `ViewportInput`, `EditorRay`, `compute_editor_ray`; rewire the four ray
   sites. Delete the crosshair in editor mode (it stays a gameplay toggle).
4. Cursor policy and `editor_fly_camera`; `WorldFocus`; gate the gameplay
   camera/physics/interaction systems out of Editor.
5. `EditorTool` state; migrate `edit_mode`/`terrain_edit_mode` users;
   engine-api ABI → 5; remove the fields from `UserSettings`.
6. Input gating (§4.6), terrain Ctrl+LMB lower.
7. `Selection`, click-select, gizmo highlight, `F` focus, Delete on
   selection.
8. Tests: cursor→pixel mapping, ray round-trip against a known camera,
   selection validation on despawn.

Observable result: docked window, free cursor, right-drag fly, click to
select with a highlight box, all existing tools (place, grab, terrain,
scripts panel, save, bake) still work via the cursor ray. Gameplay (F1)
identical to today.

**Implementation notes (2026-10-24).** Landed as designed, with these
deviations: the bevy_ui debug HUD and controls panel were retired *now*
rather than in Phase 4 (they overlapped the dock), their content moved to
the egui toolbar/status bar; `Delete` acts on the selection rather than the
hovered entity; `R` grabs the hovered entity, falling back to the selection,
and works from the Select tool (switching to Place for the carry); the
scripts strip attaches to the selection; the catalog and scripts strips are
still bevy_ui and the viewport's scroll feeds them while it is hovered. The
render target resizes only after the tab rect has held still for two passes.
`WorldFocus` lives in `game/mod.rs`. `EditorSet::Prepare` was added so the
ray and selection validation run before every consumer.

### Phase 2 — Outliner and Place panel

1. `ui/outliner.rs` per §4.10 (flat list, filter, select, focus, context
   menu with Delete/Duplicate/Rename).
2. `edits.rs`: `PlacedEdited`, `commit_placed_edits`, `mark_dirty`,
   `LightingState`; rename wiring.
3. `ui/place.rs`; retire `mesh_selector.rs`'s bevy_ui strip and scroll
   cycling (keep `MeshCatalog` and manifest loading).
4. Tests: chunk re-assignment on cross-boundary transform edit (both
   chunks dirty, `obj.chunk` updated), rename propagation to child
   `Interactable`s.

### Phase 3 — Inspector

1. Transform, header, kind sections (§4.11 1–3) including mesh respawn via
   `respawn_placed`.
2. Scripts section with prop widgets, attach/detach, re-hydration commit;
   retire the K scripts strip (`script_binding.rs` keeps attach/detach
   logic as plain functions).
3. Environment panel when nothing is selected.
4. Tests: `PropValue` ↔ widget round-trips (colour linear conversion),
   re-hydration after a prop edit (host test module already has a
   hydration harness), `Interactable.name` sync.

### Phase 4 — Menu bar, status bar, settings; retire the editor pause menu

1. `ui/menu.rs`, toolbar, `ui/status.rs` (replaces `controls.rs`,
   `debug_hud.rs`); the keybind table from `controls.rs` becomes the status
   hint source.
2. World switch/new, plugin add/remove calling the
   existing functions in `pause_menu.rs`; stop spawning the editor pause
   menu; redefine Escape in editor; `handle_pause_input` gameplay-only.
3. Layout persistence (§4.14).
4. Delete `UiAnchors` users in `src/editor`; keep `src/ui` widgets that
   gameplay/menu still use, delete the rest.

**Implementation notes for Phases 2–4 (2026-10-24).** Done in one pass.
Deviations and details: the catalog strip, scripts strip, debug HUD,
controls panel, `UiAnchors`, the engine pause menu and the bevy_ui slider
are all gone; `src/ui` is now only the main menu's widget kit. Panels get
ECS access through one `Panels` system param. Script props are drafted in
the inspector and committed as one `ScriptEdit::SetProps` when no widget
in that binding is being dragged/focused and no popup is open. Mesh asset
and collider changes respawn in place via `RespawnPlaced::InPlace`;
Duplicate uses the same path with a fresh id and a 1 m nudge. The Place
panel owns the stamp picker (the K strip's role); `MeshCatalog::select`
keeps the "stepping off a preset disarms its stamp" rule. The outliner's
context menu and the Edit menu route through `EditorAction`, as do world
switch/create, plugin add/remove and reload (`ReloadRequested`). Escape is
"back out one level" rather than pause. The engine never pauses; plugins
own `SimulationState::Paused`. Lighting staleness is advisory
(`LightingState`), set by transform/kind/environment edits and placement,
cleared when a bake is requested. Environment editing is the inspector's
empty-selection view and writes `TerrainWorld.metadata.environment`.

### Phase 5 — Grouping

1. objects v5: `PlacedEntityRecord.parent: Option<u64>`, `PlacedKind::Group`
   appended; v4 reader maps to `parent: None`; `CHUNK_OBJECTS_VERSION`
   bump; round-trip tests.
2. `PlacedObject.parent`; outliner tree with `CollapsingHeader`s and
   drag-and-drop reparenting; Group/Ungroup actions; group transform
   delta op; delete-group = ungroup (Shift = delete recursively).
3. Selection of a group selects it as an entity (inspector shows name +
   children count); viewport click on a member selects the member, Alt-click
   selects its top-most group.

**Implementation notes for Phase 5 (2026-10-24).** Objects files are v5
(`parent: Option<u64>`, `PlacedKind::Group` appended); v3/v4 files parse
through `legacy::ChunkObjectsFileV4` and upgrade with `parent: None`.
`PlacedObject.parent` mirrors the record. The outliner is a manual tree
(indent + collapse toggles, `HashSet` of collapsed group ids) rather than
`CollapsingHeader`s so rows can be both egui drag sources and drop targets;
cycles on disk are guarded in both the tree walk and `SetParent`. Group
moves are deltas measured against a per-group transform snapshot kept by
`commit_placed_edits`, so rotating a group orbits its members about the
group origin. Groups are not viewport-pickable and cannot be grabbed;
Alt+click selects the outermost group. "New group with this" wraps one
entity (single selection), then members are dragged in. Duplicate on a
group copies only the group entity. Inline rename lives in the outliner's
context menu; the inspector's name field is the other path. Also in this
pass: a proportional-scale lock in the inspector (on by default), and a fix
for Shift+Scroll scaling — egui reports Shift+wheel as horizontal scroll,
so the viewport folds it back into the vertical axis.

### Deferred, by design

Multi-select, transform gizmos (`transform-gizmo-egui` is the obvious
candidate and would slot in behind `PlacedEdited::Transform`; its egui
version must be checked against 0.33 when the time comes), undo/redo
(record-based, hooks at `commit_placed_edits`), whole-world outliner index,
"play from here", prefab/duplicate-with-scripts semantics beyond a plain
record copy, terrain layer/material tools.

---

## 6. Risks and mitigations

| Risk | Mitigation |
|---|---|
| bevy_egui 0.39 is the last line for Bevy 0.18 | We are pinned to 0.18 anyway; both crates move together on the next Bevy bump. Keep every egui call behind `src/editor/ui` so a swap stays contained (your stated intent) |
| One-frame latency between egui pass (PostUpdate) and tools (Update) | Hover/pixel positions change continuously; a frame of lag is invisible. Clicks are delivered as `clicked_px` from the same pass, consumed next `Update` |
| `CursorGrabMode::Locked` + macOS warp jitter | Same mechanism the pause menu already uses; restore position on release; if it misbehaves, fall back to `Confined` + hidden and clamp |
| Resize churn while dragging splitters | Quantise to 8 px; measure. Worst case debounce until the drag ends (`on_rect_changed` + a 2-frame settle) |
| Bindless warning on Metal from bevy_egui | Cosmetic; set `bindless_mode_array_size: None` on macOS to silence |
| Keys firing in text fields | `keyboard_free` on every binding; test by typing a name containing `r`, `k`, `s` with Shift |
| Prop re-hydration resets plugin runtime state | Commit on release/blur, never per frame; document in inspector tooltip |
| Colour editing of linear light colours | Explicit linear↔egui conversions with a round-trip test |
| Chunk pinning by dirty state during long sessions | Visible in status bar; unchanged behaviour; autosave is a later option |
| `UserSettings` ABI change breaks stale plugin dylibs | ABI 5 gate rejects them with the existing warning; rebuild with `cargo build --workspace` |
| cafe-sim's world→screen popups | Untouched: they run only in Gameplay, where the render is still full-window |

---

## 7. Decisions (settled 2026-10-24)

1. **Groups are organisational, not transform parents.** ECS parenting
   would make a child's persistence depend on a parent that may live in
   another chunk and stream out separately; local transforms would also
   break the per-instance lightmap UV budgeting that reads world-space
   triangle size. If you want true hierarchies later, they can be layered
   on groups (record local transforms, resolve at spawn) once chunk
   ownership of a hierarchy is defined.
2. **The engine stops pausing in editor mode** after Phase 4, and the
   editor gets no settings window. Escape becomes cancel/deselect.
3. **Layout persistence is skipped in v1.** The dock resets to the default
   layout each run; §4.14 describes the later option.
4. **Terrain lower = Ctrl+LMB.** Alternative: a Raise/Lower toggle in the
   Terrain panel with plain LMB.
5. **Fly only while right-dragging**, speed via scroll while looking.
   Alternative: always-on WASD when the viewport is hovered, which costs
   the single-key tool hotkeys.
