# Editor Undo/Redo — Feasibility and Plan

Status: **draft 2026-10-28, awaiting decisions in §6.** Scope: one undo stack
covering every change to *level data* made from the editor. Follow-ons
(prefabs, asset browser) are surveyed in `§7` and `§8` but not planned here.

---

## 1. Findings — what the code does today that shapes the design

Verified against the code on 2026-10-28.

### 1.1 Every mutation path, and how it fires

| Level data | Where it is mutated | Shape |
|---|---|---|
| Transform | Inspector drag fields (`ui/inspector.rs` `transform_fields`) | **Per frame** while dragging; `PlacedEdited::Transform` after the write |
| Transform | Gizmo drag (`gizmo.rs`) | Per frame; explicit **start** (`GizmoDrag.start`), **release**, **Esc cancel** (restores start) |
| Transform (+chunk) | Grab + drop (`grab.rs`) | Discrete: despawn on grab, respawn **same id** on drop; cancel paths restore |
| Transform (members) | Group move cascade in `commit_placed_edits` | Derived: delta vs a per-group snapshot, applied to loaded descendants |
| Name | Inspector header, outliner inline rename | Discrete, on focus loss; `PlacedEdited::Name` |
| Kind params (light colour/intensity/range/angles) | Inspector `kind_fields` | Per frame; `PlacedEdited::Kind` |
| Kind asset / collider mode | Inspector | Discrete; `RespawnPlaced::InPlace` → **new `Entity`, same id** |
| Parent | `EditorAction::SetParent`, `GroupSelection`, `UngroupSelection`, non-recursive delete (`delete_placed` re-files members) | Discrete, in `actions.rs` / `selection.rs` |
| Existence | `handle_place_click` (alloc id), `RespawnPlaced::Duplicate` (alloc id, +1 m x), `GroupSelection` (alloc id), delete / Shift+Delete, grab **Delete** (payload dropped) | Discrete |
| Script bindings | `ScriptEdit::{Attach, Detach, SetProps}` in `edits.rs`; stamp at placement | Discrete; `Attach` uses **schema defaults only** (no way to re-attach with given props) |
| Terrain heights | `apply_terrain_brush` writes tiles per frame; `finalize_terrain_lod` on release | **Stroke** = LMB press → release, tracked by `TerrainBrushHit.was_painting` |
| Environment | Inspector `environment()` writes `TerrainWorld.metadata.environment` | Per frame; `apply_environment` reacts to resource change detection |
| Plugin level data | `LevelDataHooks` at save time | Plugin-owned gameplay state, written only on save; **not editor data** |

Not level data (tool state): `EditorTool`, `Selection`, camera, `BrushSettings`,
`PlacementConfig`, `UserSettings` view toggles, dock layout.

Irreversible / external: Save, Bake, world switch/create, plugin
add/remove/reload, exit to menu.

### 1.2 Facts the design leans on

1. **`Entity` is not a stable identity; `PlacedObject.id: u64` is.** Respawn
   (asset/collider change), grab, and chunk unload/reload all produce a new
   `Entity` for the same id. `PlacedObjectRegistry.by_id` resolves id →
   entity for loaded ones. The history must be keyed by id and resolve at
   apply time.
2. **`PlacedEntityRecord` is already the complete memento** of a placed
   entity (`PlacedObject::to_record`), including bindings with props and
   `parent`. Spawn/despawn undo needs nothing new in the data model.
3. **Entities exist only for loaded chunks**; `HeightmapData` tiles are
   evicted on unload. Dirty chunks are pinned (`update_streaming_ring`
   filters on both dirty sets), so an *unsaved* edit's chunk cannot unload
   under the history. After a save it can. §3.6 handles this.
4. **The group cascade lives in `commit_placed_edits`** and measures a delta
   against a snapshot. Replaying a group transform through the same path
   would cascade *again*; a member unloaded during the original move but
   loaded at undo time would be moved wrongly. Undo must be able to set
   transforms **exactly, without cascade**.
5. **Bindings only take effect through re-hydration** (`detach_binding` +
   `NeedsHydration`); the host skips already-hydrated pairs. Restoring a
   previous binding list therefore means: detach everything that was
   hydrated, set the list, re-arm.
6. **The inspector already solved "when did the user let go"** for script
   props: `dirty && !interacting && !popup_open && !egui_is_using_pointer()`.
   The same predicate closes coalesced history entries.
