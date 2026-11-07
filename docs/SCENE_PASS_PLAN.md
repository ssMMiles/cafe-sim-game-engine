# Replacing the bevy scene pass — plan

Status: **Phase 0 tooling in place, Phase 1 implemented** (plan written
2026-11-06, Phase 1 landed 2026-11-07). Prerequisite already landed:
`ToonMaterial` and `TerrainMaterial` are standalone `Material`s whose shaders
import no bevy PBR fragment modules (ABI 14) — the material contract a custom
pass must honour is already small and written down (`GRAPHICS.md` §5).

Measurements live in **`docs/SCENE_PASS_BASELINE.md`**, not in `memory.md`:
they are re-taken at every phase and would swamp it.

Everything in §2 marked *verified* was read out of the bevy 0.20 sources on
the date above. **V1–V6 have since been verified** against bevy 0.20.0's
sources and, for V4, against the compiler — see §5, which now records what
each turned out to be.

---

## 0. Goal and non-goals

**Goal.** The 3D view's geometry passes — the depth+normal prepass and the
opaque colour pass — become our own code: our extraction, our pipelines, our
pass systems. What that buys:

- Remove bevy_pbr's per-frame machinery for our meshes: material extraction,
  per-frame specialization checks, queue/binning/batching, GPU mesh
  preprocessing, and the large `mesh_view` bind group (lights, clusters,
  shadows, fog, SSAO, transmission — none of which a fully baked renderer
  reads).
- Own the draw loop: explicit instancing by `(mesh, material)`, explicit draw
  order, room for things bevy's Material machinery makes awkward (per-scene
  uniforms, custom per-instance data).
- Decouple from bevy_pbr's churn on upgrades. The remaining bevy surface is
  bevy_render's stable core (§2 keep-table), which is a much smaller contract.

**Non-goals.** We are **not** replacing: bevy_render (wgpu, surface/swapchain,
`PipelineCache`, shader/WESL resolver, mesh and image upload), the `Core3d`
schedule and its post stack (tonemapping, fxaa, upscaling, our edge and
atmosphere passes), the 2D path (sprite presentation, bevy_ui, egui), gizmos,
or picking. Bevy still runs the frame; we replace what fills the 3D
geometry passes.

## 1. What the 3D camera draws today

| What | Material | Count / notes |
|------|----------|----------------|
| Terrain chunks | `TerrainMaterial` | ~1 draw per loaded chunk×LOD (ring of ≤81); each chunk its own material (its lightmap) |
| Placed objects, lightmapped | `ToonMaterial` (lightmap source) | one material per sub-mesh entity (own lightmap) → no batching possible by design |
| Dynamic/probe-lit (player, furniture, plugin pieces) | `ToonMaterial` (probe source) | shared material per glTF material / colour → **batches today; our pass must keep that** |
| Player | `ToonMaterial`, **skinned** | the one skinned mesh; animation via bevy `AnimationGraph` |
| Sky dome | `SkyDomeMaterial` (plain bevy `Material`) | 1 draw, cull off, depth write off |
| Stragglers | `StandardMaterial` | placement preview ghost + cafe ghost (alpha blend), brush disc, blob shadows (alpha blend), thumbnail rig (layer 7, own camera) |
| Gizmos (editor, avian) | bevy_gizmos | own pipeline, untouched |

Post stack (unchanged throughout): edge detection → atmosphere →
`Tonemapping::Linear` → fxaa → upscaling; the two custom passes read
`ViewPrepassTextures` (depth + normal, **MSAA 4**). Whatever we build must
keep writing those textures or outlines and fog die.

## 2. The machinery in play (bevy 0.20, verified)

- The per-view render "graph" is gone in 0.20: **`Core3d` is a schedule** with
  sets `Prepass → MainPass → EarlyPostProcess → PostProcess`
  (`bevy_core_pipeline::schedule`), and the passes are plain systems taking
  `ViewQuery<…>` + `RenderContext`. Our `post_process.rs` and `atmosphere.rs`
  already are exactly such systems — **the extension mechanism is proven
  in-repo.**
