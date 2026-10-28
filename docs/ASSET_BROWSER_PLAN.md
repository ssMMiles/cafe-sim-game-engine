# Assets Browser — Findings and Implementation Plan

Status: draft 2026-10-28, awaiting decisions in §7. Builds on
`docs/UNDO_REDO_PLAN.md` §8 and the shipped prefab system
(`docs/PREFABS_PLAN.md`: `PrefabLibrary` keyed by name, loaded from
`assets/prefabs/*.prefab` natively and OPFS `prefabs/` on web).

## 1. Findings — what the code does today that shapes the design

Verified against the code on 2026-10-28.

| # | Fact | Where | Consequence |
|---|---|---|---|
| 1.1 | `build.rs` writes `assets/meshes/manifest.manifest` (`name\|asset_path`, first `.gltf/.glb` per sub-dir, `character/` excluded) because WASM cannot `read_dir`. Loaded via `MeshManifestLoader` (`AssetLoader` for `.manifest`) | `build.rs`, `src/editor/catalog.rs` | The general asset manifest can reuse the exact same machinery: a build-time generated file + a tiny line-format `AssetLoader` |
| 1.2 | `MeshCatalog { manifest, entries }`; `rebuild()` composes manifest meshes + Point/Spot light + Empty + Group + sorted plugin `default_mesh` presets. `select(index, &mut PlacementConfig)` sets `kind`, resets `scale`, and arms/disarms `stamp_script` with careful "outgoing preset" logic. `populate_mesh_catalog` re-runs on manifest load or `ScriptRegistry` change and seeds `config.kind` on first rebuild | `src/editor/catalog.rs` | The stamp/preset logic must survive the refactor verbatim; keep `select()`'s body and only change what feeds `entries` |
| 1.3 | The Place panel draws the catalog as text buttons plus scale/yaw/stamp; it mutates scale/yaw through `bypass_change_detection` because a `PlacementConfig` change respawns the preview | `src/editor/ui/place.rs`, `object_placement.rs` | Moving the grid out of Place is a cut-and-paste plus keeping the bypass discipline; selecting from the browser *should* trigger change detection (new kind ⇒ new preview) |
| 1.4 | Dock: `EditorTab` enum, `DockLayout` rebuilt from `Default` every run, `EditorTabs: TabViewer` dispatches to `draw(ui, &mut Panels)`; `Panels` is one big `SystemParam` | `src/editor/ui/mod.rs`, `ui/viewport.rs` | Adding `EditorTab::Assets` is mechanical: enum variant, `title()`, `ui()` arm, a slot in `DockLayout::default`, new `Panels` fields |
| 1.5 | egui texture binding: `contexts.image_id(id).unwrap_or_else(\|\| contexts.add_image(EguiTextureHandle::Weak(id)))` then `egui::Image::from_texture` — done idempotently every pass for the viewport render target | `ui/mod.rs`, `ui/viewport.rs` | Thumbnails bind the same way; bevy_egui binds any filterable float texture and handles linear→sRGB |
| 1.6 | Render-to-texture: the 3D camera renders to an `Rgba16Float` `Image` (`TEXTURE_BINDING \| COPY_DST \| RENDER_ATTACHMENT`), `RenderTarget::Image`, `RenderLayers::layer(0)`; sprite + `Camera2d` on layer 1. No other layers in use | `src/game/gfx/camera.rs` | A thumbnail camera is the same recipe on a fresh layer (e.g. 7) |
| 1.7 | Tool switching goes through `EditorAction::SetTool` applied in `Update`; panels never set state directly | `src/editor/actions.rs` | "Click asset ⇒ switch to Place" is one `panels.actions.write(..)` |
| 1.8 | The placement preview spawns mesh scenes via `asset_server.load::<WorldAsset>` + `spawn_interactable_scene` with a translucent `PendingMaterial` override | `object_placement.rs` | The thumbnail renderer can spawn scenes the same way; `PendingMaterial` proves scenes can be re-materialled on load |
| 1.9 | `BakedSun` + `GlobalAmbientLight` exist precisely for `StandardMaterial` meshes; toon-converted scenes need baked lightmaps to look right | `world/mod.rs`, CLAUDE.md | Thumbnails render with `StandardMaterial` + their own light, not the toon pipeline (which would sample a missing lightmap) |
| 1.10 | `lib.rs` sets `ImagePlugin::default_nearest()`; images on `ImageSampler::Default` inherit nearest | CLAUDE.md, `baking/lightmap.rs` precedent | Thumbnail targets and scaled texture previews should get `ImageSampler::linear()` explicitly (or accept nearest — decision §7.5) |
| 1.11 | Both storage backends expose identical async `save/load/delete/list_keys(prefix)`; native `list_keys` walks `read_dir`, wasm walks OPFS | `src/storage/` | Runtime discovery of user-created assets can use `Storage::list_keys` — cross-platform already |
| 1.12 | `PlacementConfig { kind, prefab, scale, rotation, stamp_script }` | `objects.rs` | The browser arms placement purely by writing this resource |
| 1.13 | Only `assets/textures/terrain.png` (+ a cafe wall) exist; nothing in level data consumes a standalone texture | tree, §8 | Texture category is inspect-only for now |
| 1.14 | `bevy` is on default features, so `bevy_audio` + png/jpeg loaders are available; `assets/` has no audio directory | `Cargo.toml` | Audio category ships as a stub |
| 1.15 | `ViewportInput` carries `hovered/cursor_px/clicked_px`; rebuilt every egui pass | `picking.rs`, `ui/viewport.rs` | Drag-and-drop needs one more field (e.g. `dropped_px`) filled by the viewport tab on dnd release |