7. **`PlacedEdited` is `Copy` and carries no "before" value.** Recording in
   `commit_placed_edits` (the seam the design docs reserved for undo) needs
   the pre-edit value; the cheapest way is for the message to carry it.
8. **Side finding (pre-existing bug):** sculpting regenerates chunk meshes
   and the `Aabb` but **never rebuilds the heightfield `Collider`**, which is
   built once in `compute_chunk_meshes`. Physics stays on the old heights
   until the chunk reloads. The undo rebuild helper is the natural place to
   fix it (§3.5).
9. `PlacedKind`, `Transform`, `EnvironmentData`, `ScriptBinding` are all
   `PartialEq`; `PlacedObject` is not (irrelevant — we compare fields).
10. Headless `App`/`World` tests already exist for selection and delete
    (`selection.rs` tests), so every history round trip can be unit-tested
    without a window.

---

## 2. Design in one paragraph

A `History` resource holds two stacks of `Transaction`s. A transaction is a
label plus a list of `Change`s, each a **before/after pair of plain data keyed
by placed id** (never `Entity`), plus the selection before and after. Undo
applies the changes in reverse order with before/after swapped; redo replays
them. Changes are applied through one `LevelOps` system param that owns the
raw mutations (spawn record, despawn id, set transform exactly, set name, set
kind (+respawn), set parent, set bindings, set heights, set environment) and
does the bookkeeping every mutation needs (dirty chunks, `LightingState`,
mesh rebuilds). Recording happens at the existing choke points —
`commit_placed_edits`, `apply_script_edits`, `respawn_placed`,
`delete_placed`, place/grab/group actions, the terrain stroke end, and the
environment panel — so no panel can mutate level data without a record.
Continuous edits (drags) coalesce into one open transaction per `(kind, id)`
and close when the user lets go.

---

## 3. Detailed design

### 3.1 Types (`src/editor/history.rs`)

```rust
#[derive(Resource, Default)]
pub struct History {
    undo: Vec<Transaction>,
    redo: Vec<Transaction>,
    /// Total bytes of terrain tiles held, for the memory cap (§3.7).
    terrain_bytes: usize,
}

pub struct Transaction {
    /// Shown in the Edit menu: "Undo Move 'Rock'".
    pub label: String,
    pub changes: Vec<Change>,
    pub selection_before: Option<u64>,
    pub selection_after: Option<u64>,
    /// Consecutive pushes with the same key fold into this transaction
    /// while it is `open` (drag coalescing).
    pub merge_key: Option<MergeKey>,
    pub open: bool,
}

#[derive(PartialEq, Eq, Clone, Copy)]
pub enum MergeKey {
    Transform(u64),
    Kind(u64),
    Environment,
}

pub enum Change {
    /// Entity came into existence (undo = despawn by id).
    Spawn { chunk: (i32, i32), record: PlacedEntityRecord },
    /// Entity was removed (undo = spawn the record, same id).
    Despawn { chunk: (i32, i32), record: PlacedEntityRecord },
    Transform { id: u64, before: Transform, after: Transform },
    Name { id: u64, before: String, after: String },
    /// `respawn` when the asset path or collider differs (needs a new entity).
    Kind { id: u64, before: PlacedKind, after: PlacedKind },
    Parent { id: u64, before: Option<u64>, after: Option<u64> },
    Scripts { id: u64, before: Vec<ScriptBinding>, after: Vec<ScriptBinding> },
    /// Sparse: only the tiles a stroke touched. `(tx, ty, before, after)`.
    Terrain { tiles: Vec<(i32, i32, f32, f32)> },
    Environment { before: EnvironmentData, after: EnvironmentData },
}
```

`History` API: `push(Transaction)` (clears redo), `merge_or_push(tx)` (folds
into the top if `open && merge_key` matches, keeping the first `before` and
the latest `after`), `close_open()`, `pop_undo()/pop_redo()`,
`undo_label()/redo_label()`, `clear()`, `pinned_chunks()` (§3.6).

### 3.2 Applying a change — `LevelOps`

A `SystemParam` in `history.rs` bundling what the raw operations need:
`Commands`, `AssetServer`, `PlacedObjectRegistry`, `PlacedTree`,
`Query<&mut ScriptBindings>`, `MessageWriter<PlacedEdited>`,
`MessageWriter<RespawnPlaced>`, `Option<ResMut<TerrainWorld>>`,
`Option<ResMut<HeightmapData>>`, `Option<ResMut<TerrainChunkRegistry>>`,
`Assets<Mesh>`, chunk query, `Selection`, `LightingState`.