- The pass drivers (`early_prepass`/`late_prepass`,
  `main_opaque_pass_3d`/`main_transparent_pass_3d`) live in
  bevy_core_pipeline and iterate `ViewBinnedRenderPhases<Opaque3d>` etc.; the
  *items* come from bevy_pbr's queue systems. No items → nothing drawn.
- `prepare_core_3d_depth_textures` and `prepare_prepass_textures` are
  **bevy_core_pipeline** (`core_3d/mod.rs`), so `ViewDepthStencilTexture` and
  `ViewPrepassTextures` exist regardless of bevy_pbr.
- `extract_meshes_for_gpu_building` (bevy_pbr) is **change-driven**
  (`Changed<GlobalTransform>` etc.): a static editor scene costs little there.
  The recurring per-frame costs are specialization checks, queueing,
  binning/batching, the GPU preprocess dispatch, light/cluster prep and the
  view bind group build.
- `bevy_pbr::render::skin::{SkinUniforms, extract_skins, prepare_skins}` are
  `pub` — while `PbrPlugin` is still in the app they keep running, and a
  custom pipeline can **bind bevy's prepared joint-matrix buffer** instead of
  porting skinning.
- Reusable from bevy_render without bevy_pbr: `ViewUniforms` /
  `prepare_view_uniforms` (clip_from_world and friends),
  `MeshAllocator::{mesh_vertex_slice, mesh_index_slice}` +
  `RenderAssets<RenderMesh>` (GPU meshes + vertex layouts), `GpuImage`,
  `PipelineCache` + the `Specializer`/`Variants` API post_process.rs already
  uses, `AsBindGroup` (`ToonMaterial` already derives it), and visibility +
  frustum culling (main world, material-agnostic).
- Asset-loaded WESL shaders get module paths derived from their asset path
  (`bevy_shader::Shader::from_wesl` → `shaders/foo.wesl` importable as a
  module). A *shared* toon-curve module — today duplicated verbatim across two
  shaders and mirrored in Rust — looks feasible (**V4**).

### Keep / drop / keep-for-now

| Keep (bevy_render / core_pipeline) | Drop for our types (bevy_pbr) | Stays while PbrPlugin stays |
|---|---|---|
| Surface, swapchain, `ViewTarget`, MSAA | `MaterialPlugin::<ToonMaterial>` and `::<TerrainMaterial>`: extract/specialize/queue, prepass plugins | `extract_meshes_for_gpu_building` (change-driven, cheap when still) |
| `Core3d` schedule + tonemapping/fxaa/upscaling + our post passes | `mesh_view` bind group contents for our draws | light extraction + clustering for the one `BakedSun` |
| `PipelineCache`, WESL resolver, `MeshAllocator`, `GpuImage`, view uniforms, visibility | binning/batching + GPU mesh preprocessing for our items | view bind group build, preprocess dispatch over leftover (straggler) items |
| RenderDiagnostics (GPU timing panel) | | `SkinUniforms` prep (we *want* this — free skinning) |

`PbrPlugin` itself stays through Phases 1–4: the StandardMaterial stragglers
(ghosts, blob shadows, brush disc, thumbnails) render through it, and it keeps
preparing skins. Whether its fixed residue is worth evicting is a **measured**
decision in Phase 5, not an assumption.

## 3. Architecture

New module `src/game/gfx/scene_pass/` with a `ScenePassPlugin`.

**Main world: nothing changes for authoring.** `Mesh3d` +
`MeshMaterial3d<ToonMaterial>` / `MeshMaterial3d<TerrainMaterial>` stay the
components (they're plain component wrappers, usable without
`MaterialPlugin`), so placed objects, plugins, glTF conversion, the editor and
the ABI are untouched by the pass itself. The one ABI change in this plan is
the deliberate per-scene-uniform refactor (Phase 2).

**Render world dataflow:**

