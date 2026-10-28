# Prefabs (linked instances) + Prefab catalog tab — Plan

Status: **draft 2026-10-28, awaiting decisions in §6.** Decisions already
settled by the user: linked instances (edit the prefab, every instance
updates), global scope, no nested prefabs for now, single-entity prefabs are
allowed, placement goes through the Place tool, and the Place tab simply
gains a "Prefabs" tab listing created prefabs.

---

## 1. What a prefab is built on (facts from the code)

| Fact | Where | Consequence |
|---|---|---|
| A `PlacedEntityRecord` is a complete memento: transform, kind, name, scripts with props, parent | `objects.rs` | A prefab definition is a list of records with *local* ids and root-relative transforms. No new per-entity data model |
| Groups are organisational: members keep **world** transforms, a group move cascades a matrix delta to loaded members | `edits.rs` `commit_placed_edits` | An instance root can be a `Group`; moving/rotating/scaling it already moves the members. Root-relative member transforms are a *derived* quantity, computed only at define/expand time |
| Chunk `.objects` files are spawned record by record on chunk load; entities exist only for loaded chunks | `streaming.rs`, `spawn_chunk_objects` | Propagating a prefab edit to instances in **unloaded** chunks is only possible if instances are expanded from the prefab *at load time* rather than stored as baked members |
| Placed ids key baked object lightmaps and `PropValue::EntityRef` | `baking/object_lightmaps.rs`, `engine-api/script.rs` | Instance members need **stable ids per instance** across re-expansion, or every prefab edit throws away the bakes and breaks refs |
| `PropValue::EntityRef(u64)` is the only cross-entity link | `engine-api/script.rs` | Refs *inside* a prefab are remapped local→instance id on expand; refs pointing out of the prefab are left alone with a warning |
| WASM cannot list directories; `build.rs` writes a manifest for meshes | `build.rs`, `catalog.rs` | Prefab discovery must also be manifest-driven (or OPFS-listed on web). A prefab created at runtime must be registered in memory immediately, not rediscovered |
| `PlacementConfig { kind, scale, rotation, stamp_script }`; the preview is one scene for `Mesh` kinds, a gizmo otherwise | `object_placement.rs` | Placement needs a `prefab: Option<String>` and a multi-entity ghost preview |
| Undo `Change::Spawn/Despawn` carry one record and go through `spawn_entity_record` | `history.rs` | If expansion lives inside the spawn path, undo of place/delete/duplicate for instances is free |
| Objects file is v5; v2/v3/v4 legacy readers exist and upgrade in memory | `objects.rs` `legacy` | Adding a field is a routine **v6** bump with a v5 reader |

## 2. Recommended design

### 2.1 Prefab asset

`assets/prefabs/{name}.prefab`, rkyv like everything else:

```rust
pub struct PrefabFile {
    pub version: u32,          // PREFAB_FORMAT_VERSION = 1
    pub name: String,
    /// entities[0] is the root (a Group for multi-entity prefabs, or the
    /// single entity itself). `id` is a local id 0..n; `parent` refers to
    /// local ids; transforms are relative to the root.
    pub entities: Vec<PlacedEntityRecord>,
}
```

A `PrefabLibrary` resource holds every loaded definition by name. Discovery:
`build.rs` adds `prefabs/*.prefab` to a general manifest; a runtime "Save as
prefab" writes the file **and** inserts into the library (native: under
`assets/prefabs/`; web: OPFS `prefabs/` key, merged into the library at
startup). Nested prefabs: a definition may not contain an instance root
(refused with a message); revisit later.

### 2.2 Instance on disk — expand on load (objects v6)

`PlacedEntityRecord` gains one optional field:

```rust
pub prefab: Option<PrefabInstance>,

pub struct PrefabInstance {
    pub name: String,
    /// Instance id for each prefab local id (index = local id). Stable
    /// across re-expansion, so lightmaps and EntityRefs survive edits.
    pub member_ids: Vec<u64>,
    pub overrides: Vec<PrefabOverride>,
}

pub enum PrefabOverride {
    /// This member differs from the definition: the whole local record wins.
    Member { local_id: u64, record: PlacedEntityRecord },
    /// This member was deleted in this instance.
    Removed { local_id: u64 },
}
```

Only the **root** record is written to the chunk file. Members get
`PlacedObject.prefab_member_of = Some(root id)` at runtime and are **skipped
by `save_placed_objects`**; on chunk load the root is expanded from the
library (ids from `member_ids`, world transform = root × local, refs
remapped, overrides applied). This is what makes propagation to unloaded
chunks automatic: there is nothing stale on disk to update.

Extras (entities the user parents under an instance root that aren't in the
definition) are ordinary entities and save normally.

Consequences to accept:
- Members exist only while the **root's** chunk is loaded. An instance
  straddling a chunk boundary loses its far members when the root's chunk
  unloads (same class of issue groups already have).