`apply(change, Direction)` resolves `id → Entity` through `registry.by_id`
and performs:

| Change | Apply (direction already folded into before/after) |
|---|---|
| Spawn / Despawn | `spawn_entity_record` (same id, `mark_dirty`) / despawn + `by_id.remove` + `mark_dirty`. **No member re-filing** here: `delete_placed` records those as explicit `Parent` changes in the same transaction (§3.3), so apply is exact |
| Transform | Write `Transform`, send `PlacedEdited::transform_exact(entity)` so `commit_placed_edits` re-derives the chunk and flags lighting **without cascading** (§3.3) |
| Name | Write `PlacedObject.name`, send `PlacedEdited` name edit so `Interactable` labels follow |
| Kind | Write `PlacedObject.kind`; if asset path / collider changed, `RespawnPlaced::InPlace(entity)`; else `PlacedEdited` kind edit |
| Parent | Write `PlacedObject.parent`, `mark_dirty` |
| Scripts | Queue `detach_binding` for every binding in the *current* list, set `ScriptBindings = target` (remove the component if empty), insert `NeedsHydration`, `mark_dirty`. Re-hydrating untouched bindings is harmless in the editor and keeps the op exact |
| Terrain | Write tiles into `HeightmapData`; compute affected chunks (same shared-edge rule as the brush); `rebuild_chunk(chunk, all LODs)` (§3.5); `dirty_chunks.insert` |
| Environment | Write `TerrainWorld.metadata.environment` (through `ResMut`, so `apply_environment` reacts); `lighting.stale = true` |

A change whose target id is **not loaded** cannot be applied. Policy (§3.6):
the whole transaction is skipped and dropped with a status-bar message.

After applying, `Selection.primary` is set from the transaction's
`selection_before` (undo) or `selection_after` (redo), when that id is loaded.

Ordering: `apply_undo_redo` is the **first** system in the existing
`EditorSet::InGame` chain (before `apply_level_actions` … `respawn_placed`),
so the `PlacedEdited` / `RespawnPlaced` messages it emits land the same frame.

### 3.3 Recording — where each transaction is pushed

**`PlacedEdited` carries the before value** (drops `Copy`, keeps `Clone`):

```rust
pub enum EditKind {
    Transform { before: Transform, cascade: bool },
    Name { before: String },
    Kind { before: PlacedKind },
}
```

Constructors keep call sites short: `PlacedEdited::transform(entity, before)`,
`::transform_exact(entity, before)` (cascade = false — used by undo),
`::name(..)`, `::kind(..)`. Every existing sender already has the before
value in hand (inspector: clone before the widget; gizmo: last frame's
transform; outliner: `obj.name` before overwrite).

`commit_placed_edits` becomes the recorder for these three kinds:

- Transform: `merge_or_push` a transaction keyed `MergeKey::Transform(id)`,
  `open = true`, with `Change::Transform { before, after }`. If the entity is
  a group and `cascade` is set, the member moves the cascade just performed
  are appended to the **same** transaction as their own `Change::Transform`s
  (before = pre-cascade, after = post-cascade). Undo thus restores members
  exactly, regardless of what is loaded later.
- Kind: same, keyed `MergeKey::Kind(id)`.
- Name: `push` (discrete), label `Rename 'old' → 'new'`.

The gizmo keeps its explicit lifecycle: on release it calls
`history.close_open()`; on Esc cancel it restores the start transform and
sends `transform_exact` *and* then **pops the open transaction** (a cancelled
drag leaves no entry). Gizmo per-frame sends are otherwise unchanged.

**`apply_script_edits`** pushes `Change::Scripts { before, after }` per edit
(labels "Attach x/y", "Detach x/y", "Edit x/y props"). `ScriptEdit::Attach`
gains `props: Option<Vec<ScriptProp>>` (None = schema defaults) so the
inverse of a detach can re-attach with the old props — but since undo goes
through `Change::Scripts` directly, this is only needed if a panel wants it;
keep it minimal.

**`respawn_placed`**: `InPlace` records nothing (the `Kind` change that
caused it already did). `Duplicate` pushes `Spawn { record }` with the new
id, label "Duplicate 'name'".

