# Graphics pipeline: technical reference

As built on 2026-11-04 (Bevy 0.20, `webgpu` on the web). This is the map of
the whole graphics stack: every camera, target, material, pass and the data
that feeds them. Where a subsystem has its own reference, this doc
summarises it and links to it rather than repeating it. For the short,
human-readable version, see [`GRAPHICS_OVERVIEW.md`](GRAPHICS_OVERVIEW.md).

Paths are relative to the repo root. `file:line` references were correct on
the date above. Line numbers drift, but the names don't.

**Contents**
- §1 Doc map
- §2 The frame
- §3 Cameras and layers
- §4 Render target and presentation
- §5 Materials
- §6 Light at runtime
- §7 Geometry
- §8 Post-processing
- §9 Atmosphere
- §10 Editor-only visuals
- §11 Debug toggles
- §12 The bake (offline)
- §13 Streaming and when things appear
- §14 Native vs web
- §15 Shader inventory
- §16 Measured costs
- §17 Things that don't do what they look like
- §18 Stale docs and comments

---

## 1. Doc map

| Topic | Reference | Status |
|-------|-----------|--------|
| Everything lighting: units, the bake equation, the toon curve, knobs, invariants, defects | [`LIGHTING.md`](LIGHTING.md) | As-built. Parts of §2/§6.2 predate the incremental probe fill (§18) |
| Outline pass | [`EDGE_DETECTION.md`](EDGE_DETECTION.md) | As-built |
| Probe volume + blob shadows design | [`DYNAMIC_LIGHTING_PLAN.md`](DYNAMIC_LIGHTING_PLAN.md) | §8 was as-built on 2026-10-28; its numbers are stale (§18) |
| Plugin-spawned objects' lighting | [`PLUGIN_LIGHTING_PLAN.md`](PLUGIN_LIGHTING_PLAN.md) | Header current, body historical |
| Moving the object bake to the GPU | [`GPU_OBJECT_BAKE_PLAN.md`](GPU_OBJECT_BAKE_PLAN.md) | Plan; stage 1 done |
| Tracing the web build; measured hitches | [`WEB_PROFILING.md`](WEB_PROFILING.md) | Current |
| 0.18 → 0.20 migration (render graph → systems, naga_oil → WESL) | [`BEVY_0_20_UPGRADE_PLAN.md`](BEVY_0_20_UPGRADE_PLAN.md) | Historical record |
| Bevy 0.17 render graph | `RENDER_GRAPH.md`, `BEVY_RENDER_GRAPH_NODES.md` | **Obsolete**: that API no longer exists |

---

## 2. The frame

```
MAIN WORLD (Update / PostUpdate)
  streaming: chunk meshes + lightmaps + probes arrive      (§13)
  glTF scenes → toon conversion (on_scene_ready)           (§5.4)
  ToonLook / sky / edge uniforms follow the environment     (§6)
  probe volume: fill newly exposed slab within budget       (§6.3)
  editor: egui pass (EguiPrimaryContextPass), gizmos
        │ extract
        ▼
RENDER WORLD, gameplay Camera3d → GameRenderTarget (Rgba16Float, MSAA ×4)
  probe uploads  write_texture into the 4 volume textures   (RenderSystems::PrepareResources)
  Core3d:
    Prepass        depth + world normals    (every opaque mesh; sky dome writes normals, no depth)
    MainPass       opaque: terrain, toon objects, sky dome, StandardMaterial bits
                   transparent: blob shadows, placement preview, brush disc, gizmos
    (volumetric fog: scheduled, but a no-op, §9)
    PostProcess    tonemapping (Linear) → edge detection → FXAA
    upscaling
        ▼
RENDER WORLD, Camera2d → window
  gameplay: the target as a full-window sprite (layer 1), bevy_ui on top
  editor:   egui dock; the viewport tab draws the target as an egui texture
```

Nothing lights a surface at runtime. Every pixel's brightness is a baked
number: a lightmap texel, or a probe sample plus a sun term. That number
goes through the per-world **toon curve** (5 bands by default) and is
multiplied by the base colour. The outline pass then draws lines from the
prepass.

---

## 3. Cameras and layers