- A missing prefab asset on load **unpacks** the instance: root becomes a
  plain group with no members, warning logged (the alternative, failing the
  chunk, is worse).

### 2.3 Editing model — in place, Unity-style

No separate prefab-edit mode. You edit an instance's members with the normal
tools; the inspector header on a root or member shows
"Instance of *name*" with:

- **Apply to prefab** — recompute the definition from this instance (members
  made root-relative, instance ids → local ids), write the asset, bump the
  library, **re-expand every loaded instance** (despawn members, respawn
  from the new definition keeping `member_ids`; new members get fresh ids
  appended, removed members' ids drop). Lighting goes stale.
- **Revert instance** — drop this instance's overrides and re-expand.
- **Unpack** — root becomes a plain group, members plain entities (saved
  normally from then on).

Overrides are computed at **save time** by diffing each member's current
local record against the definition (`Member` override if different,
`Removed` if the member entity is gone). Nothing per-field: an overridden
member stops receiving prefab updates for *any* of its fields. Simple and
predictable; field-level merge can come later if it hurts.

### 2.4 Placement + Prefabs tab

- `PlacementConfig.prefab: Option<String>`; selecting a prefab entry sets it
  (and clears `stamp_script`); selecting a mesh clears it.
- Place tab becomes two sub-tabs: **Meshes** (today's grid + lights/empty/
  group + plugin presets) and **Prefabs** (one button per library entry,
  with a "Save selection as prefab…" button and a name dialog at the
  bottom). `MeshCatalog` is left alone; a `PrefabLibrary` feeds the second
  tab.
- Preview: spawn the definition's mesh members as translucent ghost scenes
  under one preview root (`setup_placement_preview` grows a prefab branch).
  Scale/rotate apply to the root.
- Click: build the root record (`prefab: Some(..)`, fresh `member_ids` via
  `alloc_id`), spawn through `spawn_entity_record`, which expands. Undo
  records one `Change::Spawn` of the root record — undo despawns root +
  members, redo re-expands with the same ids.

### 2.5 Undo integration

- Spawn/Despawn of an instance root: expansion/collapse lives in the spawn/
  despawn helpers, so existing `Change`s cover it. `delete_placed` on a root
  behaves like a recursive group delete (members have no life of their own).
- `Change::PrefabDefinition { name, before: PrefabFile, after: PrefabFile }`
  for Apply; `Change::PrefabOverrides { id, before, after }` for Revert and
  Unpack (`prefab: Option<PrefabInstance>` before/after).

### 2.6 Phasing

| Phase | Content | Size |
|---|---|---|
| 1 | Asset + library + manifest, objects v6 field + v5 reader, expand-on-load, save skipping members, "Save selection as prefab", Prefabs tab, placement + ghost preview, undo for place/delete | ~900 lines |
| 2 | Overrides diffing at save, Apply/Revert/Unpack, live re-expansion of loaded instances, undo for Apply/Revert/Unpack, docs | ~500 lines |

Phase 1 alone already gives "edit the prefab file, instances update on next
load"; phase 2 gives live propagation and overrides.

## 3. Risks

| Risk | Mitigation |
|---|---|
| Expand-on-load + a member in a neighbouring chunk that is loaded first | Members are spawned by the root only; a chunk file never contains members. Document the straddle limitation |
| `member_ids` drift when the definition adds/removes members | Index = local id; Apply appends ids for new local ids and truncates removed ones; the diff is by local id, never by position |
| Re-expansion resets hydrated plugin state | Same as any re-hydration today; editor-only operation |
| A prefab deleted from disk | Instances unpack with a warning on load |
| Web write path for new prefabs | OPFS mirror of `prefabs/`, merged at startup; same pattern as projects |

## 4. Decisions needed

1. **Location/format**: `assets/prefabs/*.prefab` (rkyv) listed by the build
   manifest, with runtime saves writing into `assets/` on native and OPFS on
   web. OK, or would you rather have a human-readable format (TOML/RON) for
   prefabs since they're authored content?
2. **Editing model**: in-place on an instance with Apply / Revert / Unpack
   (§2.3), no separate prefab-edit mode. Agree?
3. **Override granularity**: whole-member records (an overridden member
   stops tracking the prefab entirely) vs field-level merge. Recommend
   whole-member for v1.
4. **Instance storage**: expand-on-load with members not persisted (§2.2),
   accepting the chunk-straddle limitation. Agree?
5. **Missing prefab asset on load**: unpack to a plain group with a warning?
6. **Extras** (entities parented under an instance but not in the
   definition): plain entities that never propagate, until "Apply" folds
   them in? That is what §2.2 proposes.
7. Should "Save selection as prefab" on a **non-group** entity make a
   single-entity prefab whose root *is* that entity (no wrapping group)? I
   assume yes given your "scripted element tied to a mesh" case.
