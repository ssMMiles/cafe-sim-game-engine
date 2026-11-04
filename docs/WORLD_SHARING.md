# World sharing: native editor → browser

How a level authored in the native editor gets into someone's browser, and
why it needs almost no engine code.

## 1. The one fact everything rests on

The web build stores level data in OPFS (origin-private file system) under
keys that are **byte-for-byte the repo-relative paths the native build
uses**:

| Data | Native path = OPFS key |
|------|------------------------|
| World | `worlds/{id}/world.meta`, `worlds/{id}/chunks/…`, `worlds/{id}/objects/…` |
| Project manifest | `projects/{id}/project.toml` |
| Prefab definition | `assets/prefabs/{name}.prefab` |

(`terrain_lightmap_key`, `chunk_probes_key`, `object_lightmap_key`,
`write_manifest`, `prefab_key` — all pinned by tests such as
`opfs_keys_match_the_native_file_names`.)

So a *world bundle* is just a zip whose entry names are those keys, and the
start page can unpack it into OPFS with plain JavaScript before the engine
boots. The engine's existing startup discovery (`projects::populate_registry`,
`PrefabLibrary`, chunk streaming) then finds the world exactly as if it had
been saved in that browser.

## 2. Export (native): File ▸ Export world as zip

`src/editor/world_export.rs`, `EditorAction::ExportWorld`. Writes
`worlds/{id}.zip` next to the world directory (temp file + rename) on the
compute pool and reports in the status bar.

Contents:

- `worlds/{id}/world.meta`, every `.terrain` and `.objects` file;
- lightmaps and probe grids **of the current format version only**
  (`LIGHTMAP_FORMAT_VERSION`, `PROBE_FORMAT_VERSION` are in the file names;
  older bakes are skipped by the loader anyway and are most of a long-lived
  world's bytes — the cafe world: 115 MB on disk, 45 MB packed, 26 MB zipped);
- `assets/prefabs/{name}.prefab` for every prefab an instance root in any
  chunk references (missing ones are named in the notice);
- `projects/{project}/project.toml`, the active project's manifest narrowed
  to `worlds = [id]`, `start_world = id`.

It exports **what is on disk** — save first. Native only: the request posts a
"native only" notice on wasm. Reading is via the `zip` crate
(`default-features = false, features = ["deflate-flate2"]`, native target
only).

Dev check: `WORLD=cafe cargo test --release export_world_fixture -- --ignored
--nocapture` exports a real world into a temp dir through a symlink (nothing
in `worlds/` is touched) and prints the archive path.

## 3. Import (web): `index.html`

The start page is a landing card: engine download/compile progress, the
worlds currently in this browser's OPFS (with *remove*), an *Import zip…*
button, whole-page drag-and-drop, and an **Enter** button that enables once
the engine is compiled. `?play` skips the landing (the old auto-start, for
quick testing); `?world=<url>` downloads and imports a bundle first, so a
link can carry a level.

Import = read the zip's central directory, inflate entries with
`DecompressionStream('deflate-raw')`, write each to OPFS, then merge
manifests. Two layouts are accepted:

- **exporter bundle** — `worlds/*/world.meta` present; `worlds/…`,
  `projects/*/project.toml` and `assets/prefabs/*.prefab` entries are taken,
  anything else ignored;
- **hand-zipped world folder** — `{id}/world.meta` (or `world.meta` at the
  root, id from the file name); everything under it maps to `worlds/{id}/…`
  and a project `{id}` with just that world is synthesised, because the
  engine's menu reaches worlds only through projects.

Manifests are **merged**, never replaced: an existing `projects/{id}` keeps
its name, plugins and `start_world` and gains the bundle's worlds, so two
bundles from one project don't evict each other. Removing a world from the
page also removes it from every manifest (and drops a project left with no
worlds). Prefab files overwrite same-named ones — definitions are global,
that is the sharing semantics.

If the engine is already running when a zip is dropped, the page imports and
then reloads: project discovery happens at startup and there is no rescan.

The pure part (zip reading, layout mapping, manifest parse/serialise) sits
between `@bundle-logic-start` / `@bundle-logic-end` in `index.html`;
`node tools/bundle-check.mjs some.zip [--keys]` evaluates that exact slice
and prints the import plan, decoding every entry — run it on an archive
before blaming the page.

## 4. Caching

wasm-pack writes unhashed file names and plain static servers send no
`Cache-Control`, so browsers apply *heuristic* freshness: a 63 MB wasm cached
days ago is served next to a freshly fetched shader, and the mismatch
surfaces at startup as e.g. `The shader uses more bytes of the buffer (144)
than the layout's minBindingSize (128)` (the `BakeUniforms` struct grew on
2026-10-28; the browser was still running an older wasm). Two guards:

- `index.html` asks the server for the wasm's `Last-Modified` with
  `cache: 'no-cache'` (always revalidated) and keys both engine files on it
  (`?v=…`), so a new build is a new URL and an old build stays cached;
- `tools/serve.py` is `http.server` plus `Cache-Control: no-cache` on
  everything, which covers the assets bevy fetches on its own.

## 5. Limits

- **Assets are not in a bundle.** A world references meshes and textures by
  asset path; the deployed `assets/` must have them (and plugins don't exist
  on wasm — plugin bindings survive as unhydrated data, warn-only).
- OPFS needs `FileSystemFileHandle.createWritable` (Chrome, Edge, Firefox).
  The engine's own wasm storage has the same requirement; the page detects
  its absence and disables import.
- The whole zip is held in memory during import; fine at tens of MB.
- `.objects` files aren't rewritten, so a world exported by a newer build
  with a newer `.objects`/`world.meta` version needs a web build at least as
  new — the same rule as any two native checkouts.
