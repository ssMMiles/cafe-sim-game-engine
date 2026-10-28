# Bevy 0.18 → 0.20 Upgrade Plan

Status: **Phases 1–3 complete 2026-10-27** — the engine is on Bevy 0.20.
§8 and §9 record what each phase actually hit versus what was predicted.

Facts in this document were verified on 2026-10-27 against the published
migration guides (`bevyengine/bevy-website`, `content/learn/migration-guides/`),
the Avian migration guides (`avianphysics/avian`, `migration-guides/`), the
egui/bevy_egui changelogs, and the crates.io version/dependency graph. Call-site
counts come from the tree at `f02ea79`.

---

## 0. Verdict

**This is not a drop-in bump.** It is a two-release migration with three
sweeping overhauls that require real design work, plus a long tail of
mechanical renames.

The three overhauls:

1. **The render graph was deleted** (0.19). `RenderGraph`, `ViewNode`,
   `Node3d` labels and `add_render_graph_node` are gone; render passes are now
   ordinary systems in the `Core3d` / `RenderGraph` schedules. We have two
   hand-written nodes — the edge-detection post-process and the lightmap bake
   compute dispatch — and both must be rewritten.
2. **naga_oil was replaced by WESL** (0.20). `#import` / `#ifdef` /
   `#{MATERIAL_BIND_GROUP}` no longer exist. Five of our six shaders use those
   directives and must be translated and renamed `.wgsl` → `.wesl`.
3. **egui's `Context`-based panel API was deleted** (egui 0.34 → 0.36). Panels
   and `egui_dock`'s `DockArea` now take a `&mut Ui`, not a `&Context`. The
   editor's top-level chrome has to be restructured around a single root `Ui`.
   This is not optional and not cosmetic — `DockArea::show(&Context, …)` does
   not exist in egui_dock 0.21.