## 2. Design in one paragraph

Generalise the build-time manifest to `assets/asset_manifest.manifest` with
`kind|name|path` lines covering meshes, textures, prefabs and audio; keep
`MeshCatalog`'s composition/selection logic but rename it `AssetCatalog`,
grouping entries by a new `AssetKind`. Runtime-created prefabs merge in from
the `PrefabLibrary` (which already handles native-dir vs OPFS discovery), so
the browser never lists directories itself. Add `EditorTab::Assets`:
category sub-tabs, a search box, and a thumbnail grid; clicking a placeable
entry calls the same `select()` path (arming `PlacementConfig`, preserving
stamp-preset semantics) and sends `EditorAction::SetTool(Place)`. The Place
tool panel keeps only scale/yaw/stamp plus a compact recent strip.
Thumbnails come from one persistent offscreen camera on
`RenderLayers::layer(7)` with its own `DirectionalLight`, rendering one
queued asset per few frames into a small `Rgba8UnormSrgb` image each (cached
by asset path in a `ThumbnailCache`, bound to egui exactly like the viewport
texture); meshes/prefabs are framed by a bounding-sphere fit, textures shown
directly via their `Handle<Image>`, audio gets a static icon.

## 3. Detailed design

### 3.1 General manifest and discovery (`build.rs`, `src/editor/catalog.rs`)

**Build-time (both platforms, read-only `assets/`):** extend `build.rs` to
write one `assets/asset_manifest.manifest`:

```
mesh|rock|meshes/rock/bigrock.gltf#Scene0
texture|terrain|textures/terrain.png
prefab|lamp-cluster|prefabs/lamp-cluster.prefab
audio|door-creak|audio/door-creak.ogg
```

- Scan rules: `assets/meshes/` keeps today's "first gltf/glb per sub-dir,
  skip `character/`" rule; `assets/textures/*.{png,jpg,jpeg}` (skip `.psd`);
  `assets/prefabs/*.prefab`; `assets/audio/*.{ogg,wav}`.
  `cargo:rerun-if-changed=` per directory.
