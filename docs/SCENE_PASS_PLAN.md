# Replacing the bevy scene pass — plan

Status: **Phases 0–5 implemented** (plan written 2026-11-06; Phases 1, 3, 2
and 4 landed 2026-11-07, Phase 5 on 2026-11-08). The gameplay measurement in
`docs/SCENE_PASS_BASELINE.md` §3.4 is what tipped Phase 5 from "default: skip"
to worth doing: bevy_pbr was still ~1.5 ms a frame with nothing of ours in it. Prerequisite already landed:
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

**Phase 2 — Per-scene uniform refactor (ABI 15).** *Done (2026-11-07), after
Phase 3.* `scene_pass::SceneUniform { sun_dir, sun_rgb, curve, probe }` is
bind group 0 binding 3, filled every frame from the main world's `ToonLook`
(extracted with `ExtractResourcePlugin`); `ToonShading` is `{ source, unlit }`
and `TerrainShading` lost its `curve`. `sync_toon_look` — which rewrote every
toon and terrain material on each environment edit, and under the scene pass
would have rebuilt every material bind group — is gone; `ToonLook::apply` and
`ToonShading::set_sun` with it. Plugins rebuild (ABI bump); worlds and saves
are unaffected. The dead `impl Material for ToonMaterial` was cut down to the
`MeshMaterial3d` bound in the same engine-api change.

**Phase 3 — ToonMaterial wholesale.** *Done (2026-11-07), before Phase 2 —
the uniform refactor is simpler on top of our own pipeline than threaded
through bevy's, and this way it is its own commit with no render-path change
mixed in.* The terrain-specific code became a generic
`material::SceneMaterialPlugin<M>` (extract, `PreparedMaterial<M>` render
asset carrying `M::Data` as the pipeline key, per-view `queue_draws::<M>`),
with one `ScenePipeline` specializer per material in `pipeline.rs`.
`ToonScenePipeline` sets `VERTEX_UVS_A` / `VERTEX_UVS_B` / `SKINNED` from the
mesh layout under bevy's def names, so the fragment shaders' `@if`s were
untouched; `TOON_DOUBLE_SIDED` + no culling from `ToonMaterialKey`. Skinning
binds bevy's `SkinUniforms.current_buffer` as bind group 0 binding 2 and
`SceneInstance.skin_index` is `SkinUniforms::skin_index(main_entity)` at
queue time; `scene/toon_vertex.wesl` ports `skin_model` + the inverse-transpose
normal. The winding flag the double-sided flip reads comes from our instance
flags (`scene_instance_winds_forward`), replacing bevy's mesh flags. Grouping
by `(material, mesh)` is what keeps probe-lit furniture batching.
*Exit (unverified until run — `SCENE_PASS_BASELINE.md` §4):* trees/rock/table2
double-sided, player animation + probe lighting, cafe furniture draw count,
object bake arrival, web build clean.

**Phase 4 — Sky + measure + tidy.** *Done (2026-11-07).* The sky dome is a
third `SceneMaterialPlugin`, in a new `SceneLayer::Background`: no depth
*write*, **no prepass variant at all**, and sorted after the opaque layer
(`order_scene_draws`). The missing prepass variant is the contract the
atmosphere pass depends on — fog and god rays reconstruct position from
prepass depth, and a sky that wrote depth would be fogged as a surface a few
hundred metres off. Its `specialize` is gone: culling and depth-write now
come from the layer. `scene/toon_io.wesl` / `toon_vertex.wesl` were renamed
`mesh_io` / `mesh_vertex` now that three materials share them (terrain keeps
its own narrower pair). Measurement: `SCENE_PASS_BASELINE.md` §3.4-3.5.

**Phase 5 — evict PbrPlugin.** *Done (2026-11-08), in two commits.* The
gameplay table on build `19225e1`/`bc2b5fe` showed bevy_pbr still costing
~1.5 ms a frame with none of our geometry in its phases — `prepare_clusters_for_gpu_clustering`
0.40, `prepare_lights` 0.16, GPU preprocessing ~0.75 across its systems,
mesh/view bind groups, previous-frame transforms, skins — roughly five times
what our own pass costs, for four translucent overlays and a debug cone.