**`delete_placed`** (both callers) pushes one transaction: for a non-recursive
group delete, `[Parent{member, before: Some(group), after: group.parent}…,
Despawn{group}]`; recursive: `[Despawn{member}…, Despawn{group}]`; plain:
`[Despawn]`. Undo applies in reverse: spawn the group, then restore parents.

**`handle_place_click`** pushes `Spawn` ("Place 'kind'").
**`GroupSelection`** pushes `[Spawn{group}, Parent{child}]` ("Group").
**`UngroupSelection`** is a non-recursive delete (already covered).
**`SetParent`** pushes `Parent`.

**Grab**: on drop, push `Transform { before: origin record transform, after:
dropped }` ("Move 'name'") — the respawn kept the id, so a transform change
is the truthful model (chunk re-derivation happens in the apply). On
**Delete while carrying**, push `Despawn { record, origin_chunk }`. Cancel
paths push nothing.

**Terrain** (`terrain_edit.rs`): `TerrainBrushHit` gains
`stroke_before: HashMap<(i32, i32), f32>`; `apply_terrain_brush` inserts a
tile's old height on its first write in the stroke (`entry().or_insert`).
Where `finalize_terrain_lod` flips `was_painting` to false, build
`Change::Terrain` with after = current heights, label by brush mode
("Sculpt terrain" / "Flatten terrain" / "Smooth terrain"), and push. One
entry per stroke.

**Environment** (`ui/inspector.rs` `environment()`): where `env != before`
already triggers the write, also `merge_or_push` keyed
`MergeKey::Environment`, label "Edit environment". Panels get `History`
through `Panels` (`ResMut<History>`).

**Settling open transactions**: a small `settle_history` system (end of
`EditorSet::InGame`) calls `history.close_open()` when the left button is up,
`!EguiWantsInput::is_using_pointer`-equivalent is true and no egui popup is
open — the same predicate the props draft uses, computed once in the egui
pass and published on `ViewportInput`/a tiny `EguiBusy` resource. A merge
with a *different* key also closes the previous one.

### 3.4 Input and UI

- `EditorAction::Undo` / `EditorAction::Redo`, handled by `apply_undo_redo`.
- Keys: `Ctrl+Z` undo, `Ctrl+Shift+Z` and `Ctrl+Y` redo; on macOS also
  `Cmd` (`SuperLeft/Right`). Gated on `keyboard_free` — while an egui text
  field has focus, egui's own text undo takes the keystroke, which is what
  users expect. `Ctrl` is already a gizmo/terrain modifier; a `just_pressed`
  `Z` check does not interfere (the gizmo may flash rotation arcs for the
  duration of the press — cosmetic).
- Guards: undo/redo is ignored while a gizmo drag or terrain stroke is in
  progress. While carrying an entity it first puts the entity back (Esc
  semantics) and does nothing else that press.
- Edit menu: "Undo <label>\tCtrl+Z", "Redo <label>\tCtrl+Shift+Z", disabled
  with plain "Undo"/"Redo" when a stack is empty. Status-bar rows in
  `controls.rs` (`Base` context).
- Optional (phase 3): a **History** dock tab listing transactions, click to
  jump (multi-step undo/redo).
- World switch, create, and exit to menu call `history.clear()`. Switching
  to Gameplay keeps the history (plugins do not edit level data).

### 3.5 Shared terrain rebuild helper (and the collider fix)

Factor the per-chunk mesh regeneration duplicated between
`apply_terrain_brush` (LOD 0 + Aabb) and `finalize_terrain_lod` (LODs 1..n)
into `terrain_edit::rebuild_chunk(chunk, lods: RangeInclusive<usize>, …)`.
The undo apply calls it with all LODs. While there, rebuild the heightfield
`Collider` at stroke end / undo (finding §1.2.8). Collider construction is
what the bootstrap does off-thread; measure on-thread cost first, and gate
behind the stroke-end path (never per frame).

### 3.6 Targets in unloaded chunks

Two complementary measures, both cheap:

1. **Pin**: `History::pinned_chunks()` returns every chunk referenced by any
   transaction (spawn/despawn chunk, current chunk of each id via the
   registry, terrain tile chunks). `update_streaming_ring`'s unload filter
   also checks it, exactly like the two dirty sets. With the caps in §3.7
   this bounds how much extra terrain stays resident.
2. **Fail gracefully**: if resolution still fails (e.g. the chunk never
   loaded after a world reload — history is cleared on switch anyway, so this
   is a belt-and-braces path), skip the transaction, drop it from the stack,
   and show "Can't undo: entity in an unloaded chunk" in the status bar.

