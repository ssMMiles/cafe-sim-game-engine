# Lighting plugin-spawned objects — investigation

Status: **investigation, 2026-10-30.** Nothing below is implemented. §5 lists
the decisions needed. Every claim about bevy was checked against the 0.20.0
sources in the cargo registry, every claim about the engine against this tree.

Background: `docs/DYNAMIC_LIGHTING_PLAN.md` §8 (the probe volume as built),
`docs/LIGHTING.md` (the baked system), `docs/PLUGIN_LEVEL_ENTITY_CONTRACT.md`
(what a plugin may do to a level entity).

---

## 1. What cafe-sim spawns, and how it is lit today

| Thing | Spawn site | Visual | Lit by |
|---|---|---|---|
| Furniture (table, counter, machine, bin) | `placement.rs::spawn_piece` — `WorldAssetRoot` glTF scene + `AutoCollider` | glTF `StandardMaterial`, or `PaintMaterial` swaps in a tinted `StandardMaterial` once meshes appear | `BakedSun` + `GlobalAmbientLight` (PBR path) |
| Placement ghost | `placement.rs::sync_preview` | `PaintMaterial` at alpha 0.55 | same |
| Customers, staff | `customer.rs`, `staff_agent.rs` — one cube mesh + one `StandardMaterial { unlit: true }` each | flat colour | nothing (unlit) |
| Drinks (ready, delivered, carried, staff-carried) | `delivery.rs`, `staff_agent.rs` | shared unlit yellow cube | nothing |
| Shop thumbnails | `thumbnails.rs` — private layer 8+, own camera + `DirectionalLight` | glTF / tinted PBR | its own light (fine as is) |

None of it is toon-shaded and none of it sees the baked world: a customer
standing under a placed lamp looks the same as one outside. The shell itself is
level data and is baked correctly; the gap is everything the plugin spawns
*into* it at runtime.

Furniture is "static but spawned in gameplay": it exists only while the plugin
is active, so it is never in an editor bake — no baked shadow on the floor
under a table, and no lightmap of its own.

## 2. What the engine already provides

The receiving side is solved in principle. `ToonSource::ProbeVolume` samples
four shared 3D textures per pixel at the fragment's world position; there is
**no per-object CPU work and no per-object GPU data**. Any number of dynamic
objects share the same four texture handles, and the volume's rebuild cadence
(re-centre on focus, half-second coalesce on store churn) does not depend on
how many objects read it. Cost per probe-lit fragment versus a lightmapped one:
three extra trilinear 3D samples (`c1`–`c3`; `c0` replaces the 2D lightmap tap)
and ~20 ALU for the L1 + sun reconstruction. Negligible next to the edge
detector the camera already runs.

`BlobShadow` is in engine-api and the host drives it: one avian ray straight
down per caster per frame, a shared disc mesh and material, height drives
scale not alpha. Tens of casters are free; hundreds would want measuring.

`DynamicLit` is in engine-api. `example2` shows the hand-built path: a plugin
constructs `ExtendedMaterial { base: StandardMaterial, extension:
ToonMaterial::new(ToonSource::ProbeVolume, default_lightmap, &probes) }` from
`DefaultToonLightmap` + `ProbeVolumeImages` (both resources are shared types).
`sync_toon_sun` fills the sun uniform on `AssetEvent::Added`, one frame late,
so a plugin material never needs to know the environment.

## 3. What blocks a plugin today

### 3.1 glTF scenes cannot be converted (hard blocker for furniture)

`ConvertToToon` is a host-private component (`gltf_loading.rs`), and the
conversion is an **entity-scoped observer**: `on_scene_ready` is attached with
`.observe()` by `spawn_interactable_scene` (placed objects, editor previews)
and by the player's spawn. A plugin cannot name the marker, and even if it
could, no observer would fire for its entity. There is no way for cafe-sim to
get the engine's conversion for `Tabletop.glb`.

Fix (engine, small): move `ConvertToToon` to engine-api next to `DynamicLit`
(ABI 10 — it is a ZST marker, but it is a new shared type) and register
`on_scene_ready` **once, globally** with `app.add_observer`, dropping the three
per-entity `.observe` calls. `WorldInstanceReady` is entity-targeted, so the
global observer sees every readied scene and does nothing for roots without
one of its markers (`Interactable`, `PendingMaterial`, `ConvertToToon`). Editor
previews and thumbnails spawn without `ConvertToToon` and are unaffected.

### 3.2 `PaintMaterial` fights the conversion (plugin-side)

cafe-sim's tint replaces every sub-mesh material with a fresh
`StandardMaterial` after the scene loads. If the scene were converted to toon
first, the paint would undo it; if painted first, the conversion's
`convert_to_toon_recursive` clones whatever `StandardMaterial` it finds — so
*painting first and converting second* actually works for the opaque placed
piece. Ordering between a plugin system and a host observer is not something to
rely on, though. Cleaner: when `tint` is set, cafe-sim builds the toon material
itself (`example2` style, base colour = tint, source = probes) and does not ask
for conversion; untinted pieces (the baked table) use `ConvertToToon +
DynamicLit`.

