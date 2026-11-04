# Build times: where they go and what to try

Researched 2026-11-03; **everything in §1–§4 is applied** as of the same
day except where marked *not applied*. Nothing was timed here — measure
with `cargo build --timings` (writes `target/cargo-timings/*.html`,
per-crate durations and the critical path) on a fast machine. Ranked by
expected payoff ÷ risk.

Applied state (see `Cargo.toml` / `.cargo/config.toml`): direct `image`,
`winit`, `argh`, `env_logger`, `rand`, `anyhow`, `getrandom 0.2/0.3` pins
removed; bevy / avian3d / bevy_egui on explicit feature lists; cafe-sim on
`rand 0.10` (one `rand` in the graph); dev deps `debug = false`;
`-Zthreads=8`; `web-dev` and `perf` profiles; `wasm-opt` off; sccache as
`rustc-wrapper`. Native graph 689 → 596 crates. Not applied: **cranelift** — it compiled, but the test binary aborted with
`llvm.aarch64.neon.fmaxnmv.f32.v4f32 is not yet supported`: glam's NEON
`max_element` is inlined into our crate and rustc_codegen_cranelift has no
lowering for it (issue #171); the only workaround is glam `scalar-math`,
which costs runtime SIMD everywhere. Moving
`dynamic_linking` out of the workspace features (plugins must link the same
bevy dylib as the host; `cargo build-plugins` builds them alone, so a
per-crate toggle would silently give them a static bevy), and per-config
target dirs (a workflow choice, not a manifest change).

Corrections found while applying: `mesh_picking` **is** used (the editor's
ray casts are `MeshRayCast`), and the terrain collider carried an avian
`DebugRender` component (behind `physics_debug` until 2026-11-03, when
`debug-plugin` became always-on so the editor's collider view works in every build). `bevy_clipboard` stays
via `bevy_text`'s default, `ruzstd`/`tiff` via `bevy_image`'s default
(pulled by `bevy_ui`); both are sub-crate defaults we cannot reach from the
top-level feature list.

## The shape of the problem

- 689 crates in the native graph, 640 on wasm. Bevy alone is ~60 of them
  and the long pole; everything that re-triggers a *bevy* rebuild costs the
  full ~18 min (wasm release) regardless of what else is done.
- `target/` is **68 GB** in this worktree, with **42 distinct build units of
  the `engine` crate** under `target/debug/build/engine/` and 25 880 object
  files. Every distinct (profile × target × feature set × crate type) is its
  own unit, and a different bevy feature set is a different *bevy*. What
  produces them today: `cargo check/test` (dev, native), `cargo test
  --release`, `wasm-pack --release` (release, wasm, `opfs`), the same with
  `web-profile`, `physics_debug`, `profiling`, `cargo check --target wasm32`
  (dev, wasm), rust-analyzer (own hash). Each feature toggle is a full bevy
  rebuild in that slot and then sits on disk forever.
- Each worktree has its **own** target dir and cold cache (`oak` 68 GB,
  `elm` 23 GB, main 0.9 GB). Two sessions in two worktrees build bevy twice.
- `[profile.release]` is **fat LTO + `codegen-units = 1`**, and it is what
  `wasm-pack --release` *and* `cargo test --release` use. Fat LTO over 640
  crates into a 63 MB wasm is the single slowest step we run, and
  `wasm-pack` then runs `wasm-opt` over the result (it did on every build
  logged today).
- Dev profile: deps at `opt-level = 3` **with full debuginfo** (the default
  `debug = true` applies to `package."*"` too). That is the normal bevy
  setup for runtime speed, but full DWARF for 689 optimised crates is a lot
  of bytes to write and to link into `libbevy_dylib.dylib` (229 MB).

## 1. Stop paying for things we don't use (dependency trims)

Verified by reading the manifests and grepping the code; each one is a
bevy (or near-bevy) rebuild to apply, so batch them and measure once.

| Dependency | Why it's there | Finding | Action |
|---|---|---|---|
| `image = "0.25.6"` (direct, default features) | pinned "to match bevy" | **Not used directly** — every `image::` in `src/` is `bevy::image::` or `exr::image::`. Its default features are `rayon` + **all formats: avif, bmp, dds, exr, ff, gif, hdr, ico, jpeg, png, pnm, qoi, tga, tiff, webp** — and because features unify, they are forced onto bevy_image's `image` too. `avif` pulls **`ravif` → `rav1e`**, an AV1 *encoder*, one of the heaviest crates in the ecosystem; `ravif v0.11.12` is in our graph right now. | Remove the direct dep. Keep `exr`/`half` (used). Expect the biggest single win here. |
| bevy `audio` (default) | default | **No audio code anywhere** (`AudioPlayer`/`AudioSource`: 0 hits; 0 audio assets). Pulls `rodio`, `cpal`, `lewton` (vorbis), CoreAudio bindings. | `default-features = false`, list what we use (below). |
| bevy `bevy_gilrs` (via `default_platform`) | default | No gamepad code. `gilrs` + `gilrs-core`. | drop |
| bevy `sysinfo_plugin` (default) | default | `sysinfo` crate; only our `profiling` feature wants it. | drop from default, keep in `profiling` |
| bevy `bevy_clipboard` + bevy_egui `manage_clipboard` | defaults | Two clipboard stacks (`arboard` + objc2 AppKit, plus bevy's). The editor's text fields paste through egui's; test whether removing both breaks paste in the inspector. | try dropping bevy's; egui's `manage_clipboard` may need to stay |
| bevy_egui `open_url`, `picking` (defaults) | default | `webbrowser` crate; egui picking backend — our editor picks with its own `ViewportInput`. | `default-features = false, features = ["render", "default_fonts", "bevy_ui"]` (the `bevy_ui` one controls egui/bevy_ui ordering — verify "bevy_ui renders above egui" still holds) |
| bevy `webgl2` (via `default_platform`) | default | We build `webgpu` only (`WEBGL2` is a compile-time cfg; `webgpu` wins when both are set). Compiles webgl shader variants/defs for nothing. | drop |
| bevy `picking` → `sprite_picking` | via 2d/3d | `ui_picking` is used (`Hovered`/`Pressed` on bevy_ui buttons) and so is `mesh_picking`: the editor's picker, placement and brush rays are `MeshRayCast`s. Only the sprite backend is dead. | keep `bevy_picking`, `ui_picking`, `mesh_picking`; drop `sprite_picking` |
| bevy `ktx2`, `zstd_rust` (via `3d_api`) | default | No `.ktx2` assets (assets are gltf/glb/png). `ruzstd` in graph. | drop |
| bevy `smaa_luts`, `tonemapping_luts` | via `3d_api` | We use `Tonemapping::Linear` and no SMAA. These are embedded binary blobs (cheap compile) but `tonemapping_luts` also drags `ktx2`+`zstd` back in. | drop with the above |
| bevy `bevy_anti_alias`, `bevy_post_process` | via `3d_bevy_render` | The camera (`gfx/camera.rs`) uses `Fxaa` (anti_alias) plus `DistanceFog`/`VolumetricFog` (pbr, not post_process). So `bevy_anti_alias` **stays**; `bevy_post_process` (bloom, DoF, motion blur, …) has no user — `dof::prepare_depth_of_field_pipelines` ran every frame in the web trace for nothing. | drop `bevy_post_process` only |
| bevy `morph`, `morph_animation` | via `3d_api` | No morph targets. `gltf_animation` **is** used (`AnimationGraph`). | drop morph |
| bevy `reflect_auto_register` | via `default_app` | **Required.** Tried dropping it: `bevy_world_serialization`'s scene spawner panics on the first unregistered component (`Transform`) when a glTF scene spawns. | keep |
| bevy `x11`, `wayland`, `custom_cursor` | default | Linux-only / unused. No cost on macOS but noise. | drop |
| avian3d defaults: `xpbd_joints`, `bevy_picking`, `bevy_scene`, `debug-plugin` | default | **No joints anywhere** (`RevoluteJoint` etc.: 0 hits) — yet the web trace showed ~500 joint-solver system spans per physics step, i.e. they run empty every frame. No avian picking. `debug-plugin` is already default, so our `physics_debug` feature currently toggles nothing at compile time. | `default-features = false, features = ["3d", "parry-f32", "parallel", "collider-from-mesh"]`, `debug-plugin` only under `physics_debug`. Runtime win too. |
| `winit`, `argh`, `env_logger` (direct) | leftovers | `winit::` unused (bevy re-exports); the other two are flagged by `unused_dependencies` already. | remove |
| `getrandom` 0.2 / 0.3 / 0.4 pins | wasm `js` features | 0.2 is depended on **only by us**; 0.3 by `rand_core 0.9` (and us). | drop the 0.2 pin; keep 0.3/0.4 only while something needs them (`cargo tree -i getrandom@0.3`) |
| `rand` ×3 (0.8 in cafe-sim, 0.9 in engine, 0.10 via bevy_math/glam) | drift | Engine's `rand` is unused (warned). Plugins use 0.8. | drop from engine; bump cafe-sim to 0.10 to unify |
| `wasm-bindgen` as a plain dep | wasm entry | Only used under `cfg(wasm32)`; warned as unused natively. | move to the `cfg(target_arch = "wasm32")` dependency table |

Other duplicates in the graph (`cargo tree -d`): `hashbrown` ×3, `itertools`
×3, `objc2`/`objc2-app-kit`/`objc2-foundation` ×2 (winit vs arboard —
dropping clipboard removes one stack), `bitflags` ×2, `core-foundation` ×2,
`block2` ×2, `nix` ×2, `font-types` ×2. Mostly transitive; the objc2 pair is
the only one worth chasing and the clipboard trim does it.

A trimmed bevy would look like (to be validated with `cargo tree -e features
-p bevy` and by running the editor; 0.20's feature split is new and some
names imply others):

```toml
bevy = { version = "0.20", default-features = false, features = [
    "std", "async_executor", "multi_threaded",
    "bevy_asset", "bevy_log", "bevy_state", "bevy_winit",
    "bevy_render", "bevy_core_pipeline", "bevy_pbr", "bevy_light",
    "bevy_gltf", "gltf_animation", "bevy_scene",
    "bevy_sprite", "bevy_sprite_render",          # the gameplay render-target sprite
    "bevy_ui", "bevy_ui_render", "bevy_ui_widgets", "bevy_text", "default_font",
    "bevy_picking", "ui_picking",
    "bevy_gizmos", "bevy_gizmos_render",
    "bevy_anti_alias",                            # Fxaa on the camera
    "png", "jpeg",
    "web", "webgpu",
    "debug",                                      # system names in spans / wgpu labels
    "dynamic_linking",                            # see §3
] }
```

## 2. Profiles: don't fat-LTO every iteration

- **A `web-dev` profile** for iterating on the browser build:
  `inherits = "release"`, `lto = "thin"`, `codegen-units = 16`,
  `opt-level = 2`, `debug = false`. wasm-pack only knows `--dev`,
  `--profiling`, `--release`; use it via `wasm-pack build --release` only
  for a deploy, and for iteration run the two steps it wraps:
  `cargo build --target wasm32-unknown-unknown --profile web-dev --features opfs`
  then `wasm-bindgen --target web --out-dir pkg --out-name game_engine_lib
  target/wasm32-unknown-unknown/web-dev/game_engine_lib.wasm`
  (needs `cargo install wasm-bindgen-cli` at the version in `Cargo.lock`).
- **Turn off `wasm-opt` for non-deploy builds**:
  `[package.metadata.wasm-pack.profile.release] wasm-opt = false`
  (and the same under `profile.dev`/`profiling`). On a 63 MB module it is
  a separate multi-minute pass that only shrinks/speeds the final artifact.
- **`cargo test --release`** builds the whole graph *again* under fat LTO.
  The only reason we use it is the ignored benchmarks (`rebuild_cost`,
  `terrain_bake_cost_breakdown`, `export_world_fixture`). Give them a
  `[profile.perf]` with `inherits = "release"`, `lto = false`,
  `codegen-units = 16` and run `cargo test --profile perf …`.
- **Dev debuginfo**: `[profile.dev.package."*"] debug = false` (or
  `"line-tables-only"` if you want backtraces through bevy) — keep
  `opt-level = 3` for the deps. Our own crate keeps full debug. This shrinks
  every dep artifact and the final link; it is the classic bevy tip after
  `dynamic_linking`. The same block is currently duplicated in `Cargo.toml`
  and `.cargo/config.toml` — keep one.
- `[profile.release] debug = false, strip = "debuginfo"` for native release
  if not already (less to link).

## 3. Compiler / toolchain flags (nightly is already in use)

- **`-Zthreads=8`** (parallel front end): `[build] rustflags = ["-Zthreads=8"]`
  — or `[target.aarch64-apple-darwin]` to keep wasm rustflags separate. Our
  crate is 44 k lines in one unit; the front end is serial without it.
  Changing `rustflags` invalidates the cache once.
- **Cranelift for dev**: `rustc-codegen-cranelift-aarch64-apple-darwin` is
  already installed and the manifest has the commented-out
  `codegen-backend = "cranelift"`. It does **not** support `wasm32`, which
  is presumably why the per-target cfg was tried. The clean form is
  `[profile.dev] codegen-backend = "cranelift"` plus
  `[target.wasm32-unknown-unknown] rustflags = [..., "-Zcodegen-backend=llvm"]`
  (so wasm dev builds override back to LLVM), with
  `cargo-features = ["codegen-backend"]` at the top of `Cargo.toml`. It
  speeds up *our* crate's codegen on every incremental build; deps at
  `opt-level = 3` should stay on LLVM (`[profile.dev.package."*"]
  codegen-backend = "llvm"`), which is also what keeps the game fast.
- **`dynamic_linking`** in the workspace bevy features is right for dev
  (it is what makes the incremental link fast). Bevy excludes `bevy_dylib`
  on wasm itself, so the web build is unaffected. It *is* on for native
  `--release` too, which costs runtime (no LTO across bevy) rather than
  build time — move it to a `dev` feature or `cargo run --features
  bevy/dynamic_linking` when a release native binary matters.
- **Linker: lld, applied 2026-11-04.** `-Clink-arg=-fuse-ld=lld` in the
  `[target.aarch64-apple-darwin]` table. Measured on this project, `touch
  src/lib.rs` then `cargo build`:

  | Linker | Samples (s) | Median |
  |--------|-------------|--------|
  | Apple `ld-27037` (Xcode 26.5) | 8.43, 8.94, 9.51, 10.38 | ~9.2 s |
  | Homebrew LLD 23.1.5 | 3.53, 3.59, 3.67, 3.85, 4.22, 4.44, 4.61, 4.88 | ~4.1 s |

  So the edit-rebuild loop roughly halves. This was worth re-testing even
  though Apple's new `ld` is itself the "fast" linker — being faster than
  the *old* ld64 did not make it faster than lld here. Verified after the
  switch: 174 tests link and pass, the plugin dylibs still `dlopen` and
  report the right ABI version (checked directly, since a linker is exactly
  the thing that could break the dylib plugin system).

  Caveats: **`brew install lld` is now required** for any native build, the
  flag rehashes every crate so each target dir pays one full rebuild, and
  the binary still relies on `cargo run` setting `DYLD_FALLBACK_LIBRARY_PATH`
  for `bevy/dynamic_linking` (no `LC_RPATH` is emitted, with either linker).
- **mold cannot be used on macOS at all** (checked 2026-11-04 with brew's
  mold 3.0.0: it only emits ELF, rejects Mach-O objects as "unknown file
  type"; its macOS fork `sold` was discontinued). It only matters if a Linux
  native target appears. The wasm build links with rustc's bundled
  `rust-lld` regardless. `split-debuginfo = "unpacked"` is the macOS default
  already.

## 4. Stop rebuilding the same bevy over and over

**Applied 2026-11-03 as fixed build configurations** (table at the top of
`.cargo/config.toml`): four profiles only — dev, perf, web-dev, release —
and one alias or `tools/web.sh` mode per feature set, each feature set in
its own target directory (`target/web`, `target/web-profile`,
`target/web-release`, `target/profiling`; `target/physics-debug` went away
with the feature). Profiles
already get their own subdirectory, so the separate dirs exist for
*feature* changes, which is what re-hashes bevy. `tools/web.sh` replaces
wasm-pack locally (cargo build + wasm-bindgen; wasm-opt only on request);
CI still uses wasm-pack. `release-native`, the `tracy` feature and the
wasm-pack metadata are gone. The `opfs` feature is set for every wasm build
by the target's rustflags, so no command passes it.

- **One target dir per configuration, kept warm.** Feature sets that
  change bevy's features (`web-profile`, `profiling`, `tracy`) should build
  into their own dir (`CARGO_TARGET_DIR=target-profile wasm-pack …`) so
  toggling them back doesn't evict the normal build. Alternatively never
  toggle: keep `web-profile` builds in a separate checkout.
- **sccache** (`brew install sccache`, `[build] rustc-wrapper = "sccache"`):
  shares compiled *dependency* artifacts across target dirs and worktrees,
  so a second worktree's cold build hits the cache for all 600+ deps and
  only compiles the workspace crates. This is the fix for the worktree
  workflow (and for two sessions building at once — a shared `target-dir`
  would instead serialise them on cargo's build lock).
- Clean out the 42 stale engine units: `cargo clean -p engine` is cheap
  (deps stay); `cargo-sweep --time 7` for the rest.
- rust-analyzer: make sure its check command uses the same features/target
  as your terminal builds (`rust-analyzer.cargo.features`,
  `.check.targetDir` off) or it keeps a private, constantly-invalidated unit.

## 5. Things that look like savings but aren't

- `opt-level = 1` → `0` for our crate: faster codegen, but the editor and
  bake become unusable; cranelift gets the same compile win without it.
- `lto = "off"` for *deploy* wasm: binary grows a lot; keep fat LTO for the
  artifact you ship, just not for every iteration.
- `panic = "abort"`: small code-size win; risky with dylib plugins that
  may unwind across the boundary. Skip.

## Suggested order

1. Remove the direct `image` dep (+ `winit`, `argh`, `env_logger`,
   `getrandom 0.2`, engine's `rand`). Measure.
2. `[profile.dev.package."*"] debug = false`, `-Zthreads=8`, `wasm-opt = false`.
3. `web-dev` + `perf` profiles so fat LTO only runs for deploys.
4. bevy `default-features = false` with the list above; avian3d and
   bevy_egui trims. Run the editor, the game, a bake, and the web build
   after each.
5. sccache + per-config target dirs; `cargo clean -p engine`.
6. Cranelift for the dev profile.