- Delete the old `assets/meshes/manifest.manifest` writer and path in the
  same commit the loader moves (one source of truth); `build.rs` removes a
  lingering old file.
- Loader: `MeshManifestLoader` → `AssetManifestLoader` producing
  `AssetManifestAsset(Vec<AssetManifestEntry { kind, name, path }>)`.
  Unknown kind strings skipped with a warning (forward compatibility).

**Runtime discovery:**

- **Prefabs**: the `PrefabLibrary` is the source of truth ("Save as prefab"
  inserts immediately; OPFS merges at startup). Catalog rebuild reads
  `Res<PrefabLibrary>` with `is_changed()`, as it reads `ScriptRegistry`
  today. The library wins on name collisions with manifest lines.
- **Native "Rescan" button** (optional): `EditorAction::RescanAssets`
  re-running the scan via `std::fs`, hidden on wasm. Not required for v1.
- **Web**: nothing beyond prefabs is creatable at runtime today; if web
  import appears later, `Storage::list_keys` is the hook.

**`AssetCatalog` model** (rename of `MeshCatalog`, same file):

```rust
pub enum AssetKind { Mesh, Prefab, Texture, Audio }

pub struct AssetEntry {
    pub kind: AssetKind,
    pub name: String,
    pub path: String,                    // asset path; thumbnail cache key
    pub placed: Option<PlacedKind>,      // what selecting it arms (None for Texture/Audio)
    pub preset_script: Option<(String, String)>,
    pub prefab: Option<String>,
}
```

- `rebuild(&registry, &prefab_library)` composes manifest meshes, the four
  built-ins (synthetic paths like `builtin://point_light`), plugin presets
  exactly as today, prefab entries from the library, textures/audio with
  `placed: None`.
- `selected_index` / `select` keep their current bodies, operating on
  `placed: Some(..)` entries; `select` also sets/clears `config.prefab`.
  **Preset disarm semantics, preset sort stability, and first-rebuild config
  seeding are preserved unchanged.**

### 3.2 `EditorTab::Assets` (`ui/mod.rs`, new `ui/assets.rs`)

- New enum variant + dispatch + a slot in the bottom node next to `Tool`.
- `AssetsBrowserState { category, search, recent: VecDeque<String>, inspected: Option<String> }`.
- Panel: category sub-tabs (**Placeable / Prefabs / Textures / Audio**),
  search filter, `ScrollArea` + `horizontal_wrapped` grid of ~96×96 cells
  (thumbnail or per-kind fallback glyph, selected ring from
  `selected_index`).
- Click placeable/prefab: `catalog.select` through normal change detection,
  push to `recent`, `SetTool(Place)` if needed. Click texture: large
  inspect-only preview. Audio: disabled "▶".
- **Place panel slims** to hint + scale/yaw/stamp + a recent strip (last ~8,
  same `select`) + "Open Assets…" (egui_dock `find_tab` + `set_active_tab`,
  routed like `reset_layout`).
- Drawing a cell with no cached thumbnail queues its path in
  `ThumbnailQueue` (dedup set) — only visible entries get rendered.

### 3.3 Thumbnail rendering (new `src/editor/thumbnails.rs`)

- Constants: `THUMBNAIL_LAYER = 7`, `THUMBNAIL_SIZE = 128`.
- `ThumbnailCache(HashMap<String, Handle<Image>>)`, `ThumbnailQueue`,
  `ThumbnailRig { camera, state }` (Idle / Loading / Settling).
- Rig: persistent disabled `Camera3d` + `RenderTarget::Image` +
  `RenderLayers::layer(7)` + `Tonemapping::Linear`, **no** prepasses/post/fog;
  one `DirectionalLight` (shadows off) on layer 7. Target images:
  `Rgba8UnormSrgb`, `ImageSampler::linear()`, one per cached thumbnail
  (~64 KB each); the camera re-points per job.