| Camera | Where | Target | Layers | Notable components |
|--------|-------|--------|--------|--------------------|
| Gameplay `Camera3d` (`PlayerCamera`) | `src/game/gfx/camera.rs:84-127`, spawned `OnEnter(InGame)` | `GameRenderTarget` image | 0 | `Hdr`, `Msaa::Sample4` (default), `DepthPrepass`, `NormalPrepass`, `Tonemapping::Linear`, `Fxaa`, `EdgeDetectionSettings`, `DistanceFog`, `VolumetricFog` |
| Presentation `Camera2d` | `camera.rs:140-155` | window | 1 | `PrimaryEguiContext`, `IsDefaultUiCamera`, `Fxaa` |
| Main-menu `Camera2d` | `src/menu/mod.rs:58` | window | 0 (default) | despawned on leaving the menu |
| Editor thumbnail rig | `src/editor/thumbnails.rs:186-221` | 128 px `Rgba8UnormSrgb` per job | 7 | `is_active: false` until a job; `Tonemapping::Linear`; own `DirectionalLight` (12 000 lux) |
| cafe-sim thumbnails | `plugins/cafe-sim/src/thumbnails.rs:68-90` | 256² `Bgra8UnormSrgb` each | 8 + i | default tonemapping (see §17) |

**Projection.** Perspective, `near` 0.1 and `far` 1000 (Bevy defaults; the
depth buffer is reverse-Z infinite, so depth = `0.1 / distance`). The fov
spawns at π/5. In gameplay, `camera_follow` sets it every frame: π/2.5
(72°) in first person, π/5 (36°) in third (`camera.rs:34-35, 293, 319`). The
editor keeps whatever the camera has.

**Layers.**