The **placement ghost** must stay `StandardMaterial`: `ToonMaterial`'s
`MaterialExtension::alpha_mode()` forces `Opaque`, so a translucent toon
material is impossible without a shader change. A PBR ghost under PBR lighting
is acceptable for a preview.

### 3.3 One material per mesh entity, and none of them batch (performance)

`convert_to_toon_recursive` allocates a new `ToonMat` per **mesh entity**, even
when ten entities share one glTF material. Verified in bevy 0.20:
`ToonMaterial` has no `#[bindless]`, and `combined_bind_group::
bindless_slot_count` only enables bindless when *both* halves are, so every
`ExtendedMaterial<StandardMaterial, ToonMaterial>` is its own bind group, and
`material_bind_group_index` is part of `Opaque3dBatchSetKey` — so every
converted sub-mesh is its own batch set, i.e. its own draw. For level objects
this is unavoidable (each sub-mesh has its own lightmap and
`object_lightmaps::unique_material` clones on purpose). For probe-lit objects
it is pure waste: nothing per instance lives in the material.

A full cafe is 240 cells; `Tabletop.glb` is a handful of sub-meshes; tens of
customers and drinks on top. Hundreds of draws is survivable on desktop but is
exactly the sort of count WebGPU in a browser tab notices, and it is cheap to
avoid:

- Host: a `HashMap<(AssetId<StandardMaterial>, ToonSource), Handle<ToonMat>>`
  cache consulted by the conversion when the source is `ProbeVolume`, so every
  dynamic instance of one glTF material shares one toon material. Same mesh +
  same material then batches into one instanced draw through bevy's normal
  path. Lightmapped conversions keep allocating (they need to).
- Plugin: one shared toon material per kind for cubes (customers, staff,
  drinks), exactly as the unlit `StandardMaterial`s are shared now.

### 3.4 Nothing shadows furniture, and furniture shadows nothing (quality)

A table placed at runtime is lit from the 2 m probe lattice at its position:
right on average, no AO under its top, no contact darkening on the floor, and
the floor's baked lightmap knows nothing about it. Options:

| | Cost | Result |
|---|---|---|
| **A. Accept it** | none | Toon-banded, consistent colour with the room; no grounding under tables. Characters and drinks are moving anyway |
| **B. `BlobShadow` under each piece** | one ray/frame/piece (static — could be made one-shot) | A soft disc under every table and counter. Reads as contact shadow in this art style; wrong shape for a long counter |
| C. Promote furniture to level data | large | Furniture would be baked, but it is per-save-slot gameplay state, not the level. Rejected |
| D. Runtime re-bake of the shell's lightmap when furniture changes | the object bake is CPU, inline in `Update`, seconds per bake | Correct and far too slow to run on every placement; a deferred "bake when idle" is a project of its own |

Recommend **A now, B as a one-line experiment** (the host already supports
it). D is where "properly lit" ultimately leads if the toon look makes the
missing grounding obvious; it needs the GPU object bake
(`docs/GPU_OBJECT_BAKE_PLAN.md`) first.

### 3.5 Probes inside the shell

The probe bake treats scenery hits as `0.5 · ambient` and skips probes below
terrain, but **not probes inside a BVH mesh** (plan §9). The cafe's walls are
thin; a 2 m lattice will put some probes inside them, and trilinear sampling
bleeds those into the neighbouring cells. Dynamic objects right against a wall
may read darker than they should. Watch for it; the fix is the all-hits parity
query the plan already names.

## 4. Proposed order of work

1. **Engine**: `ConvertToToon` → engine-api, global `on_scene_ready` observer,
   per-source material cache for probe-lit conversions. ABI 10. ~150 lines,
   one afternoon, plus a test that two dynamic instances of one glTF material
   end up on one `ToonMat` handle.
2. **cafe-sim characters/drinks**: swap the three unlit `StandardMaterial`s for
   three shared probe-lit `ToonMat`s; `BlobShadow` on customers and staff
   (`CUSTOMER_SIZE`-scaled radius). Nothing else changes. ~40 lines.
3. **cafe-sim furniture**: `ConvertToToon + DynamicLit` on untinted pieces;
   tinted pieces build their own probe-lit toon material; ghost preview stays
   PBR. ~60 lines.
4. Look at it. Decide on §3.4 B, and whether the interior probe density (§3.5)
   is a problem before touching the probe baker.

## 5. Decisions needed

1. ABI 10 for `ConvertToToon` in engine-api — acceptable? (Alternative: a
   plugin-visible *request* component with another name, leaving the host's
   marker private. Same ABI cost, one more type.)
2. Blob shadows under static furniture (§3.4 B): try it or skip it?
3. Should the material cache also dedupe **lightmapped** conversions until
   their lightmap arrives? It would shave bind groups off the editor's placed
   objects too, but `unique_material` then has to clone every one back out at
   apply time. Not worth it unless placed-object counts become a problem.