- Pipeline (one state step per frame, one job in flight): spawn subject on
  layer 7 (mesh: `WorldAssetRoot` scene; prefab: each mesh member at its
  local transform), stamp layer 7 onto `Mesh3d` descendants when the scene
  instantiates (`RenderLayers` is not inherited), park the rig far from the
  origin, wait for loads (timeout → fallback icon), frame by merged AABB
  (3/4 view), render 2 frames, cache, despawn, next.
- **Toon-conversion escape hatch**: verify what triggers toon conversion in
  `gltf_loading.rs` during implementation; thumbnails must stay on
  `StandardMaterial` (add a `NoToonConvert` marker if conversion is
  unconditional).
- Textures need no rig: cache stores `asset_server.load::<Image>(path)`.
- WASM: plain WebGPU raster render-to-texture — identical code path.

### 3.4 Texture category

Manifest-listed; click → large preview + dimensions + "textures aren't
placeable yet". When a consumer appears (terrain painting / material
overrides), the entry's `placed` slot is ready.

### 3.5 Audio category stub

Empty-state line ("Drop files into assets/audio/ and rebuild"), speaker
glyph + disabled play. ~30 lines.

### 3.6 Drag-from-browser into the viewport (optional, later)

Cells become `dnd_drag_source`; the viewport tab checks
`dnd_release_payload` and writes `ViewportInput.dropped: Option<(Vec2, usize)>`;
an `Update` system selects the entry and places through the
`handle_place_click` body factored to take an explicit hit point —
placement, stamping, undo and selection come along free.

## 4. Phasing (commits)

| Phase | Content | Size |
|---|---|---|
| 1 | General manifest + loader; `MeshCatalog` → `AssetCatalog`; call sites; behaviour-neutral | ~300 lines |
| 2 | `EditorTab::Assets` + panel (categories, search, text-button grid, fallback icons), Place panel slimmed + recent strip, texture inspect, audio stub | ~350 lines |
| 3 | `thumbnails.rs`: rig, queue, layer stamping, AABB framing, cache, egui binding; prefab thumbnails | ~300–400 lines |
| 4 (optional) | Drag-from-browser → viewport drop placement | ~120 lines |

## 5. Risks

| Risk | Mitigation |
|---|---|
| Toon conversion grabbing thumbnail scenes | Inspect the trigger in Phase 3; `NoToonConvert` escape hatch |
| `RenderLayers` not inherited → invisible/world-visible thumbnails | Stamp layer 7 onto descendants + remote rig origin |
| Catalog rename breaking preset/stamp semantics | Port `select`/`selected_index` verbatim; headless test of disarm logic |
| Muscle memory for the Place-tab grid | Recent strip + "Open Assets…" + default layout shows Assets |
| Stale old mesh manifest on disk | `build.rs` deletes it |
| A wedged thumbnail job stalls the queue | Per-job timeout → fallback icon |
| WASM memory from thumbnails | 64 KB each, visible-only queueing, optional LRU cap |

## 6. WASM parity summary

Build-time manifest + `PrefabLibrary` OPFS merge (no `read_dir`); thumbnails
are plain render-to-texture on WebGPU; texture preview via the asset server;
audio stub inert; dnd is egui-side.

## 7. Decisions needed

1. **Category layout**: built-ins (lights/Empty/Group) and plugin presets
   inside the "Placeable" category (plan), or their own sub-tabs?
2. **Place panel grid**: fully replaced by the recent strip (plan), or keep
   a collapsed full grid as fallback?
3. **Default dock position**: bottom next to Tool (plan) vs left under
   Outliner.
4. **Thumbnail look**: neutral `StandardMaterial` + directional light
   (plan) — confirm no toon-styled thumbnails wanted.
5. **Thumbnail sampler**: linear (plan) vs nearest (pixel-art consistent).
6. **Scope**: everything stays global under `assets/`;
   `projects/{id}/assets/` is out of scope for this feature. Confirm.
7. **Persist thumbnails to disk** (native-only speedup)? Plan says no for v1.