1. **Extract** (`ExtractSchedule`): visible instances
   (`ViewVisibility` + `MeshMaterial3d<T>`) → a `SceneInstances` list:
   `{ mesh id, material id, world_from_local, flags }` where flags carry the
   3×3 determinant sign (winding correction for mirrored meshes — computed on
   CPU, replacing bevy's `Mesh.flags` bit our shaders read today) and a skin
   index for the skinned path. v1 rebuilds the list every frame — at the
   hundreds of entities this project has, that is microseconds; a retained map
   keyed by `MainEntity` is the known upgrade if the Systems table ever says
   otherwise.
2. **Prepare** (`RenderSystems::PrepareResources` / `PrepareBindGroups`):
   - *Scene uniform* (view-independent): sun_dir, sun_rgb, toon curve, probe
     cell size/dims. One buffer for every draw (see Phase 2).
   - *Instance buffer*: sort instances by `(pipeline key, material, mesh)`,
     upload one storage buffer, and emit a flat
     `Vec<SceneDraw { pipeline, material bind group, mesh, instance_range }>`.
     One draw per group ⇒ probe-lit furniture keeps batching; lightmapped
     objects are groups of 1 by design; terrain is 1 per chunk either way.
   - *Material bind groups* via `AsBindGroup::as_bind_group`, cached per
     material `AssetId`, invalidated by `AssetEvent`s — lightmaps arriving
     (`apply_object_lightmaps`, terrain bootstrap/bake), inspector edits,
     `sync_dynamic_lit_source` swapping copies, Shift+L all mutate material
     assets today and keep working unmodified.
   - *Pipelines*: a specialized cache keyed on
     `{ mesh vertex layout, double_sided, skinned, has UV0, has UV1, pass
     (colour/prepass), msaa, target format }` — the existing
     `Specializer`/`Variants` pattern from post_process.rs, or bevy_render's
     `SpecializedMeshPipelines`.
3. **Draw** (`Core3d` schedule, per view, exactly like `edge_detection_pass`):
   - `scene_prepass` in `Core3dSystems::Prepass`: one render pass over
     `ViewPrepassTextures` (normal target + prepass depth), drawing every
     `SceneDraw` with the prepass pipeline variant. Ordering vs bevy's
     `early_prepass`/`late_prepass` and attachment Clear/Load ops: **V2**.
   - `scene_opaque` in `Core3dSystems::MainPass`,
     `.before(main_opaque_pass_3d)`: one render pass over the `ViewTarget`
     colour attachment + `ViewDepthStencilTexture`, same draw list with the
     colour pipelines. Bevy's opaque/transparent passes still run after for
     the stragglers and depth-test against what we wrote. Clear-ownership when
     bevy's phases are empty: **V3**.
   - Bind group 0 is ours: view uniform, scene uniform, instance buffer,
     (skin buffer). Material stays its own group — pass
     `MATERIAL_BIND_GROUP` as a shader def from our pipeline (it is just an
     int def today, set by bevy to 3; we set our own) so the fragment shaders
     keep their `@group(constants::MATERIAL_BIND_GROUP)` lines untouched.

**Shaders.** New `assets/shaders/scene/` modules: a view module (`View` struct
+ `position_world_to_clip`), an instance module (instance struct + fetch by
`instance_index`), and the vertex shaders — one static/skinned object vertex
(`@if(SKINNED)`), terrain keeps its skirt-flipping vertex ported onto the
instance buffer. The *fragment* bodies of `toon_lighting.wesl` /
`terrain_toon.wesl` stay as they are; their only bevy_pbr imports
(`forward_io::VertexOutput`, `mesh` flags) move to our modules —
`toon_prepass.wesl` likewise. `shader_check.rs` gains the new modules in its
stub resolver (and loses most of the stubs, since the imports become our own
files); `DUMP_WGSL` + `tint-check.mjs` stays the WebGPU-strictness check.
If **V4** (asset-to-asset imports) pans out, the duplicated toon-curve block
becomes one module and `toon_curve_matches_the_shaders` degenerates into an
ordinary unit test against a single copy.

**Skinning.** The player is the only skinned mesh. Phases 1–4 bind bevy's
`SkinUniforms` buffer (prepared by the still-present PbrPlugin) and set
`SKINNED` on the player's pipeline. Only Phase 5 (dropping PbrPlugin) would
require porting `extract_skins`/`prepare_skins` — both `pub`, together a few
hundred lines.

**Migration rule: one material type at a time, atomically.** An entity must
never be drawn by both paths (double draw), and bevy offers no per-entity
opt-out of a registered `MaterialPlugin` — so a type moves wholesale:
unregister `MaterialPlugin::<T>` and have the scene pass own every `T` in the
same change. That ordering is why terrain goes first (no textures-from-glTF,
no skinning, no double-sided, no sharing) and toon goes after the groundwork
is proven.

## 4. Phases

**Phase 0 — Baseline.** Protocol, the row lists, and the tables themselves
live in **`docs/SCENE_PASS_BASELINE.md`**. The Performance panel's Systems
(CPU) section grew a **Save snapshot** button
(`system_timing::snapshot_markdown`) so a before/after is a file rather than a
transcription from a screenshot. *Status: tooling and the row lists are in; the
tables need a `cargo run-profiling` session, which only a human can drive.*

**Phase 1 — Terrain through the scene pass.** *Done (2026-11-07).*
`src/game/gfx/scene_pass/` — plugin + instance buffer + bind group (`mod.rs`),
the specializer (`pipeline.rs`), terrain extract/queue and the material render
asset (`terrain.rs`), the two pass systems (`pass.rs`); shaders in
`assets/shaders/scene/`. `MaterialPlugin::<TerrainMaterial>` is gone, replaced
by a hand-written `init_asset` + `register_type` (**V1**), and the material
bind group is still bevy's `AsBindGroup` derive, reached through a
`RenderAsset` impl so every existing path that mutates a terrain material
(bake arrival, Shift+L, bootstrap) keeps working untouched. **V2/V3** both
resolved to "run before bevy's pass and let first-use clearing do its job".
Two things were fixed in passing: skirt normals are flipped in the *prepass*
too (bevy's default prepass vertex didn't — `GRAPHICS.md` §17), and back faces
are culled in both passes rather than only the colour one.
*Exit (unverified until someone runs it — `SCENE_PASS_BASELINE.md` §4):*
outlines, fog, god rays, Shift+L, bake arrival, chunk streaming and the web
build all visually unchanged; draw count = chunk count; Phase 0 rows for
terrain gone.

**Phase 2 — Per-scene uniform refactor (ABI 15).** Move sun, toon curve and
probe constants out of `ToonShading`/`TerrainShading` into the scene uniform;
shrink `ToonShading` to `{source, unlit}`. Today `sync_toon_look` rewrites
*every* toon material on any environment change — under the scene pass that
would be a full bind-group rebuild storm, with the scene uniform it is one
buffer write. Plugins rebuild (ABI bump); worlds/saves unaffected. This can
land while toon still renders through bevy (the Material path reads the same
uniform via a small shared binding) or be folded into Phase 3 — decide when
Phase 1 has settled the bind-group layout.

**Phase 3 — ToonMaterial wholesale.** The big one: textures + UV transform,
double-sided variants (`TOON_DOUBLE_SIDED` def from our key), lightmapped vs
probe-lit, the skinned player via bevy's `SkinUniforms`, material-asset event
handling (lightmap arrival, source swaps, Shift+L), and instance grouping so
probe-lit furniture still batches. Unregister `MaterialPlugin::<ToonMaterial>`.
*Exit:* the full visual checklist (trees/rock/table2 double-sided, player
animation + probe lighting, cafe furniture batching = draw-count check, object
bake arrival), plugins unchanged apart from the Phase 2 rebuild, web build
clean through `tint-check`.

**Phase 4 — Sky + measure + tidy.** Port the sky dome (one pipeline: cull
off, depth write off, draw last in our opaque pass). Re-run Phase 0's
measurement; update `docs/PERFORMANCE.md` / `GRAPHICS.md`; add
RenderDiagnostics spans around our two passes so the GPU-timings panel keeps
naming them (**V6**).

**Phase 5 — optional: evict PbrPlugin.** Only if Phase 4's numbers say the
residue matters. Requires porting: skinning (extract/prepare), the stragglers
(ghosts + blob shadows need a small alpha-blend pipeline in our pass with
back-to-front sorting over a handful of entities; brush disc likewise), and a
lit path for thumbnails (or accepting flat thumbnails); plus verifying
bevy_gltf still functions without `PbrPlugin` registering
`Assets<StandardMaterial>` (the glTF loader and `with_surface_of` conversion
still need the asset type to exist). `DirectionalLight`/`GlobalAmbientLight`
and `world/mod.rs`'s derivation of them go with it. Default expectation:
**skip** — a near-empty bevy_pbr is probably cheap, and this phase is pure
risk otherwise.

## 5. Verification items — resolved 2026-11-07

Read out of bevy 0.20.0's published sources (`~/.cargo/registry/.../bevy_*-0.20.0`),
except V4 which was also run through the WESL compiler.

- **V1 — settled, and smaller than feared.** `MaterialPlugin::<M>::build` does
  exactly: `init_asset::<M>()`, `register_type::<MeshMaterial3d<M>>()`,
  `init_resource::<EntitiesNeedingSpecialization<M>>()`,
  `ErasedRenderAssetPlugin::<MeshMaterial3d<M>>`, two `PostUpdate` systems
  (`mark_meshes_as_changed_if_their_materials_changed`,
  `check_entities_needing_specialization`) and, in the render app, the shader
  cache, the bind-group allocator and four extract systems. Only the first two
  have to be kept, and `terrain/mod.rs` now does them by hand. `MeshMaterial3d<M>`
  is bounded on `M: Material`, so the `impl Material for TerrainMaterial` stays
  — gutted to a doc comment plus `enable_shadows`, since nothing reads it.
- **V2 — run before `early_prepass`.** The prepass node renders depth into the
  view's *main* `ViewDepthStencilTexture` and `copy_texture_to_texture`s it
  into `ViewPrepassTextures.depth` at the end of the pass, so our geometry has
  to be in the depth buffer before bevy's prepass finishes. Both `ColorAttachment`
  and `DepthAttachment` clear on first use and load afterwards (an `AtomicBool`
  per attachment), so running first means we clear and bevy loads. `early_prepass`
  returns early only if the view's prepass phases are *absent* — being empty is
  not enough — and those phases come from bevy_core_pipeline's
  `extract_camera_prepass_phase`, which keys off `DepthPrepass`/`NormalPrepass`
  on the camera and knows nothing about bevy_pbr. So it always runs for our
  camera and always does the copy.
- **V3 — same answer, same reason.** `main_opaque_pass_3d` also early-returns
  only on *absent* phases (`extract_core_3d_camera_phases`, also
  bevy_core_pipeline), and it begins its render pass before testing whether the
  phases have items — so the clear happens either way. `scene_opaque` runs
  `.before(main_opaque_pass_3d)`, clears on first use, and bevy's pass loads and
  depth-tests the stragglers against what we wrote.
- **V4 — yes, asset shaders can import each other, and Phase 1 relies on it.**
  `Shader::from_wesl` gives an asset at `shaders/a/b.wesl` the module path
  `/shaders/a/b` (`PathOrigin::Absolute`), `scan_wesl_imports` resolves
  `package::`/`super::` against it, and `ShaderLoader` turns every resulting
  `ShaderImport::AssetPath` into a loaded `file_dependencies` entry — so the
  imported file is loaded and registered in `module_path_to_asset_id` without
  anything holding a handle. The scene shaders import each other as
  `package::shaders::scene::…`, and `shader_check.rs` compiles the graph in
  `cargo test`. (So the shared toon-curve module is feasible — Phase 2/3 work,
  not done here.)
- **V5 — nothing exotic.** Bind group 0 is a dynamic-offset uniform (bevy's own
  `ViewUniform`) and a read-only storage buffer in `VERTEX_FRAGMENT`; both are
  core WebGPU. `cargo web-check` passes. The design only *removes* bindings.
- **V6 — works, same call as the post passes.** `ctx.diagnostic_recorder()` +
  `diagnostics.pass_span(&mut pass, "scene prepass" | "scene opaque")`, exactly
  as `post_process.rs` does. Still empty on macOS, for the Metal reason in
  `docs/PERFORMANCE.md`.
- **V7** — (note, no action) editor picking is `MeshRayCast` on main-world
  mesh data and never touches the render path.

### Also learned while building Phase 1

- **Per-view visibility is `RenderVisibleEntities`**, populated by
  `collect_visible_cpu_culled_entities` in bevy_render's `ViewPlugin`
  (`RenderSystems::PrepareAssets`), keyed by visibility class — so
  `visible.get::<Mesh3d>()` is the frustum-culled, render-layer-filtered list
  for that view, with no bevy_pbr involved. Using it rather than a global
  `ViewVisibility` sweep is what keeps terrain out of the thumbnail rig's
  camera (layer 7).
- **`@builtin(instance_index)` is `firstInstance + i` by WebGPU spec**, so a
  draw over an instance range indexes its own slice of the instance buffer
  with no extra uniform. Bevy's own batching depends on this too.
- **The instance buffer must go through `encase`, not `bytemuck`.** glam's
  `Mat3` is 36 bytes; WGSL's `mat3x3<f32>` is 48. `BufferVec` (encase) pads
  correctly; `RawBufferVec` (Pod) would shear every instance past the first.
- **Bevy's prepass does not cull back faces while its opaque pass does**
  (`PrimitiveState::default()` vs `cull_mode: Some(Face::Back)`). Ours culls
  in both: a back face winning the depth buffer on a pixel the colour pass
  then discards is a hole with fog in it.
- **`ViewQuery` skips its system when the view doesn't match** (a
  `SystemParamValidationError::skipped`), so a pass system can simply require
  `&SceneDraws` and stay silent on views that have none.