| Layer | Contents |
|-------|----------|
| 0 | The world, plus the default for everything unlabelled (gizmos too) |
| 1 | The presentation sprite and its `Camera2d` |
| 7 | The editor thumbnail rig and its subjects (the layer is stamped onto meshes as they appear; it isn't inherited) |
| 8+ | cafe-sim thumbnails |

**One camera, two drivers.** The gameplay `Camera3d` is the only 3D view.
In the editor, `editor_fly_camera` drives it; in gameplay, `camera_follow`
does. `WorldFocus` (the camera in the editor, the player in gameplay) is
what streaming, the probe window and the bake centre on.

---

## 4. Render target and presentation

`GameRenderTarget` (`camera.rs:46-79`):
- 1920×1080 `Rgba16Float`, usage `TEXTURE_BINDING | COPY_DST | RENDER_ATTACHMENT`.
- **Nearest sampler.** It is presented, not sampled by materials. The
  nearest filter is a deliberate pixel look (memory.md notes the user wants
  to revisit how it re-quantises on HiDPI).

| | Gameplay | Editor |
|---|---|---|
| Size | 1920×1080 fixed | the viewport tab's physical pixel size, after it holds for 2 egui passes (`sync_render_target`, `src/editor/camera.rs:208-252`) |
| Shown by | `Sprite` on layer 1, `custom_size` = window logical size every frame (`resize_render_texture_sprite`) | `egui::Image` of the texture (bevy_egui binds float textures and does its own linear→sRGB, so the colours match) |
| Switch | `OnExit(EditorMode::Editor)` → `restore_render_target` (sprite visible, back to 1080p) | `sync_render_target` hides the sprite each frame |

egui is configured with `UiRenderOrder::BevyUiAboveEgui` (`src/editor/ui/mod.rs:74-77`).

---

## 5. Materials

Every lit surface is a Bevy `ExtendedMaterial` over `StandardMaterial`. The
base provides colour, the base colour texture and the prepass. The
extension replaces the fragment shader. Both toon materials force
`AlphaMode::Opaque`.

### 5.1 `ToonMat` = `ExtendedMaterial<StandardMaterial, ToonMaterial>`

Placed meshes, the player, and plugin pieces.
- Types: `crates/engine-api/src/toon_material.rs` (shared with plugin dylibs; ABI-gated).
- Host sync: `src/game/gfx/toon_material.rs`.
- Shader: `assets/shaders/toon_lighting.wesl` (fragment and deferred). Vertex and prepass are Bevy's.

| Binding | What |
|---------|------|
| 100/101 | `lightmap` texture + sampler (an object's baked EXR; else the 1×1 white `DefaultToonLightmap`) |
| 102 | `ToonShading` uniform: `source` (1 lightmap / 2 probes), `unlit`, `probe_cell_size`, `probe_dims`, `sun_dir`, `sun_rgb`, `curve` (bands, floor, white, gamma) |
| 103/104 | probe `c0` 3D texture + its repeat/linear sampler |
| 105–107 | probe `c1`–`c3` |

Fragment logic (`toon_lighting.wesl:327-394`):
- **Colour.** `base_color` comes from the material uniform, multiplied by the
  base colour texture on UV0 when the material has one (respecting
  `uv_transform`). Vertex colours, normal maps, emissive and
  metallic/roughness are ignored.
- **Radiance.** White by default (the unlit view, or no UV1). Probe source:
  `probe_radiance(world_pos, n)` (§6.2). Lightmap source: the lightmap on UV1.
- **Output.** `base × toon_shade(radiance, curve)`.
- Branches test a `let` copied from the uniform, which WebGPU's uniformity
  rule requires (§14).

### 5.2 `ExtendedMaterial<StandardMaterial, TerrainMaterial>`

`src/game/world/terrain/material.rs`; vertex `terrain.wesl`, fragment
`terrain_toon.wesl`.

| Binding | What |
|---------|------|
| 100/101 | `stone_texture` (`textures/terrain.png`, repeat) |
| 102/103 | the chunk's `lightmap` |
| 104 | `TerrainShading { use_lightmap, curve }` (note: `use_lightmap` = 1 is *lit*, the opposite polarity of `ToonShading::unlit`) |

- **Base.** Flat sRGB (0.81, 0.62, 0.60).
- **Stone.** Darkened by a *stochastic no-tile* sample of the stone texture
  at `world.xz / 3`: four randomly offset lattice cells blended with
  smoothstep weights, at a fixed mip 1, mixed towards near-black by up to 85%.
- **Lightmap.** Sampled on UV1, which is a planar 0–1 mapping across the chunk.
- **Toon curve.** Identical to the toon shader's. The block is duplicated
  verbatim and `toon_curve_matches_the_shaders` holds both copies and the
  Rust mirror together.

The vertex shader flips **skirt** normals (zero-length in the mesh) to
straight down. The prepass uses Bevy's vertex shader, which doesn't do this
(§17).

Each chunk gets its own clone of the base material, carrying its own
lightmap (`bootstrap.rs:242-253`).

### 5.3 `SkyDomeMaterial`

`src/game/world/sky.rs`, `sky_dome.wesl`.
- **Mesh.** A plain `Material` on a 900 m UV sphere (32×16).
- **Pipeline.** `cull_mode: None`, depth write off, forward.
- **Position.** Follows the camera's translation every frame.
- **Shader.** A gradient from `horizon` to `zenith` (`pow(dir.y, 0.6)`), a
  sun disc (`smoothstep(0.9990, 0.9996, ·)`) plus a `pow(·,16)` halo, and
  blends to `ground` below the horizon.
- **Colours.** `SkyColors::from_environment` derives them from the ambient,
  the sun and `1/white`. LIGHTING.md §6.1 flags those gains as a guess
  that hasn't been checked by eye.

### 5.4 glTF → toon conversion

`src/game/gltf_loading.rs`.
- **The observer.** One global observer, `on_scene_ready`, handles every
  `WorldAssetRoot` scene instance, placed or plugin-spawned alike.
- **What converts.** A root carrying `ConvertToToon` (engine-api) has each
  mesh's `StandardMaterial` cloned in as the *base* of a new `ToonMat`, so
  colour, textures and `uv_transform` survive.
- **Source.** `DynamicLit` on the root picks the probe source; otherwise it
  is the lightmap source.
- **Sharing.**
  - Probe-lit conversions share one `ToonMat` per glTF material
    (`ProbeToonMaterials`), so identical moving pieces batch.
  - Lightmapped conversions get one material per mesh entity, since each
    has its own lightmap.
- **Toggling.** `sync_dynamic_lit_source` (editor only) swaps a private
  copy in when `DynamicLit` is added or removed after conversion.

| Who | Markers |
|-----|---------|
| Placed mesh objects | `ConvertToToon` + `NeedsObjectLightmaps` (`objects.rs:921-922`) |
| Player (`meshes/character/…`) | `ConvertToToon` + `DynamicLit` + `BlobShadow` (`simulation.rs:44-51`) |
| cafe-sim furniture | `ConvertToToon` + `DynamicLit` (`plugins/cafe-sim/src/placement.rs:330`) |
| Some plugin pieces | hand-built probe-source `ToonMaterial`s (`cafe-sim/src/shading.rs`, `example2`) |

### 5.5 What stays on `StandardMaterial`

These are the only things the runtime `BakedSun` directional light and
`GlobalAmbientLight` affect (§6.4):
- Placement preview: translucent grey, `AlphaMode::Blend`, via `PendingMaterial`.
- Terrain brush disc: unlit, blended.
- The sun debug cone.
- Editor thumbnails: lit by their rig's own light, not the world's.
- Blob shadow discs: unlit, blended.
- cafe-sim's `PaintShading::Pbr` tints.

### 5.6 Blob shadows

`src/game/blob_shadow.rs`.
- **The disc.** A `BlobShadow { radius, fade_height }` on a root gets a
  separate, unparented disc: a shared unit circle with a shared unlit
  blended material, black at 0.55 alpha, through a 64² radial falloff
  texture with a linear sampler.
- **Placement.** Each frame an avian ray is cast down from the caster
  (lifted 0.3 m, reach 40 m, excluding the caster's own hierarchy). The
  disc sits 2 cm above the hit, aligned to the hit normal, and is scaled
  and faded by height.
- **Edges.** Because it's blended, it isn't in the prepass, so the outline
  pass never draws round it.

---

## 6. Light at runtime

[`LIGHTING.md`](LIGHTING.md) is the reference. This is the runtime half in
brief.

### 6.1 Static surfaces: lightmaps
- **Terrain.** One EXR per chunk (`Rgba16Float`, outgoing radiance in
  *lightmap units*, 1.0 = full base colour), decoded in the chunk's
  meshing task.
- **Objects.** One EXR per toon sub-mesh, on UV2 that `uv_unwrap` generates:
  every triangle gets its own atlas cell, and the shared mesh asset is
  mutated once (`LightmapUvCache`).
- **Sampler.** Always an explicit **linear** sampler (`into_image`). The
  app default is nearest, which would staircase every band edge.
- **Unbaked.** An unbaked surface binds a 1×1 white texel and renders fully lit.

### 6.2 Moving things: the probe volume
- **What it stores.** Irradiance on a world lattice of **1 m** cells
  (`PROBE_CELL_SIZE`), baked per chunk and streamed into `ProbeStore`.
- **The textures.** Four `Rgba16Float` 3D textures (192×48×192 cells,
  ±96 m × ±24 m around `WorldFocus`). They are **toroidal**: cell `i` lives
  at texel `i mod dims`, and the repeat sampler does the wrapping, so the
  shader needs no origin.
- **What a texel holds.** `c0.rgb` is the constant term and `c0.a` the
  sun's visibility; `c1`–`c3` are the linear-basis L1 terms.
- **The read** (`toon_lighting.wesl`):
  - A **cubic B-spline**, as 8 hardware-trilinear taps per texture, 32 in all.
  - The L1 is read non-linearly: linear below `|c|/c0 = ½`, an isotropic
    + `q³` lobe above 1, blended in between, so it is never negative.
  - Then `+ sun_rgb × visibility × max(n·sun, 0)`.
- **Filled incrementally** (`src/game/world/probe_volume.rs`). When the
  window moves, only the newly exposed slab is enqueued; a chunk arriving
  enqueues its own cells; an environment change enqueues everything.
- **Budget.** 131 072 cells per frame. The render world `write_texture`s
  just those regions, and no CPU copy of the textures is kept.
- **Uncovered cells.** Cells no chunk covers get ambient with visibility 1.

### 6.3 The toon curve
`x = (lum / white)^(1/gamma)` is quantised into `bands`, then mapped back
through `^gamma` and spread over `[floor, 1]`. Band boundaries are
antialiased with `fwidth`, and hue is preserved. It is per world
(`EnvironmentData::toon_curve`) and edited in the inspector → Shading.
`ToonLook` (`gfx/toon_material.rs`) is the one path from the environment
into every toon and terrain uniform, re-applied when `TerrainWorld`
changes.

### 6.4 Real lights that exist
- **`BakedSun`.** A `DirectionalLight` with shadows off, illuminance
  = `sun_intensity × 10 000`.
- **`GlobalAmbientLight`.** Brightness = `ambient_intensity × 320`.

Both follow the environment (`src/game/world/mod.rs:116-163`) and reach
**only** the `StandardMaterial` meshes in §5.5. Placed point and spot lights
are bake inputs only: no Bevy light component is ever spawned for them.
There are no shadow maps and no clustered lights.

---

## 7. Geometry

### 7.1 Terrain chunks
- **Size.** 256×256 vertices at 0.25 m, so a chunk is 63.75 m on a side.
  `Mesh3d` per chunk, with a per-chunk material (§5.2), an `Aabb`, and a
  child heightfield collider (64×64).
- **LODs.** Four, built off-thread together. Strides are 1/2/4/8, giving
  65 536 / 16 384 / 4 096 / 1 024 surface vertices.
- **Skirts.** Each LOD has skirts round the edge, `stride × 1 m` deep,
  with zero normals that the vertex shader turns into "down".
- **Attributes.** POSITION, NORMAL (central differences), UV0 (all zero,
  only there to satisfy `VERTEX_UVS_A`), UV1 (planar 0–1 for the lightmap).
- **LOD switching** (`terrain/lod.rs`):
  - Swaps the `Mesh3d` handle by camera distance to the chunk's AABB centre,
    with thresholds of 500 / 1000 / 2500 m.
  - Coarsening happens at the threshold; refining happens below 0.9× it.
  - At most one step per check, and it only re-checks after 15 m of camera
    movement.
  - The 4th constant (4000 m) is never read.
  - In practice most chunks stay at LOD0 (§17).

### 7.2 Placed objects
- Each is a glTF scene (`WorldAssetRoot`), converted to toon (§5.4), with an
  optional static trimesh collider.
- Lights, empties and groups spawn nothing visible. Lights are drawn as
  editor gizmos (§10).

### 7.3 The player
- The `meshes/character/` glTF, kinematic, toon + probe-lit, with a 0.54 m
  blob shadow.
- Skinning goes through Bevy's default mesh vertex path (the toon material
  doesn't override the vertex shader).
- An `AnimationGraph` with Idle / Jump / Run, 200 ms blends
  (`src/game/animation.rs`).
- Hidden in first person, shown in third (`camera_follow`).

---

## 8. Post-processing

`Core3d`, in `Core3dSystems::PostProcess`, on the gameplay camera only:

1. **`tonemapping`, `Tonemapping::Linear`.** This is no tone curve, but
   Bevy's `tone_mapping` still starts with `max(color, 0)`. That clamp is
   needed: the 5-band quantiser can produce slightly negative channels.
   `Tonemapping::None` is a full passthrough since 0.20, so never use it here.
2. **Edge detection.** Relative second difference of prepass depth, plus the
   crease angle between prepass normals. Its own doc is
   [`EDGE_DETECTION.md`](EDGE_DETECTION.md). `.after(tonemapping).before(fxaa)`.
3. **`fxaa`.** Bevy defaults (High/High); it also smooths the outlines.
4. Then `upscaling` to the target.

The presentation `Camera2d` runs its own Linear tonemapping and FXAA over
the sprite and UI.

**Not present:** bloom, SSAO, SSR, TAA, SMAA, colour grading overrides,
environment maps, skybox, and shadow maps. `bevy_post_process` and
`tonemapping_luts` aren't compiled in (`Cargo.toml:7-37`).

---

## 9. Atmosphere

- **Sky dome** (§5.3).
- **`DistanceFog`** on the gameplay camera: exponential, density 0.0003,
  warm colour, with a sun-direction tint. **It only reaches
  `StandardMaterial` meshes.** None of our shaders (toon, terrain, sky) call
  Bevy's fog function, so the world itself is unfogged (§17).
- **`VolumetricFog`** (`step_count` 64). **A no-op, and free.**
  `extract_volumetric_fog` returns early and strips `VolumetricFog` off the
  render-world views when no `VolumetricLight` exists, and nothing spawns one
  or any `FogVolume`. Every downstream system then filters on a component
  that isn't there: the three `prepare_*` systems and the `volumetric_fog`
  pass all match nothing, no pipeline is ever specialised, and
  `prepare_view_depth_textures_for_volumetric_fog` doesn't even reach its
  depth-usage bump (it queries the *render* world, which extract already
  cleared). What remains is five empty system dispatches a frame plus two
  meshes and an embedded shader registered at startup. The comment at
  `camera.rs:120` about "cloud FogVolumes" is history (`git log -S FogVolume`).
  Cost of *using* it, and cheaper alternatives: §9.1.

---

### 9.1 What fog would cost

Read from the code, not measured on a GPU.

**Distance fog in our shaders — effectively free.** Bevy's `apply_fog` is
`length()`, an `exp()` and a `mix()`, plus a loop over directional lights
for the sun-scattering tint (we have one, and its shadow branch is skipped
because `BakedSun` has shadows off). Tens of ALU per fragment against a
toon shader that already does a B-spline probe read. The uniform is already
bound and `DISTANCE_FOG` is already defined, so applying it is a few lines
in `toon_lighting.wesl` and `terrain_toon.wesl` behind `@if(DISTANCE_FOG)`.

Two design questions come with it, neither of them about cost:
- **Before or after the toon curve.** After, fog is a smooth gradient laid
  over flat bands. Before (fog as a radiance term), the bands stay crisp
  and distance reads as desaturation. The second suits the look; the first
  is what every PBR engine does.
- **The sky dome is a 900 m sphere.** At density 0.0003 it would come out
  ~24% fog colour if fogged like geometry. Normally you exclude the sky and
  let `SkyColors` carry the horizon instead — which is what happens today.

Note the outline pass runs *after* tonemapping and reads depth, not colour,
so fogged-out geometry still gets full-strength outlines. That is what
`EdgeStyle`'s own distance fade exists for; the two fades want to be set to
roughly the same range.

**Bevy's volumetric fog — expensive, and against the grain here.** The
shader raymarches `step_count` steps per pixel (64 by default) and samples
a directional light's *cascaded shadow map* at every step. That shadow map
is the problem: `BakedSun` is `shadow_maps_enabled: false` because
everything is baked, so switching it on means adding real-time shadow
passes over the whole visible world — geometry re-rendered per cascade,
every frame — purely to feed the fog. The raymarch on top is ~130 M shadow
samples per frame at 1080p. On the web build, where the whole renderer
shares one thread with the game, that is not a trade worth making.

**Cheaper routes to the same look**, roughly in order of effort:
1. **Analytic height fog** in the toon and terrain shaders: `exp()` on the
   fragment's world Y and distance. Gives valley mist and aerial
   perspective for a handful of ALU. No new passes, no shadow maps.
2. **Radial god rays as a post-process**: a fullscreen pass that blurs
   outward from the sun's screen position, masked by the depth buffer we
   already render for the outlines. ~16 taps, one pass, no shadow maps.
   This is the classic cheap light-shaft effect and would sit next to the
   edge pass.
3. **Baked shafts.** We already bake sun visibility into the probe grid
   (`c0.a`). A volumetric term could be baked alongside it and read for
   free at runtime, at probe resolution (1 m) — coarse, but consistent with
   how everything else here is lit.

If volumetric fog is wanted, (2) is the one to build: it reuses the depth
prepass, costs one pass, and needs nothing from the lighting model.

## 10. Editor-only visuals

| Visual | Mechanism |
|--------|-----------|
| Transform gizmo | `EditorGizmoGroup`: line width 3, `depth_bias -1` (draws over geometry), `src/editor/gizmo.rs` |
| Selection / hover boxes, placement preview outline, brush ring, cafe grid | default `Gizmos` group |
| Light markers | `LightGizmos` group; spheres for point lights, line + circle for spot lights; F8 / View ▸ Light gizmos |
| Collision shapes | avian `PhysicsGizmos` (green, colliders only; the terrain heightfield is excluded with `DebugRender::none()`); Shift+P / View ▸ Collision shapes |
| Placement preview | the real glTF, translucent `StandardMaterial` (§5.5) |
| Brush disc | blended unlit disc on the terrain |
| Asset thumbnails | the offscreen rig on layer 7: renders until a readback shows the subject (CLAUDE.md, *Assets Browser*) |

Gizmo groups render on layer 0 through the gameplay camera, so they pass
through the edge pass and FXAA like everything else. They aren't in the
prepass, so they never get outlines.

---

## 11. Debug toggles

All of these keys work **only in the editor** (they're in
`EditorSet::InGame`, behind `keyboard_free`). The View menu has matching
checkboxes.

| Key | Effect | Applied by |
|-----|--------|-----------|
| Shift+L | baked lighting ↔ flat unlit (`ToonShading.unlit`, `TerrainShading.use_lightmap`) | `sync_lightmap_mode`, on `UserSettings` change |
| F2 | edge pass on/off | `sync_edge_detection_settings` |
| Shift+P | collision shapes | `sync_physics_debug` |
| F8 | light markers | `toggle_light_gizmos` (the gizmo config directly) |
| Shift+T | 4 px white crosshair (bevy_ui node) | `update_crosshair_display` |

---

## 12. The bake (offline)

[`LIGHTING.md`](LIGHTING.md) §2 has the detail. The flow, from
`src/game/world/baking/mod.rs` `run_bake`:

1. **Trigger.** Shift+B, the Bake button or File ▸ Bake send a
   `BakeMessage`. The region defaults to 3×3 chunks round `WorldFocus`.
2. **Shared inputs.** One `TriangleBvh` over the static occluders (placed
   objects and `RigidBody::Static` ancestry, minus `DynamicLit`). The sun
   direction comes from `BakedSun`, and every loaded placed light is collected.
3. **Terrain.** GPU compute (`lightmap_bake.wgsl`) when `GpuBakeSupported`.
   That is a runtime flag set at `RenderStartup` from compute support plus
   a workgroup-storage canary, so it works on native and on WebGPU alike.
   Otherwise the CPU path runs, synchronously in `Update`, using rayon.
4. **Probes.** One `AsyncComputeTaskPool` task per chunk.
5. **Objects.** Jobs are collected on the main thread, then an async task
   rasterises them into the UV2 atlas, shades (rayon), blurs, dilates and
   writes the EXR.
6. **Applying results.** These land in the live materials without a reload:
   - GPU terrain: `handle_bake_complete` (a readback observer).
   - Objects: `apply_object_bake`.
   - Probes: `apply_probe_bake` → `ProbeStore`.

| Output | Name | Version constant |
|--------|------|------------------|
| Terrain lightmap | `worlds/{id}/chunks/chunk_X_Y_lightmap.v12.exr` | `LIGHTMAP_FORMAT_VERSION` 12 |
| Object lightmap | `worlds/{id}/objects/obj_{id}_{k}_lightmap.v12.exr` | same |
| Probe grid | `worlds/{id}/chunks/chunk_X_Y_probes.v13.bin` | `PROBE_FORMAT_VERSION` 13 |

The version is part of the file name, so older bakes are skipped (with a
warning), never misread. On the web, the same strings are OPFS keys.

---

## 13. Streaming and when things appear

From `src/game/world/streaming.rs`:
- **Rings.** The load ring is Chebyshev radius 4 (about ±255 m) and the
  unload ring is 6. Loads go closest first, with up to 9 new loads and 2
  spawns per frame. Dirty and undo-pinned chunks never unload.
- **Native.**
  - A chunk's 4 LOD meshes, heightfield collider, decoded lightmap and
    decoded probe grid are all built in **one** async task. Terrain is
    therefore lit the frame it appears.
  - Its probes enter the volume over the next couple of frames (budget).
- **Web.**
  - Four OPFS reads per chunk: terrain, objects, lightmap and probes.
  - Meshing waits for **all four**, so on the web a chunk appears already
    lit, just later.
- **Objects.** A placed object's lightmaps arrive after its glTF loads and
  converts: native polls in `apply_object_lightmaps`, the web adds an async
  fetch step. So an object can show unlit (white radiance) for a frame or more.

---

## 14. Native vs web

There is no `cfg` anywhere in `src/game/gfx/` or in the shaders. The
differences are elsewhere:

| | Native | Web (WebGPU) |
|---|---|---|
| Build | — | `bevy/webgpu`; WebGL2 is not a target (it is a compile-time cfg, and one binary can't serve both) |
| GPU terrain bake | yes | yes (runtime flag, not a cfg) |
| Threads | rayon + task pools | **one**: rayon falls back to one thread and `AsyncComputeTaskPool` is `spawn_local` on the main thread, so CPU bakes, meshing, EXR/probe decode and probe fills are all main-thread long tasks |
| Bake/chunk I/O | `std::fs`, `AssetServer` | OPFS futures |
| Bindless / binding arrays | available, unused | unavailable |
| Bevy `IrradianceVolume` | available, unused | `cfg`'d out, which is why the probe volume is hand-rolled |
| Shader uniformity | naga skips the fragment uniformity check | **Tint enforces it**: `textureSample`, `textureSampleBias`, `dpdx`/`fwidth` must be in uniform control flow, or pipeline creation fails (black screen). Branch on a `let` read from the uniform. Check with `node tools/tint-check.mjs` (WGSL repro) |

---

## 15. Shader inventory

All are WESL except the compute bake.

| File | Stage | Used by | Tested by |
|------|-------|---------|-----------|
| `toon_lighting.wesl` | fragment (+ deferred) | `ToonMaterial` | `toon_curve_matches_the_shaders` (curve block) |
| `terrain.wesl` | vertex | `TerrainMaterial` | — |
| `terrain_toon.wesl` | fragment (+ deferred) | `TerrainMaterial` | `toon_curve_matches_the_shaders` |
| `sky_dome.wesl` | fragment | `SkyDomeMaterial` | — |
| `edge_detection_post.wesl` | fragment (Bevy fullscreen vertex) | `EdgeDetectionPlugin` | naga validation via import substitution; uniform size; plane/step geometry (`post_process.rs` tests) |
| `lightmap_bake.wgsl` | compute 16×16 | GPU terrain bake | naga validation, uniform size, f16 packing (`gpu.rs` tests) |

The material shaders import `bevy_pbr::render::*` modules, so they only
compile inside Bevy's resolver. A broken material shader shows up at
runtime, not in `cargo test`.

---

## 16. Measured costs

Web numbers are from 2026-11-03, cafe world, headless Chrome on an M-series
Mac ([`WEB_PROFILING.md`](WEB_PROFILING.md)).

| What | Cost |
|------|------|
| Probe volume, old full rebuild | 47–101 ms per rebuild on web (the walking stutter) |
| Probe volume, incremental fill | ≤ 4.7 ms max / 0.9 ms p95 per frame on web; native: full window 12.8 ms, one chunk 1.4 ms, one-cell step 0.15 ms |
| Probe volume memory | 56.6 MB GPU (4 × 14.2 MB); probe file about 2.9–3.4 MB per chunk; a full 81-chunk ring about 230 MB in `ProbeStore` |
| Terrain EXR | about 2.5 MB per chunk |
| Load-time hitches on web, no span | OPFS chunk futures (frames of 327–600 ms), trimesh collider build in `FixedMain` (106 ms), `apply_object_lightmaps` (71 ms), first `extract_render_asset<RenderMesh>` (62 ms), first egui pass (267 ms) |
| CPU terrain bake | sun 329 / AO 3347 / bounce 2829 ns per texel; a 3×3 region about 1.6 s single-threaded after the two-pass split (was about 61 s) |
| Object bake (cafe) | shading about 0.46 s of about 0.6 s |

`Time<Virtual>::set_max_delta(50 ms)` caps fixed-step catch-up after a hitch.

---

## 17. Things that don't do what they look like

Each item was read from the code; none was observed on screen.

1. **Volumetric fog is a no-op**, as is the "cloud FogVolumes" comment
   (§9). Either remove the component or add a `VolumetricLight` and
   `FogVolume`s.
2. **Distance fog doesn't touch the world.** Only `StandardMaterial` meshes
   get it (§9). If the horizon is meant to haze, the toon and terrain
   shaders would have to apply it.
3. ~~`outline.rs` is inert.~~ **Fixed 2026-11-04**: the module, its
   `GfxSet::Running` system, the Shift+O binding and `UserSettings
   .outline_visible` are deleted (engine-api ABI 13). The screen-space edge
   pass is the only outline.
4. **Shift+L doesn't reach new toon materials.** `sync_lightmap_mode` runs
   only when `UserSettings` changes, and `ToonShading::new` starts lit. An
   object that streams in or converts while the unlit view is on renders
   lit until the next toggle. Terrain is fine, because each chunk clones the
   base material, which the toggle *did* update.
5. **Terrain LOD > 0 is nearly unused.** Coarsening starts at 500 m, but the
   load ring's corner is about 400 m away and unloading starts past
   ~6.5 chunks (about 586 m diagonally). So only chunks lingering near the
   unload edge ever drop a level.
6. **Skirt normals in the prepass.** The mesh stores zero-length normals,
   which `terrain.wesl` fixes up, but the prepass uses Bevy's vertex shader,
   which normalises them. Possible NaN normals where skirts show through
   LOD cracks (`EDGE_DETECTION.md` §7).
7. **The blob shadow doesn't check its caster's visibility.** The player's
   disc stays under the camera in first person, where the player mesh is
   hidden.
8. **cafe-sim's thumbnail cameras use the default `TonyMcMapface`**, and
   `tonemapping_luts` isn't compiled in. Bevy logs an error when it
   specialises that method without the LUTs. Whether these LDR cameras hit
   that path is unconfirmed.
9. **The gameplay image is a fixed 1920×1080 with a nearest sampler**,
   stretched to the window. On a window of a different size or aspect
   ratio, that re-quantises pixels (deliberate; flagged for a revisit).
10. **The depth prepass is rendered for the outlines alone.** Nothing else
    reads it: the fog that would have is inert (item 1), and the toon
    shaders don't sample depth. Turning edge detection off with F2 skips
    the pass but still pays for the prepass.

---

## 18. Stale docs and comments

**Docs**
- **`RENDER_GRAPH.md`, `BEVY_RENDER_GRAPH_NODES.md`.** Written for Bevy
  0.17's render graph (`ViewNode`, `add_render_graph_edges`, `Node3d`
  labels), all removed in 0.19. They cite paths that moved and a lighting
  model (runtime shadows, clustered lights, Reinhard) that no longer exists.
  Each now carries a banner pointing here.
- **`LIGHTING.md`.**
  - §2.3 and §6.2 still describe the probe volume as a full rebuild.
  - §2 orders the bake objects-then-probes; the code runs probes first.
- **`DYNAMIC_LIGHTING_PLAN.md` §8–§9.** Gives the 2 m lattice, 128×24×128
  dims, the linear L1 read, the `0.5·ambient` scenery hit and the
  half-second rebuild cadence. All superseded.
- **`CLAUDE.md`, Rendering.** "Lightmaps baked via GPU compute (native) or
  CPU raycast (WASM)" contradicts its own platform table (GPU on WebGPU
  too), and `dilate_passes_for_atlas` no longer exists.

**Code comments**

| Where | Says | Actually |
|-------|------|----------|
| `world/mod.rs:119-121` | "the player character" uses `StandardMaterial` | the player is toon + probe-lit |
| `gfx/toon_material.rs:91` | terrain uses "one shared handle" | one per chunk |
| `toon_lighting.wesl:206` | "2 m cell" | 1 m |
| `engine-api toon_material.rs:256-258` | the CPU keeps copies of the volume (~170 MB) | images are render-world only, no CPU copy |
| `terrain_toon.wesl:194` | "terrain has no UV0" | it has an all-zero UV0 |
| `baking/gpu.rs:27` | `BakeComputeNode` render-graph node | a `RenderGraph`-schedule system |
| `baking/gpu.rs:359` | "only on non-wasm targets" | called unconditionally |
| `camera.rs:120` | volumetric fog for cloud `FogVolume`s | none exist (§17.1) |