Everything else is mechanical or a behaviour watch-out. The good news is that
the ecosystem pin documented in `CLAUDE.md` ("bevy_egui 0.39 + egui_dock 0.18
are the last lines for Bevy 0.18") **resolves cleanly** — see §1.

Rough effort: the two overhauls dominate. The mechanical tail is a day's work
but is mostly `sed`-able. The real cost is verification, because the entire
visual identity of this engine (baked toon banding, edge detection) lives in
exactly the two subsystems being overhauled, and I can't run the app.

---

## 1. Version matrix

Everything we depend on has a Bevy 0.20 line, and — importantly — `bevy_egui`
and `egui_dock` land on the **same** egui (0.36), so the constraint that forced
the 0.39 / 0.18 pin is gone.

| Crate | Now | Target | Constraint |
|---|---|---|---|
| `bevy` | 0.18.0 | **0.20.0** | — |
| `avian3d` | 0.5 | **0.8.0** | requires `bevy ^0.20` |
| `bevy_egui` | 0.39 | **0.43** | `bevy_ecs ^0.20`, `egui ^0.36` |
| `egui_dock` | 0.18 | **0.21** | `egui ^0.36` ✅ same line |
| `egui` (transitive) | 0.33 | 0.36 | — |
| `wgpu` (transitive) | — | 30 | via `bevy_render 0.20` |
| `winit` (direct dep) | 0.30.12 | **unchanged** | `bevy_winit 0.20` still wants `^0.30` |
| `naga` (dev-dep) | 27 | **30** | must match wgpu 30 for the shader-validation test |

All five bevy features we use (`web`, `webgpu`, `jpeg`, `debug`,
`dynamic_linking`) still exist in 0.20 with the same meaning. Default features
are now `2d, 3d, ui, audio` — `audio` and `ui` are no longer implied by `3d`,
but since we take defaults, nothing changes for us.

A clean intermediate stack exists for the halfway point:

> Bevy 0.19 + avian3d 0.7 + bevy_egui 0.40.1 + egui_dock 0.19.1 (both on egui 0.34)

**Do the migration in two steps.** Each published guide then applies verbatim,
and when something breaks you know which release caused it. A single jump mixes
two guides' worth of breakage into one non-compiling tree.

---

## 2. Sweeping work (needs design, not just renames)

### 2.1 Render graph → systems (Bevy 0.19)

The `RenderGraph` API is gone. `ViewNode` becomes a plain system taking a
`ViewQuery<...>` param and a `RenderContext` param; ordering is expressed with
`.before()` / `.after()` against **the actual pass functions**, not `Node3d`
labels.

```rust
// 0.18
impl ViewNode for MyNode {
    type ViewQuery = (&'static ExtractedCamera, &'static ViewTarget);
    fn run(&self, _graph, render_context, (camera, target), world) -> Result<(), NodeRunError>
}
render_app
    .add_render_graph_node::<ViewNodeRunner<MyNode>>(Core3d, MyLabel)
    .add_render_graph_edges(Core3d, (Node3d::Foo, MyLabel, Node3d::Bar));

// 0.19
pub fn my_render_pass(world: &World, view: ViewQuery<(&ExtractedCamera, &ViewTarget)>, mut ctx: RenderContext) { … }
render_app.add_systems(Core3d, my_render_pass.after(foo_pass).before(bar_pass).in_set(Core3dSystems::MainPass));
```

Two sites:

| Site | What it is | Migration |
|---|---|---|
| `src/game/gfx/post_process.rs:48-56,66-140` | `EdgeDetectionNode: ViewNode`, inserted between `Node3d::Tonemapping` and `Node3d::EndMainPassPostProcessing` | Becomes a system in `Core3dSystems::PostProcess`, ordered `.after(tonemapping)`. The `ViewQuery` tuple carries over unchanged. |
| `src/game/world/baking/gpu.rs:234-282,323-325` | `BakeComputeNode: render_graph::Node` added to the root `RenderGraph` resource | The `RenderGraph` **schedule** survives (`bevy::render::renderer::RenderGraph`) for non-camera work. `render_graph.add_node(…)` → `render_app.add_systems(RenderGraph, bake_compute_pass)`, ordered within `RenderGraphSystems::{Begin, Render, Submit, Finish}`. |

Note the post-process set also split in 0.19: `Core3dSystems::PostProcess` became
`EarlyPostProcess` + `PostProcess`. Pick `PostProcess` to match today's
placement after tonemapping.

Secondary, same files:

- `ExtractComponent` was refactored. It now has a `SyncComponent` supertrait
  that declares which components to tear down when the source component is
  removed (the render entity is no longer despawned). `EdgeDetectionSettings`
  (`post_process.rs:200`) needs this.
- In **0.20**, extraction became generic over worlds: the derive needs
  `#[extract_app(RenderApp)]`, and the machinery moved to `bevy_extract`
  (mostly re-exported from `bevy_render`).

#### Specialize the edge-detection pipeline on the view's target format

`ViewTarget::is_hdr` is gone and `TextureFormat::bevy_default()` is deprecated.
`post_process.rs:184` currently hard-codes `TextureFormat::Rgba16Float` with
the comment "Camera has Hdr enabled". **Do this properly rather than keeping
the constant**: the blessed source is `ExtractedView::target_format` (confirmed
present on `ExtractedView` in 0.19) plumbed through a specialization key.

Concretely, `EdgeDetectionPipeline` stops being a one-shot
`CachedRenderPipelineId` built in `RenderStartup` and becomes a
`SpecializedRenderPipeline` whose `Key` carries the `TextureFormat`:

```rust
impl SpecializedRenderPipeline for EdgeDetectionPipeline {
    type Key = EdgeDetectionKey;            // { format: TextureFormat }
    fn specialize(&self, key: Self::Key) -> RenderPipelineDescriptor { … }
}
```

`RenderStartup` then only builds the layout, sampler and shader handle; the
pass system resolves the pipeline per view via
`SpecializedRenderPipelines::<EdgeDetectionPipeline>::specialize(&cache, &pipeline, key)`
using the `ExtractedView` it is already querying. This removes the silent
coupling between the post-process and the camera's `Hdr` component — today,
dropping `Hdr` from the camera would produce a format mismatch at pipeline
creation with no compile-time signal.

Cost: one extra `ExtractedView` in the pass's `ViewQuery`, one key type, one
`init_resource::<SpecializedRenderPipelines<_>>()`. Worth it; it is the
difference between the pass working for *the* camera and working for *a*
camera.

### 2.2 naga_oil → WESL (Bevy 0.20)

Shaders move from the naga_oil dialect to [WESL](https://wesl-lang.dev) and are
renamed `.wgsl` → `.wesl`. Plain WGSL with no preprocessor directives keeps
working untouched.

| Shader | Directives used | Needs conversion? |
|---|---|---|
| `assets/shaders/toon_lighting.wgsl` | `#import` ×3, `#ifdef` ×3, `#{MATERIAL_BIND_GROUP}` ×3 | **Yes** |
| `assets/shaders/terrain_toon.wgsl` | `#import` ×3, `#ifdef` ×3, `#{MATERIAL_BIND_GROUP}` ×5 | **Yes** |
| `assets/shaders/terrain.wgsl` | `#import` ×1, `#ifdef` ×1 | **Yes** |
| `assets/shaders/sky_dome.wgsl` | `#import` ×1 | **Yes** |
| `assets/shaders/edge_detection_post.wgsl` | `#import` ×1 | **Yes** |
| `assets/shaders/lightmap_bake.wgsl` | none — plain WGSL | **No** |

Translation rules:

- `#import a::b::C` → `import a::b::C;` (semicolon; imports come first, before
  any declaration or `enable`).
- **Module paths changed to match crate layout**: `bevy_pbr::mesh_view_bindings`
  → `bevy_pbr::render::mesh_view_bindings`, `bevy_pbr::forward_io` →
  `bevy_pbr::render::forward_io`, `bevy_pbr::prepass_utils` →
  `bevy_pbr::prepass::utils`. Each of our imports must be re-pathed
  individually; there is no mechanical rule.
- `#ifdef FOO … #endif` → `@if(FOO)` attached to a whole declaration,
  struct member, parameter, import or statement. Our `#ifdef PREPASS_PIPELINE /
  #else / #endif` blocks around whole `#import` lines and whole function bodies
  map to `@if` / `@else` reasonably directly, but the `#else` arms that swap an
  entire `FragmentOutput` type will need care.
- `@group(#{MATERIAL_BIND_GROUP})` → `@group(constants::MATERIAL_BIND_GROUP)`.
- Update the six `ShaderRef` paths in Rust
  (`terrain/material.rs:25,29,33`, `sky.rs:24`, `post_process.rs:30`,
  `engine-api/src/toon_material.rs:60,64`) to `.wesl`.

`lightmap_bake.wgsl` stays `.wgsl` and its `include_str!` validation test
(`gpu.rs:817-854`) keeps working — but bump the `naga` dev-dep 27 → 30 so the
test validates against the same naga wgpu 30 links.

The `shader_format_wesl` feature is gone (always on) and GLSL support was
removed. Neither affects us.

### 2.3 Tonemapping semantics (Bevy 0.20) — visual regression risk

`src/game/gfx/camera.rs:109` spawns the 3D camera with `Tonemapping::None`.

In 0.20 `Tonemapping::None` became a **full passthrough**: `ColorGrading` and
`DebandDither` no longer apply under it, and negative colour channels are no
longer clamped to zero. The guide is explicit: *"If you used `Tonemapping::None`
to turn off the tone curve, use the new `Tonemapping::Linear` instead. It
applies no tone curve and keeps grading, dither, and the clamp."*

**Use `Tonemapping::Linear`** to preserve current behaviour. This matters more
here than in most projects: the toon shaders band HDR radiance into five steps,
and losing the negative-channel clamp ahead of that quantiser is exactly the
kind of thing that produces sparkle in the darkest band. Bevy also logs a
warning for `Tonemapping::None` + `DebandDither::Enabled`.

Related, lower risk: `Hdr` moved to `bevy_camera` (0.19) and `Tonemapping` /
`DebandDither` moved to `bevy_render::view` (0.20). Rust re-exports keep both
compiling; only reflected type paths changed, which we don't serialize.

### 2.4 Editor chrome: restructure around a root `Ui` (egui 0.34 → 0.36)

egui spent 0.34–0.35 collapsing the long-standing `Context`/`Ui` overlap. The
end state, verified against egui 0.36.2 and egui_dock 0.21.1:

- `SidePanel` and `TopBottomPanel` are **gone** (deprecated aliases for `Panel`
  in 0.34, removed in 0.35 along with everything else marked `#[deprecated]`).
- `Panel::show` and `CentralPanel::show` take **`&mut Ui`**, not `&Context`.
  (What used to be `show_inside` *was renamed to* `show`; the old
  `Context`-taking `show` no longer exists. `show_inside` survives as a
  deprecated alias pointing the other way.)
- `egui_dock::DockArea` has **only** `show_inside(&mut Ui, …)` in 0.21. There
  is no `DockArea::show(&Context, …)` to call.
- `Ui` now `Deref`s to `Context`, so `ui.input(…)`, `ui.style()` etc. work
  directly and naked `Context`s largely disappear.
- Free-floating containers are unaffected: `egui::Window::show(&ctx, …)` still
  takes a `&Context`, so `menu::dialogs` needs no change.

`bevy_egui` still hands out a `&mut Context` (`contexts.ctx_mut()?`) and owns
`begin_pass`/`end_pass` itself, so we cannot use `Context::run_ui` (the
integration-level entry point that replaced `Context::run`). Inside an
`EguiPrimaryContextPass` system the root `Ui` is built explicitly:

```rust
let mut root = egui::Ui::new(
    ctx.clone(),
    egui::Id::new("editor_root"),
    egui::UiBuilder::new().max_rect(ctx.content_rect()),
);
```

`Context::content_rect()` is the correct rect — `viewport_rect` minus anything
an OS status bar or notch might cover. `Context::available_rect` is deprecated.

Then `src/editor/ui/mod.rs::editor_ui` threads that one `Ui` through
everything, which is also what makes the layout correct: each panel shrinks the
root `Ui`'s available rect, and the `DockArea` picks up whatever is left.

```rust
egui::Panel::top("editor_menu").show(&mut root, |ui| menu::menu_bar(ui, &mut panels));
egui::Panel::top("editor_toolbar").show(&mut root, |ui| menu::toolbar(ui, &mut panels));
menu::dialogs(&ctx, &mut panels);                       // Window — still Context
egui::Panel::bottom("editor_status").show(&mut root, |ui| { … });
DockArea::new(&mut dock.0).style(style).show_inside(&mut root, &mut tabs);
```

Knock-on: `egui_dock::Style::from_egui(ctx.style().as_ref())` becomes
`Style::from_egui(root.style().as_ref())` — which is the more correct source
anyway, and sidesteps the `Context::style` → `global_style` rename entirely.

**Do this in Phase 1, not Phase 2.** egui 0.34 (the Phase 1 target) still has
`Panel::show_inside` and `egui_dock 0.19.1` still has both `show` and
`show_inside`, so the restructured code compiles against 0.34 *and* 0.36. Doing
it early means Phase 2's egui bump is a pure version bump with no chrome work,
and we avoid landing a pile of deprecation warnings in Phase 1 that we'd only
have to revisit.

Blast radius is small and contained: the five call sites in
`src/editor/ui/mod.rs` listed above. The per-panel functions already take
`(ui: &mut Ui, &mut Panels)` and are untouched, as is the `TabViewer` impl in
`viewport.rs`. This does **not** change `ViewportInput`, pointer authority, or
the one-frame-latency contract described in `docs/EDITOR_UI_DESIGN.md`.

---

## 3. Mechanical work

All of these are rename-or-wrap, verified present in the tree.

| # | Change | Release | Sites |
|---|---|---|---|
| M1 | `SceneRoot` → `WorldAssetRoot` | 0.19 | 6 — `objects.rs:550`, `simulation.rs:43`, `object_placement.rs:124`, `cafe-sim/thumbnails.rs:96`, `cafe-sim/placement.rs:104,266` |
| M2 | `SceneInstanceReady` → `WorldInstanceReady`; `bevy::scene::` → `bevy::world_serialization::` | 0.19 | `gltf_loading.rs:12,69` |
| M3 | `TextFont.font_size: f32` → `FontSize::Px(f32)` | 0.19 | **43** across `src/ui/*`, `src/menu/*`, `gltf_loading.rs:379`, `cafe-sim/{ui,staff_agent}.rs`. No `font:` field is ever set, so **no** `FontSource` work is needed. |
| M4 | `shadows_enabled` → `shadow_maps_enabled` on lights | 0.19 | 2 — `world/mod.rs:114`, `cafe-sim/thumbnails.rs:84` |
| M5 | `ShaderStorageBuffer` → `ShaderBuffer`, `GpuShaderStorageBuffer` → `GpuShaderBuffer` | 0.19 | `gpu.rs:39,143,455,463,507,617` |
| M6 | `ShaderBuffer` data model rework: `data: Option<Vec<u8>>` → `ShaderBufferData`; `with_size` takes `u64`; `set_data`/`resize_in_place` removed | **0.20** | `gpu.rs:463` (`with_size`) plus readback plumbing. Not a pure rename — re-read §"`ShaderBuffer` stores typed data in an `AlignedVec`" when you get there. |
| M7 | `#[derive(ExtractComponent)]` gains `#[extract_app(RenderApp)]`; implement `SyncComponent` | 0.19/0.20 | `post_process.rs:200` |
| M8 | Bump `ENGINE_SCRIPTS_ABI_VERSION` 6 → 7 | — | `crates/engine-api/src/lib.rs:27` + the doc comment's version history |

The egui panel renames that used to live here were promoted to §2.4 — they are
not a rename, the `Context`-based panel API is gone.

`bevy_egui`'s own API is stable across this jump: `EguiPlugin`,
`EguiGlobalSettings::auto_create_primary_context`, `EguiPrimaryContextPass`,
`EguiContexts`, `EguiTextureHandle` and `PrimaryEguiContext` all still exist in
0.43. `egui_dock` keeps `DockArea`, `DockState`, `TabViewer`, `NodeIndex`,
`Style::from_egui`. The docked-editor architecture survives intact.

---

## 4. Behaviour watch-outs (compiles fine, acts differently)

These are the ones that will waste a day if they aren't on the list.

**Resources are now components (0.19).** `Resource` is a subtrait of
`Component`. I checked: **nothing in this tree derives both**, so there's no
splitting work. The residual risk is *broad queries* — `Query<EntityMut>`,
`Query<EntityRef>`, `Query<Entity>`, `Query<Option<&T>>` — which now also match
resource entities and can conflict with a `Res<T>` param in the same system.
Fix with `Without<IsResource>`. Also note `World::clear_entities` now clears
resources too.

**`Assets::get_mut` returns `AssetMut`, not `&mut A` (0.19).** It only fires
`AssetEvent::Modified` when the value actually changes. Nine sites, all in the
bake path (`baking/{terrain,mod,gpu,object_lightmaps}.rs`,
`uv_unwrap.rs:53`, `terrain_edit.rs:363,433`, `editor/camera.rs:244,272`). This
mostly *helps* — `sync_render_target` resizing the viewport image every frame
was triggering needless re-extraction — but anything that relied on the event
firing unconditionally will silently stop.

**Dropping a `Task<T>` cancels it on web (0.19).** Previously it detached. The
WASM streaming path and the async terrain/object bake jobs need an audit for
tasks that are dropped-and-forgotten on purpose; add `.detach()` where that was
the intent.

**Built-in schedules now order sets *weakly* (0.20).** `Render`, `RenderGraph`,
`Core3d`, `ExtractSchedule` and UI sets use `chain_weak()`: non-conflicting
systems in adjacent sets may now run in any order. Our GPU bake path talks
across systems through a `GpuBakeInFlight` resource, so the ECS can see the
dependency and will keep the ordering — but `clear_in_flight_after_render`
(`gpu.rs:318`, in `RenderSystems::Cleanup`) relative to the new bake pass system
is worth an explicit `.after()` rather than trusting set membership.

**Avian: speculative CCD is no longer on by default (0.8).** `SpeculativeMargin`
became an opt-in `SpeculativeCcd` component, and
`NarrowPhaseConfig::default_speculative_margin` is gone. Our kinematic character
controller (`game/physics.rs`) does its own ground detection and wall
depenetration, so the blast radius is small, but tunnelling-at-speed is the
thing to test. Swept CCD is now on by default for fast dynamic bodies.

**Avian: `SpatialQuery` queries `&Position` and `&Rotation` (0.6).** If a system
takes `SpatialQuery` *and* mutates `Position`/`Rotation`, it now conflicts. We
use `SpatialQuery` once, so this is a quick check rather than a project.

**Lifecycle rename (0.19):** `Replace` → `Discard`, `on_replace` → `on_discard`.
Grep found no hits — we don't use those hooks.

**Same-state transitions (0.19):** `DespawnOnEnter` / `DespawnOnExit` now fire
when a state transitions to itself. If any code does `NextState::set(current)`
as a no-op, it will now despawn things. Use `set_if_different` (renamed from
`set_if_neq` in 0.20).

**`RenderDebugOverlay` joined DefaultPlugins (0.20)** with default keybindings
F1/F2 — but **disabled by default**, opt-in via `RenderDebugOverlayKeybindings`.
Our F1 (editor toggle) and F2 (edge detection toggle) are safe. Don't enable it.

---

## 5. Phasing

Each phase ends at a compiling tree with tests passing. Work directly on a
branch in the main checkout — **no worktree** for this one.

**Phase 0 — baseline.** `cargo build --workspace && cargo test` on 0.18, record
what passes. Save a couple of reference screenshots of a baked scene (I'll need
you for this — §6).

**Phase 1 — Bevy 0.19.** Pin `bevy 0.19`, `avian3d 0.7`, `bevy_egui 0.40.1`,
`egui_dock 0.19.1`. Apply:

- §2.1 — the render-graph rewrite, *including* the `target_format`
  specialization (don't leave the constant behind as debt).
- §2.4 — the editor root-`Ui` restructuring. Lands here, not Phase 2, because
  it compiles against egui 0.34 and 0.36 alike.
- M1–M5 and the `SyncComponent` half of M7.
- The §4 watch-outs that are 0.19-era: broad-query conflicts, `AssetMut`,
  web task cancellation, same-state transitions.

Shaders are untouched in this phase — naga_oil still exists in 0.19.

**Phase 2 — Bevy 0.20.** Bump to `bevy 0.20`, `avian3d 0.8`, `bevy_egui 0.43`,
`egui_dock 0.21`, `naga 30`. Apply §2.2 (WESL), §2.3 (Tonemapping), M6, the
`extract_app` half of M7, and the 0.20 watch-outs (weak set ordering, Avian
CCD defaults). The egui bump should be a no-op if Phase 1 did §2.4 properly.

**Phase 3 — ABI + plugins.** Bump `ENGINE_SCRIPTS_ABI_VERSION` to 7 (M8),
`cargo build --workspace`, confirm `cafe-sim` loads, hot-reloads (Shift+R) and
that placed script bindings still hydrate.

**Phase 4 — verification.** §6.

A note on ordering: §2.1 and §2.2 are independent and both large, but they are
naturally serialized — §2.1 *must* happen in Phase 1 (the API vanishes in
0.19), §2.2 *cannot* happen before Phase 2 (WESL doesn't exist in 0.19). Don't
try to parallelize them. §2.4 is independent of both and can be done first,
which is the cheapest way to de-risk Phase 1: it touches one file and the
result is immediately visible.

---

## 6. Verification

Compilation proves almost nothing here. The risky changes are all in the
rendering path, and `cargo test` only covers the shader parse
(`lightmap_bake.wgsl`) and the lightmap sampler assertion
(`baked_lightmaps_carry_an_explicit_linear_sampler`).

I can run `cargo build`, `cargo check`, `cargo clippy` and `cargo test`. I
can't run the app, so **I'll need you to eyeball these** after Phase 2:

1. **Toon banding** — load a baked world, confirm the 5-band shading and the
   `fwidth` antialiased band edges look identical to the Phase 0 screenshots.
   This is the single highest-risk item: it depends on the WESL translation,
   the lightmap sampler, *and* the tonemapping change.
2. **Edge detection** (F2 on/off) — the post-process rewrite is the other
   overhaul. Check edges still appear and that toggling doesn't corrupt the
   view-target ping-pong.
3. **A fresh bake** (Shift+B) — exercises the rewritten compute dispatch,
   the `ShaderBuffer` rework and the readback observer. Compare against a
   pre-upgrade `.exr`.
4. **Editor chrome** — docked viewport renders, panels lay out (M7 changed
   three of them), outliner/inspector/gizmo all respond.
5. **Character movement** — walk, jump, run into walls and off ledges, for the
   Avian CCD default change.
6. **WASM build** — the task-cancellation change (§4) only bites on web.

---

## 7. Open questions

1. ~~Is it worth tracking `ExtractedView::target_format`?~~ **Settled
   2026-10-27: yes, do it properly.** Folded into §2.1 as required Phase 1
   work; the hard-coded `Rgba16Float` goes away.
2. ~~Is the egui panel change a one-line rename?~~ **Settled 2026-10-27: no.**
   I had this wrong initially — `Panel::show` takes a `&mut Ui` in egui 0.36
   and `egui_dock 0.21`'s `DockArea` has no `Context` entry point at all. It is
   a required restructuring, promoted to §2.4 and scheduled into Phase 1.
3. **`required_plugins` / world format** are untouched by this upgrade — no
   `world.meta` or `.objects` version bump is needed. Worth confirming that
   `AnimationTargetId`'s algorithm change (0.19) doesn't affect us; we don't
   serialize animation target IDs, so it shouldn't, but the character
   animation graph (`game/animation.rs`) should be eyeballed once.

---

## 8. Phase 1 outcome (Bevy 0.19) — 2026-10-27

Landed. Resolved stack: `bevy 0.19.1`, `avian3d 0.7.0`, `bevy_egui 0.40.1`,
`egui_dock 0.19.1`, `egui 0.34.3`, `wgpu`/`naga 29.0.4`, `winit 0.30.12`
(unchanged). One egui version and one wgpu version in the graph.

**Verification:** `cargo check --workspace --all-targets` 0 errors; `cargo test
--workspace` **76 passed / 0 failed**, identical to the Phase 0 baseline;
`cargo fmt --all --check` clean; `cargo build --workspace` succeeds including
the plugin dylibs. Clippy shows 83 style warnings, all pre-existing — a
line-level cross-check against the diff found only two landing on changed
lines (both `div_ceil`, on workgroup math moved verbatim from the old node),
and those were fixed. The 7 remaining compiler warnings are pre-existing dead
code in files the migration never touched.

### What the plan predicted correctly

§2.1 (render graph), §2.4 (editor root `Ui`), M1–M5. The `target_format`
specialization was done properly as agreed — no constant left behind.

### What the plan missed

The published guide's highlights don't mention these; they only surface on
contact with the compiler. Logged so Phase 2 estimates are less optimistic:

| Found | Fix | Sites |
|---|---|---|
| `Handle<Scene>` no longer valid — `Scene` is now the *BSN* trait; the old asset is `WorldAsset` | `Handle<WorldAsset>` | 5 |
| `AlphaMode`, `OpaqueRendererMethod`, `SpecializedMeshPipelineError` moved to the new `bevy_material` crate (the last one lives in `material::specialize`, not the root) | re-path imports | 3 |
| `Hdr` moved `bevy_render::view` → `bevy_camera` | re-path import | 1 |
| `Assets::get_mut` returns an `AssetMut` guard | bind `Some(mut x)`; one site needed a `let` binding before `&mut` | 8 |
| `SystemState::get` returns `Result` (0.19 moved validation into `get_param`) | `.unwrap()` in a test | 1 |
| `RenderPassDescriptor` gained `multiview_mask` (wgpu 29) | `multiview_mask: None` | 1 |
| `depth_write_enabled` is now `Option<bool>` | `Some(false)` | 1 |
| Run-condition combinators `.and`/`.or` deprecated | `.and_then`/`.or_else` (same lazy semantics) | 14 |
| `World::register_resource` deprecated (resources *are* components now) | `register_component::<R>()` — includes the `add!` macro in engine-api, so all plugin dylibs | 3 |
| `AssetServer::load_with_settings` deprecated | `load_builder().with_settings(..).load(..)` | 1 |
| `Context::is_using_pointer` renamed | `egui_is_using_pointer` | 1 |
| Bevy 0.19's deeper `SystemParam`/auto-trait chains overflow the default 128-deep trait solver — currently a warning, **scheduled to become a hard error** | `#![recursion_limit = "256"]` on `game_engine_lib` and `engine_api` | 2 |

### Carried into Phase 2

1. **`Panel::show_inside` → `Panel::show`.** egui flips this naming in 0.35:
   `show_inside` is canonical in 0.34 but becomes the deprecated alias in 0.36,
   where `show` is the `&mut Ui` method. There is no spelling that is
   warning-free on both, so the editor currently uses `show_inside` (correct
   and warning-free on 0.34) and needs a 3-site rename at the 0.36 bump.
   `DockArea::show_inside` keeps its name and needs no change.
2. **`ShaderBuffer::with_size` takes `usize` in 0.19, `u64` in 0.20**, on top of
   the `AlignedVec`/`ShaderBufferData` rework already captured as M6.
3. **`proc-macro-error2`** (via `avian_derive 0.2.2` ← `avian3d 0.7.0`) emits a
   future-incompat warning. Not ours to fix; the avian 0.8 bump should clear it.
4. **ABI bump still pending** — `ENGINE_SCRIPTS_ABI_VERSION` stays at 6 until
   Phase 3, since nothing has forced a plugin-visible ABI change yet. The
   `add!` macro body did change, so plugins must be rebuilt regardless.

### Not yet verified

Everything in §6. Phase 1 compiles and its tests pass, but **nothing visual has
been confirmed** — the toon banding, edge detection and a fresh bake all need
eyes on them. The render-graph rewrite and the editor `Ui` restructure are both
the kind of change that compiles perfectly and renders wrong.

---

## 9. Phase 2 + 3 outcome (Bevy 0.20) — 2026-10-27

Landed. Resolved stack: `bevy 0.20.0`, `avian3d 0.8.0`, `bevy_egui 0.43.1`,
`egui_dock 0.21.1`, `egui 0.36.2`, `wgpu`/`naga 30.0.1`, `wesl 0.6.0`,
`winit 0.30.12` (unchanged). `ENGINE_SCRIPTS_ABI_VERSION` bumped 6 → 7 (Phase
3): the shared bevy types behind every FFI entry point changed, so a dylib
built against the old engine-api must be refused rather than loaded.

**Verification:** `cargo check --workspace --all-targets` 0 errors; `cargo test
--workspace` 76 passed / 0 failed, identical to the 0.18 and 0.19 baselines
(this includes the `lightmap_bake.wgsl` validation, now against naga 30);
`cargo fmt --all --check` clean; `cargo build --workspace` succeeds with the
plugin dylibs. Phase 1 was confirmed visually by the user ("all the lighting
and edge detection effect works just the same"); Phase 2 likewise.

### What the plan predicted correctly

§2.2 (WESL — all five shaders converted, `lightmap_bake.wgsl` untouched and
still validated by `cargo test`), §2.3 (`Tonemapping::Linear`), M6
(`ShaderBuffer::with_size(u64)` + `buffer_usage`), the `extract_app` half of
M7, the `Panel::show_inside → show` rename carried over from §8, and the egui
bump being otherwise a no-op thanks to §2.4 having landed in Phase 1.
Avian 0.8 compiled with **no source changes**.

### What the plan missed

| Found | Fix | Sites |
|---|---|---|
| **`ui::widget::Button` is a deprecated *type alias* in 0.20**, so using it as a *value* (`Button,` in a bundle) is a hard error, not a warning. `Interaction` is likewise an alias slated for removal after 0.20. The guide lists both as merely "deprecated" | Migrated the whole bevy_ui button model: `ui_widgets::Button` + picking `Hovered` on every button; handlers poll `Added<Pressed>` instead of `Changed<Interaction>` + `== Pressed` (same press-down timing); `update_interactive_visuals` recolours from `Has<Pressed>` / `Hovered.0`, driven by `Changed<Hovered>`, `Added<Pressed>` and `RemovedComponents<Pressed>` so retained UI isn't re-extracted every frame | 11 spawns, 20 handlers, 2 visuals systems across 14 files |
| `Hovered(bool)` is only maintained on entities that already carry it | insert `Hovered::default()` at spawn | 11 |
| `egui_dock 0.21` `TabViewer` requires `fn id(&mut self, tab) -> Id` | `Id::new(*tab)`; `EditorTab` now derives `Hash` | 1 |
| `ViewPrepassTextures::depth_view` removed (depth is `Option<DepthStencilAttachment>` for stencil support) | `depth_only_view()` — the single-aspect view a shader binding wants | 1 |
| `bevy_render::extract_component::ExtractComponentPlugin<C, F>` is a type alias that already bakes in `RenderApp`; only the *derive* needs `#[extract_app(RenderApp)]` | keep `ExtractComponentPlugin::<T>::default()` | 1 |
| `bevy::math::primitives::Cuboid` moved to the new `bevy_shape` crate | `bevy::shape::Cuboid` (tests) | 2 |

### Behaviour notes

- Button activation timing is preserved: `Added<Pressed>` fires on pointer-down
  exactly like `Changed<Interaction>` reaching `Pressed` did. bevy's own
  convention for `ui_widgets::Button` is activate-on-release (`Activate`, or
  `ActivateOnPress` to opt back in); switching to that would be a one-line
  change per handler but was deliberately *not* made in a migration commit.
- **Correction to §8:** `proc-macro-error2` is *still* pulled in by
  `avian_derive 0.2.2` under `avian3d 0.8.0`, so the avian bump did not clear
  the future-incompat warning as predicted. It is transitive and not fixable
  from here; it becomes a hard error only in a future Rust release.

### Process note

Phase 2/3 was implemented once, left uncommitted at the user's instruction
(only the 0.19 commit had been requested), and then **lost** when the worktree
was reset between sessions — `git reflog` showed `reset: moving to <phase 1
commit>`. It was reconstructed from this document plus the saved edit scripts.
Lesson: commit each phase as it goes green rather than batching, even when the
next phase is expected to follow immediately.