## 6. Expected wins — hypotheses to check against Phase 0

Honest framing: with a *static* scene, bevy's extraction is change-driven and
cheap; nobody should promise "N ms back" before Phase 0's table exists. What
the design removes per frame regardless: per-material-type specialize+queue
systems, binned-phase build + batching, the GPU preprocess dispatch for our
items, and the `mesh_view` bind-group rebuild — plus, with Phase 2, the
write-every-material environment sync. What it *adds*: our own extract +
prepare, deliberately simple (one list rebuild, one buffer upload). The
secondary win is wall-clock in the render pass itself (two tight passes, no
per-item draw-function dispatch); the primary win is strategic ownership.

Size estimate: Phase 1 ≈ 600–900 lines (plugin + buffers + pipelines + two
pass systems + shader modules), Phase 3 ≈ the same again rather than more
(the machinery exists; it gains variants), Phases 2/4 small. Comparable in
shape to `atmosphere.rs` + `post_process.rs`, which is the house style it
should follow.

## 7. Alternatives considered

- **Queue custom phase items into bevy's `Opaque3d`/`Opaque3dPrepass`**
  (they live in bevy_core_pipeline, so it works without bevy_pbr): keeps
  bevy's pass drivers, binning, sorting and batching — which is exactly the
  machinery we're trying to shed, and it drags in `GetBatchData`/binned-item
  plumbing. Kept as the fallback if direct pass systems fight the schedule
  (V2/V3 turning out hostile).
- **Fork/trim bevy_pbr**: maximum control, but every bevy upgrade becomes a
  merge. Rejected.
- **Stay on `Material` and slim the view bind group**: not possible —
  `MaterialPlugin` hardwires `MeshPipeline` and its layout.

Decisions already taken: authoring components stay (`MeshMaterial3d<T>`);
material types migrate atomically (no double-draw window); PbrPlugin stays
until measurement justifies Phase 5; post stack and prepass *textures* are
bevy's, only the geometry that fills them is ours.