*5a — nothing of ours on `StandardMaterial`.* engine-api gained
`UnlitMaterial` (ABI 16): a translucent unlit tint drawn by a new
`SceneLayer::Transparent` (alpha blend, depth test, no depth write, no
prepass variant, drawn last, back to front per `(material, mesh)` run). The
placement ghost (`PendingMaterial` is now just a colour), the brush disc,
blob shadows and cafe-sim's placement ghost use it; the sun debug cone and
cafe-sim's thumbnail tints are flat toon (`shading.unlit = 1`); both thumbnail
rigs lost their `DirectionalLight` and spawn subjects with `ConvertToToon`, so
thumbnails render flat in their own colours and textures; `BakedSun` is a
bare `Transform` and `GlobalAmbientLight` is gone. cafe-sim's thumbnail
cameras — one per catalog piece, previously active forever — switch off
120 frames after the last piece's meshes appear.

*5b — `PbrPlugin` and `GizmoRenderPlugin` disabled.* What had to be replaced:
**skinning** (`scene_pass/skin.rs`: joint × inverse-bindpose matrices for
every visible `SkinnedMesh`, rebuilt each frame into our own storage buffer,
replacing bevy's `SkinUniforms` at bind group 0 binding 2); **3D line
gizmos** (`scene_pass/gizmos.rs` + `shaders/gizmos/lines.wesl`, bevy's own
shader): bevy's 3D gizmo pipeline is only installed when `PbrPlugin` is
present and is built on `MeshPipeline`'s view layout, and everything it uses
inside `bevy_gizmos_render` is private, so this is a from-scratch renderer
over `bevy_gizmos`' public `GizmoHandles`/`GizmoAsset` — solid, dotted and
dashed lines, list and strip, perspective width, depth bias; **line joints
are not supported** (nothing here configures them) and neither are retained
`Gizmo` components (nothing uses them); and **`StandardMaterial` as an
asset**, which the glTF loader still produces and `with_surface_of` converts:
`init_asset` + `register_type::<MeshMaterial3d<StandardMaterial>>` by hand,
the way Phase 1 did for terrain.

What goes with `PbrPlugin`: `MaterialPlugin<StandardMaterial>`, the mesh
render plugin (extraction, GPU preprocessing, binned phases, `mesh_view` bind
groups), lights/clusters/shadows, SSAO/SSR/fog/volumetrics/decals/atmosphere
plugins, the prepass material systems and previous-frame transforms. What
stays: `bevy_light`'s `LightPlugin` (main-world, added separately by
`DefaultPlugins`; inert with no light components), `bevy_gizmos::GizmoPlugin`
(main world — the `Gizmos` API, config store, `GizmoAsset` building).

**Phase 6 — fold the prepass into the colour pass.** *Done (2026-11-08),
unmeasured (`SCENE_PASS_BASELINE.md` §3.7).* Prompted by the Phase 5
measurement: with the CPU cost gone the gameplay frame still floored at
~7.3 ms with the render thread idle in the swapchain acquire — faster than
the ProMotion panel's refresh, so GPU-bound. The two passes rasterised every
chunk and object twice; on a tile-based GPU the colour pass already shades
nothing hidden, so the prepass bought only what it wrote. Now `scene_pass`
(in `Core3dSystems::Prepass`, before bevy's `early_prepass`, which still
copies depth for the post passes) writes colour, depth and
`ViewPrepassTextures.normal` as a second colour target; `toon_prepass.wesl`
and `terrain_prepass.wesl` are gone and the colour fragments write
`@location(1) normal`. The sky and transparent layers keep the output and
get `ColorWrites::empty()` on that target. Views without a `NormalPrepass`
(the thumbnail rigs) have no normal texture, so `SceneViewKey.normal_target`
(bevy's `Has<NormalPrepass>` on the render view) drops the target and sets
`SCENE_NORMAL_TARGET`, under which the shaders drop the output — wgpu merely
logs an unmatched output, WebGPU rejects it. Known trade: on a desktop GPU
the prepass was also early-Z against overdraw, and opaque draws are not
distance-sorted (`order_scene_draws`).