### 3.7 Caps

- 200 transactions, FIFO eviction from the bottom of the undo stack.
- Terrain tile bytes capped at 64 MB total (a 5 m stroke is ~25 KB; a
  50 m radius worst case ~2.5 MB), evicting oldest terrain-bearing
  transactions first.

### 3.8 Tests (headless, `history.rs` + existing test style)

For each `Change` kind: apply forward → undo → assert state equals before →
redo → equals after. Specific cases:

- Delete group (keep members) → undo restores the group with the same id and
  every member's `parent`.
- Group move with one member → undo restores both transforms exactly;
  members are not double-moved.
- Coalescing: 10 per-frame transform edits with the same key produce one
  transaction whose `before` is the first value; a different key closes it.
- Gizmo cancel leaves the stack unchanged.
- Scripts: attach → undo removes the binding and the hydrated components
  (reuse the `script_host` test harness's fake registry).
- Terrain: a stroke's tiles restore and the chunk is dirty again.
- Unresolvable id skips and drops the transaction.
- Cap eviction and `pinned_chunks` contents.
- `editor_systems_have_disjoint_parameters` gets `apply_undo_redo` and
  `settle_history`.

---

## 4. Phasing and size

| Phase | Content | Size |
|---|---|---|
| 1 | `history.rs` (types, stacks, `LevelOps`, apply), `PlacedEdited` with before values, recording for every placed-entity path (transform, name, kind, parent, scripts, spawn/despawn, grab, group ops), `Ctrl+Z/Y`, Edit menu, tests | ~700–900 lines, the bulk |
| 2 | Terrain strokes: `stroke_before`, `rebuild_chunk` helper, collider fix, pinning in streaming, caps | ~250 lines |
| 3 | Environment coalescing, `settle_history` polish, History dock tab, docs (`CLAUDE.md` editor model, `EDITOR_UI_DESIGN.md` §5) | ~150 lines |

Phase 1 is self-contained and shippable. Nothing touches the plugin ABI or
any file format.

---

## 5. Risks

| Risk | Mitigation |
|---|---|
| Double cascade on group undo | `cascade: bool` on the transform edit; members recorded explicitly (§3.3); unit test |
| A mutation path added later bypasses recording | Recording sits in the commit/apply systems, not panels; `PlacedEdited` *requires* a before value, so the compiler forces callers to think about it |
| Coalescing closes too eagerly (colour picker: each wheel drag becomes an entry) | Acceptable v1; tune the settle predicate (idle timeout) if it feels wrong |
| Terrain memory growth | Byte cap §3.7 |
| Pinned chunks keep terrain resident | Bounded by the caps; status bar can show pinned count if needed |
| `Ctrl+Z` on macOS where `Cmd+Z` is habit | Accept both |
| One-frame latency between egui pass and `Update` | Same as every other editor message; invisible |

---

## 6. Decisions needed

1. **Selection / camera / tool in the stack?** Proposed: no. Each transaction
   restores the selection it affected, which gives the "undo brings back
   what I was looking at" feel without polluting the stack.
2. **History survives save?** Proposed: yes, with chunk pinning (§3.6).
3. **Terrain granularity = one entry per stroke?** Proposed: yes.
4. **Keep history across F1 → Gameplay → F1?** Proposed: yes; cleared only on
   world switch / exit.
5. **Bundle the heightfield collider fix** (§1.2.8) into phase 2? Proposed:
   yes, it is the same helper.
6. **Keybinds**: `Ctrl+Z` / `Ctrl+Shift+Z` / `Ctrl+Y`, plus `Cmd` on macOS?

---

## 7. Prefabs — findings and questions (not planned yet)

What exists that a prefab system can stand on:

- `PlacedKind::Group` + `parent: Option<u64>` is already a subtree container
  with an origin; members keep **world** transforms (design §7 of
  `EDITOR_UI_DESIGN.md`). A prefab is naturally "a group subtree saved as a
  file", with member transforms made **group-relative** on save and
  re-absolutised on instantiate.
- `PlacedEntityRecord` is a complete, rkyv-serialisable memento, bindings and
  props included. A `.prefab` file is `Vec<PlacedEntityRecord>` with local
  ids, plus a name and a version. Instantiation allocates fresh ids and
  remaps `parent` and any `PropValue::EntityRef` **inside** the prefab;
  refs pointing outside are left as-is with a warning.
- Plugin `default_mesh` presets are one-entity prefabs in all but name; a
  prefab catalog could subsume them later.
- Ids key baked object lightmaps, so instances never share bakes — correct,
  they sit in different light.
- Undo: instantiation is one transaction of `Spawn` changes; "save as
  prefab" touches no level data. Fits §3 unchanged.
- Storage: project-scoped files via the existing storage layer work on
  native and OPFS (projects already persist that way).

Questions:

1. **Stamp copies or linked instances?** A copy-on-place prefab needs no
   level-format change and ships quickly. Linked instances (edit the prefab,
   every instance updates; per-instance overrides) need a `prefab:
   Option<PrefabRef>` on the group record (objects **v6** + legacy reader),
   an override-diff model, and a propagation pass. Recommend copies first,
   but reserve the field so v6 is additive.
2. **Scope**: project (`projects/{id}/prefabs/`), world, or global
   `assets/prefabs/`? Project seems right (plugins are per project, and
   bindings reference plugin scripts).
3. **Nested prefabs** — defer?
4. Should a single non-group entity be saveable as a prefab (yes, trivially:
   a one-record prefab)?
5. Does placing a prefab go through the Place tool (preview = the prefab's
   meshes under the cursor)? The placement preview currently handles one
   scene; a multi-entity preview is a small extension of
   `setup_placement_preview`.

## 8. Asset browser — findings and questions (not planned yet)

Today's catalog:

- `build.rs` writes `assets/meshes/manifest.manifest` (first `.gltf/.glb`
  per sub-directory, `character/` excluded) because **WASM cannot list
  directories**. `MeshCatalog` = manifest + lights/empty/group + plugin
  presets, drawn as text buttons in the Place tab. There are only two
  textures in the tree (`assets/textures/terrain.png`, the cafe brick wall),
  and nothing consumes a texture from the level data yet.

Feasibility:

- **Discovery**: extend `build.rs` into a general manifest
  (`kind|name|path`, all of `assets/` by extension: gltf/glb, png/jpg, ogg/wav,
  prefab) — same mechanism, works on both platforms. Native could add a
  runtime "Rescan" using `read_dir`; `AssetServer::load_folder` exists in
  Bevy 0.20 but is unsupported on the web, so it is no help for parity.
  Plugin assets under `assets/{plugin}/` can be grouped under the plugin's
  name and filtered by the active project's plugin list.
- **Texture previews**: trivial. `Handle<Image>` → `EguiUserTextures::add_image`
  → `egui::Image`; the viewport already does exactly this with the render
  target.
- **Mesh and prefab thumbnails**: need an offscreen thumbnail camera: a
  dedicated `RenderLayers` layer, a small `Image` render target per entry,
  spawn the scene, render for a frame or two, cache the texture id, despawn,
  next entry (frame-budgeted). ~250 lines; the render-to-texture plumbing
  already exists for the viewport. Fallback icon until the thumbnail lands.
- **Audio**: `bevy_audio` is in the default features, so `Handle<AudioSource>`
  loads; preview = icon + play button. Nothing in the level references
  audio yet, so this is browse-only until a consumer exists.
- **UI**: a new dock tab (`EditorTab::Assets`) with category tabs, a search
  box and a thumbnail grid. Selecting a mesh/prefab arms `PlacementConfig`
  exactly as `MeshCatalog::select` does (and could switch to the Place tool);
  the Place tab keeps scale/yaw/stamp only. Drag-from-browser-into-viewport
  is possible with egui dnd + `ViewportInput` (drop position → `EditorRay`).
- `MeshCatalog` becomes an `AssetCatalog` with an `AssetKind`; the preset
  stamp logic in `select()` is kept.

Questions:

1. **"Assets related to the project"** — today everything is global under
   `assets/`, plugin assets under `assets/{plugin}/`. Do you want a
   project-scoped asset directory (`projects/{id}/assets/`, registered as its
   own asset source like `world://`)? That changes where the manifest comes
   from and how the web build ships assets.
2. What should **selecting a texture do** right now? Preview/inspect only,
   or is terrain texture painting / per-object material override on the
   roadmap (which decides whether textures need a level-data consumer)?
3. Thumbnails: rendered with the sun + `StandardMaterial` (fast, generic) or
   through the toon pipeline (matches the viewport, needs a baked lightmap
   placeholder)? Recommend the former.
4. Is the Place tab's catalog grid removed once the Assets tab exists, or
   kept as a compact "recent" strip?
