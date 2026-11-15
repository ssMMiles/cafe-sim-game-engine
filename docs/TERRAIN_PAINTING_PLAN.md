# Terrain Painting — plan

Status: **Phases 0–3 built 2026-11-10** (§5, §8–§11); direction agreed the
same day (§6). The technical sections are deliberately thin and are expanded
phase by phase. §4.7 is the performance / WebGPU review.

## 1. Goal

Today every square metre of terrain looks the same: one flat base colour
with one stochastic stone pattern darkening it (`terrain_toon.wesl`). We
want a terrain **painting** tool in the editor that lets a world have a
palette of ground materials and decide, per spot, which one shows — by
hand with a brush, or automatically from rules (height, slope, noise) for
procedural-looking terrain — with blending between them that fits the toon
look rather than fighting it.

The deliverable is the whole feature, not just a brush:

- a **palette** of terrain layers per world, each a texture plus a few
  knobs, built from the textures already listed in the Assets panel;
- a neutral **default layer** (a plain grey triangle pattern) that new
  worlds and unpainted ground wear, so "unpainted" is visibly unpainted;
- **brushes** to paint, erase, fill, smooth, jitter and replace, sharing the
  radius / strength / falloff model the sculpt brush already has;
- **blend modes** so a transition can be a soft fade, a hard toon step, a
  height-driven "gravel fills the cracks" blend, or a dithered scatter;
- **rules** that paint automatically from height and slope (with noise), so
  a freshly sculpted mountain gets rock on its cliffs and snow on its top
  without a human colouring it in, and keeps doing so as it is sculpted;
- everything **undoable, saved with the world, streamed per chunk, and
  working on the web** build like every other terrain feature.

## 2. What we have to build on

- **Terrain is per-chunk heightmap tiles at 0.25 m** (`CHUNK_SIZE` 256
  vertices, 63.75 m chunks), streamed by proximity, saved as rkyv frames.
  A paint map at one sample per tile lines up with the heightmap exactly
  and is the natural resolution for a brush that already works in tiles.
- **The chunk file already has an unused `MaterialData` stub** (per-vertex
  material id + weight). It was never written or read, so we are free to
  design the real thing rather than inherit it; the enum can be removed in
  the same change.
- **Lightmaps and probe grids are side files per chunk**
  (`chunk_X_Y_lightmap.v12.exr`, `chunk_X_Y_probes.v13.bin`), each with its
  own format version, loaded alongside the `.terrain` file in the same
  async task. (Paint data, by contrast, is level data and goes inside the chunk file,
  see §4.1.)
- **Terrain already renders through our own scene pass** with our own
  material, shaders and bind group, so adding a texture array and a paint
  map is a change to code we own end to end — no bevy material system to
  negotiate with.
- **Lighting is entirely baked**, and the shader multiplies *base colour ×
  toon-curved lightmap*. Paint only changes the base colour term. Textures
  therefore need an albedo and nothing else (no normals, roughness or
  height for *lighting*; a height channel may still drive blending).
- **Stochastic no-tile sampling** (`sample_no_tile`) already hides tiling
  for the stone pattern and carries over per layer.
- **The brush, the stroke lifecycle and undo** exist: `BrushSettings`,
  `apply_terrain_brush` / `finalize_terrain_stroke`, a `Change` that stores
  before/after tiles with a 64 MB per-world cap, chunk pinning against
  unload, collider rebuild. Paint strokes slot into the same shape.
- **The Assets panel lists textures but nothing consumes them** ("inspect
  only"). The palette is the consumer it has been waiting for.
- **The bakers treat terrain as one scalar albedo** (`terrain_albedo`
  0.5) when computing bounce light and probe gathers. Once ground can be
  dark rock here and pale sand there, that should become a colour read
  from the paint map; it is the one place painting touches lighting.
- **Textures are 8-bit PNGs under `assets/textures/`**, discovered by the
  build-time manifest (the web build can't list directories), with the
  app-wide default sampler set to *nearest* for the pixel-art look.

## 3. How other engines do it (survey summary)

Full notes with sources are in the research below; the short version.
Two families exist:

**Weight-per-layer splat maps** — Unity Terrain, Unreal Landscape,
CryEngine, World Machine/Gaea exports. Each layer gets a weight channel
(RGBA textures hold four), the shader samples every layer and sums.
Smooth N-way blends; cost grows with the layer count (Unity: 4 per pass;
Unreal: weightmaps allocated per 64-m component, extra layers cost extra
fetches). Recurring pitfalls: weights that don't sum to one (washed-out or
black patches), per-chunk layer allocation complicating shaders, mixed
blend types corrupting normalisation.

**Material-ID maps** — Godot's Terrain3D, Ubisoft's Far Cry 5 and Ghost
Recon Wildlands. Each sample stores *which* material(s) are there rather
than a weight per material. Terrain3D packs base id + overlay id + blend
amount + per-sample UV rotation/scale + an "auto" flag into 32 bits;
cost is constant whatever the palette size (32 textures), weights can't
drift, and a palette of 256 is routine in AAA. The limit: at most two
materials meet at one sample, so three-way blends don't exist.

**Blend modes seen everywhere**: linear weight; **height blend** (a
per-texture height map sharpens the transition so pebbles sit *between*
cobbles — Unreal `LB_HeightBlend`, MicroSplat, Unity HDRP, Source's
luminance blend); **alpha overlay** on top of the weighted set (puddles,
Unreal `LB_AlphaBlend`); triplanar on steep slopes.

**Brushes seen everywhere**: paint (Shift erases), smooth, flatten-to-one
texture, noise, "spray" that nudges a blend, eyedropper, per-brush masks
by slope / height / existing layer (CryEngine layers carry altitude and
slope ranges; Unity's Terrain Tools add slope/height/noise filters),
import a greyscale mask as a layer, replace layer A with B across the
world (Unity's layer palette, Terrain3D's data API).

**Automation**: Terrain3D's *autoshader* is a per-sample flag — flagged
samples take their materials from slope and height in the shader and
keep painted data underneath until the flag is cleared. Unreal does it in
the material graph plus non-destructive *edit layers* with procedural
brushes. Generators (Gaea, World Machine) export slope / flow / wear
masks and the engine turns them into layers.

**Anti-tiling**: per-tile random rotation and shift (Terrain3D), stochastic
or "cluster" sampling (MicroSplat), distance resampling, macro noise,
decals rasterised into a virtual texture (Far Cry).

**Infrastructure pitfalls**: texture atlases bleed across tiles in the
mips and break hardware wrap — every ID-based system uses a *texture
array* (uniform size and format); chunk-edge seams unless neighbouring
samples are shared across the boundary; hard thresholds alias at sample
granularity; rotating or scaling UVs per sample breaks derivatives, so
those shaders sample with explicit gradients.

## 4. Recommendation for this engine

### 4.1 Store "what is here", not "how much of everything"

Use the **material-ID family**: each paint sample holds a *base* layer, an
*overlay* layer and a blend amount between them, plus a few flag bits
(auto-paint, and room for later per-sample variation). Reasons, in order:

1. **Constant shader cost at any palette size.** We ship for WebGPU on
   the main thread as well as native; a weight scheme makes every added
   layer a texture fetch for every terrain pixel, and the per-chunk layer
   allocation Unreal needs to contain that is exactly the kind of
   complexity we don't want in our own scene pass.
2. **Nothing can un-normalise.** No sum-to-one invariant, no washed-out
   patches, no special base layer.
3. **It suits toon.** A flat-shaded, hard-banded look wants mostly crisp
   transitions with a controlled soft or dithered edge, not five materials
   feathered together. Two materials per sample with a good blend is
   enough; three-way blends are the thing we give up, and the quantiser
   would hide them anyway.
4. **Rules fit naturally.** "Rock above 60°, snow above 120 m" writes two
   ids and a blend per sample, and the Terrain3D-style *auto* flag lets
   rules keep re-evaluating under sculpting while hand paint stays put.

**Resolution: one sample per heightmap tile (0.25 m)**, 256² per chunk.
At 32 bits a sample that is 256 KB raw per chunk; a full streaming ring is
about 20 MB in memory, and storage compresses well (most of a chunk is a
single layer). Finer than the tile grid would let blends curve between
vertices, but doubles memory for a texel finer than the brush ever is;
start at tile resolution and keep the sample size independent of
`CHUNK_SIZE` so it can change later. The blend amount is interpolated
between neighbouring samples in the shader, so transitions are already
smooth at sub-tile scale; the ids themselves are not interpolated.

**Storage: inside the `.terrain` chunk file** (format v3), replacing the
dead `MaterialData` stub with the paint grid, run-length or
single-value compressed since most of a chunk is one layer. Paint is
level data edited by the same brush, dirtied by the same flag and saved
by the same Shift+S as heights, so it belongs with them: one file, one
read, no way for heights and paint to get out of step. The web build in
particular already pays four OPFS round trips per chunk, and a fifth was
the main cost of the side-file idea first considered. v2 files read
through a `legacy` reader as "all default layer", so existing worlds load
unchanged. (Lightmaps and probes stay side files because they are *bake
outputs* with their own lifetime.) The palette itself (layer list and
settings) lives in `world.meta`, next to the environment, because it is
world-wide data the editor shows when nothing is selected. World export
must include the textures the palette references.

**On the GPU: one texture array** (all palette textures resized to one
size at load, see §6) plus one paint-map texture per chunk bound like the
lightmap is today — every chunk already has its own material clone for
its lightmap, so this adds a binding, not a draw. Mips work, hardware
repeat works, no atlas bleed.

### 4.2 The palette

A **terrain layer** is: a texture (from the Assets panel), a **tint**
(multiplied in, so one grey rock texture can be brown rock and grey rock
as two layers), **tiling scale** in metres, a **no-tile toggle** (the
stochastic sampler, on by default), a **blend profile** (see 4.3), an
optional **height channel** (the texture's alpha or luminance) for height
blends, and an **albedo colour for the bake** (derived from the texture's
average, overridable).

**Textures are patterns, colour comes from the tint** (decision 2 in §6).
A layer texture is a flat, greyscale-ish *pattern* — triangles, a cobble
outline, grass strokes — with no light or shadow drawn into it; all
shading comes from the lightmap through the toon curve, exactly as the
current stone pattern works (it darkens a base colour where the pattern
is dark). That keeps the look clean under the band quantiser and means
a handful of patterns × any tint covers a whole world's palette. A
photo-style texture with its own shadows still *works*, it just reads as
noise against the bands.

Layer 0 is always the **default layer**: a shipped grey-triangle texture,
neutral tint. Its texture can be swapped per world but it cannot be
deleted. The current stone look becomes a layer in the shipped palette so
existing worlds look the same the day this lands.

Palette operations: add from the Assets panel (click a texture → "Add to
palette", or drop onto the palette), reorder, rename, duplicate, delete
(samples using it fall back to the default layer or to a chosen
replacement), **replace everywhere** (swap layer A for B across loaded
chunks), **lock** (brushes refuse to overwrite a locked layer — CryEngine's
"mask by layer"), per-layer **eyedropper**. The palette panel shows the
thumbnail the Assets panel already renders for textures. Cap the palette
at 32 layers for now (5 bits per id); raising it later is a format bump,
not a redesign.

### 4.3 Blending

Blend is a per-sample amount between base and overlay; **how** that amount
turns into a visible transition is the overlay layer's **blend profile**:

- **Soft** — plain linear fade across the brush falloff. Reads painterly.
  **The default.**
- **Hard** — the fade is pinched into a step with a one-pixel antialiased
  edge (the same `fwidth` trick the toon bands use). Reads as a toon cel
  boundary.
- **Height** — the fade is modulated by the two textures' height channels,
  so the overlay appears first in the base's low spots (gravel between
  cobbles, snow settling in cracks). Contrast knob.
- **Scatter** — the fade becomes a dithered scatter of overlay patches
  using the same hash the no-tile sampler already has (grass tufts
  thinning into dirt). Patch-size knob.
- **Stepped** — the fade quantised into *n* bands, matching the lighting's
  banding (an optional variant of Hard).

Because the amount is stored and the profile is a layer setting, changing
a layer's profile restyles every transition into it at once, and the
brush never has to know which profile is active.

### 4.4 Brushes

**Paint is its own `EditorTool`** (Select / Place / Terrain / **Paint**,
key `4`, decision 6 in §6), not a mode of the Terrain tool: the Tool tab
then has room for the whole workflow — the palette strip at the top,
the sub-tool row, brush settings, masks, and the rules stack — without
crowding the sculpt panel. It shares the brush *machinery* with Terrain
(radius, strength, falloff, `[`/`]`, the disc preview, the stroke
lifecycle and undo shape), just not the panel. The Tool panel offers:

| Sub-tool | Does |
|---|---|
| **Paint** | Raises the blend towards the selected layer under the brush (sets it as overlay; once blend saturates the overlay becomes the base, so painting over fully is clean). Ctrl erases: lowers the blend back towards what was underneath. |
| **Fill** | Sets the brush area to exactly the selected layer, no blend. Alt+Fill fills the whole chunk; a palette command fills every loaded chunk. |
| **Smooth** | Blurs the blend amounts (softens a transition without changing which layers are there). |
| **Jitter** | Adds noise to the blend amounts for a ragged edge. |
| **Replace** | Under the brush only, swaps layer A for layer B (A chosen by eyedropper or dropdown). |
| **Auto** | Paints / erases the *auto* flag (section 4.5). |

Brush **masks**, togglable on any sub-tool: **slope range**, **height
range** (both with a soft width), **only over layer X**, **not over locked
layers**, and **noise** (a world-space noise threshold, so the brush dabs
texture organically). These are the CryEngine / Unity Terrain Tools
filters; they are what make hand-painting a mountainside take seconds.

The disc preview shows the selected layer's texture inside the disc so you
see what you are about to paint; an **eyedropper** (Alt+LMB, matching the
Alt-click convention elsewhere) picks the base layer under the cursor.
`Shift+Scroll` could cycle the selected layer. Strokes coalesce into one
undo transaction exactly like sculpt strokes.

### 4.5 Rules and auto-paint

A per-world ordered list of **rules**, edited in the inspector next to the
environment: each rule names a layer and a condition — height band
(min/max with a soft width), slope band, optional noise (scale,
threshold) to break up straight contour lines, optional "only where
currently layer X". Rules are evaluated top to bottom; the first two that
apply at a sample become base and overlay, the second's match strength the
blend. A live preview in the inspector shows the rule stack on a height ×
slope swatch.

Two ways to use them:

1. **Apply rules** as a command: on the brush area (the Auto sub-tool with
   "bake rules" on), the current chunk, or all loaded chunks. Writes plain
   paint samples — undoable, then editable by hand.
2. **The auto flag**: samples flagged *auto* ignore their stored ids and
   take the rules' result, recomputed when the chunk's heights change
   (after a sculpt stroke, on the chunk's own tiles) or when the rules are
   edited. New worlds start fully auto; painting by hand clears the flag
   under the brush, so hand work sits on top of the rules; the Auto brush
   re-flags an area to hand it back. This is the Terrain3D model and it is
   the part that makes "sculpt, and it colours itself" true continuously.
   **Rules are opt-in** (decision 4): a new world is flat default layer
   everywhere with an empty rule stack and no auto samples; "Apply rules"
   or the Auto brush is how an area is handed to them.

The rule result is written into the same paint map (the flag just says
*who owns* a sample), so the shader, the bake and the saved file never
need to know about rules. Imported greyscale masks from Gaea / World
Machine would be a later addition: a rule whose condition is "mask image
value", resampled over the world.

### 4.6 What painting touches elsewhere

- **Lighting bake.** Replace the scalar `terrain_albedo` with a colour
  read from the paint map (the layers' bake albedo, blended) in all four
  bakers, so pale sand bounces more light than dark rock and the probes
  agree with the lightmaps. A lighting-model change, so it bumps the
  lightmap and probe format versions. Painting marks lighting stale
  (status-bar warning), as sculpting does.
- **Edge detection** is unaffected: textures change colour, not depth or
  normals. Optional later: a per-layer "draws an outline against other
  layers" flag that writes a tiny normal tilt or an id edge into the
  normal target so material boundaries can be inked.
- **LOD** is unaffected: the paint map is sampled per pixel, not per
  vertex, so every LOD reads the same map.
- **Gameplay hooks.** Plugins will want "what surface am I standing on"
  (footstep sounds, speed). Expose a `surface_at(world_xz) → layer name`
  through engine-api later; the data makes it trivial.
- **Web.** Paint files go to OPFS with the other chunk files; textures
  come through the manifest as today. Brush application is CPU work on
  the main thread, per stroke frame, bounded by brush radius — same class
  as sculpting, which is fine there.
- **World export** includes the palette's textures (paint travels inside the chunk files it already ships).
- **Undo** records paint samples per stroke (before/after per chunk
  region) under the existing per-world cap; palette edits and rule edits
  are transactions too.
- **Debug views**: colour-by-layer-id and show-auto-flag, in the View
  menu next to the lightmap toggle.

### 4.7 Performance and WebGPU review

Reviewed 2026-11-10 against the code as it stands. The frame is GPU-bound
(`SCENE_PASS_BASELINE.md`), terrain is the biggest surface on screen, and
the web build runs everything CPU-side on one thread, so the questions
are: fragment cost per terrain pixel, memory, and what the brush does per
frame.

**Fragment cost — the common case must stay near today's.** Today a
terrain pixel does 4 stochastic taps of one texture. With painting, the
common case (both the pixel's neighbouring tiles agree on the layer pair)
is 1 paint-map read + 2 layers × 4 taps = **9 taps**, and when the blend
is 0 the overlay's 4 are skipped → **5**. That is the budget; the 4-tap
no-tile sampler can later drop to 3 taps (the simplex/hex variant) if it
needs clawing back. Measure with the Performance panel's A/B
(`EffectOverride`) before and after Phase 0, native and web.

**Tile corners — where the cost could explode, and how it doesn't.** A
pixel sits between four paint samples. Evaluating each corner's own
layer pair and bilinearly mixing the four colours (what Terrain3D does)
is 4 × 2 × 4 = 32 taps *everywhere*. Instead: read the four corners'
ids (cheap `textureLoad`s), and

- if all four agree on the pair — nearly every pixel — interpolate the
  blend amount bilinearly and sample the two layers once (9 taps);
- otherwise fall back to the per-corner evaluation, only on the one-tile
  band along a layer boundary.

The brush keeps that band rare: painting layer Y over X writes
`(base X, overlay Y, blend ramp)` on every tile it touches, so a
transition zone shares one pair end to end; only *Fill* and rule edges
produce adjacent tiles with different pairs, and those are hard edges by
intent. When a new overlay replaces an existing one mid-blend, the tile
snaps its base to whichever of its two layers was dominant (blend ≥ ½)
and restarts the blend at 0 — a small pop at the moment of painting, the
same as Terrain3D, accepted.

**WebGPU uniformity — this is the one hard rule.** Chrome's Tint rejects
implicit-derivative sampling (`textureSample`, `textureSampleBias`,
`dpdx`, `fwidth`) in non-uniform control flow, and every branch above is
non-uniform (it depends on the paint map). So the terrain shader
computes the UV gradients *once, at the top, in uniform flow* and
samples **every** layer tap with `textureSampleGrad` — legal anywhere.
(Today's shader side-steps the issue with a fixed mip level
`textureSampleLevel(…, 1.0)`, which is why sparse textures don't blur
to white at distance; the explicit-gradient form keeps real mip
selection and is what Terrain3D does for the same reason.) The Hard
profile's `fwidth` on the blend amount must likewise be taken before any
branch. `shader_check` + `tools/tint-check.mjs` on the dumped WGSL
catches a slip; `DUMP_WGSL` already exists for that.

**Texture array on WebGPU.** `texture_2d_array` is core WebGPU; the
guaranteed minimum is 256 layers and 8192² texels, so a 32-layer array
of 512² or 256² textures is nowhere near a limit, and it is one binding
+ one sampler against a cap of 16 per stage (the terrain material uses
5 today). No bindless needed — the array *is* the indirection. All
palette textures are resized to the array's size when the array is
built (CPU, once per palette change, not per frame); the array is
rebuilt only when a layer is added, removed or retextured.

**Paint map texture format.** One `Rgba8Unorm` per chunk, 4 bytes per
tile: base id, overlay id, blend, flags. Ids are read with `textureLoad`
(exact integers, `round(x·255)`), the blend channel with a filtered
sample from the same texture. One binding, no uint/filterable split.

**Memory.**

| Item | Size |
|---|---|
| Paint map per chunk (256² × 4 B) | 256 KB GPU + 256 KB CPU |
| Loaded ring (9 × 9 chunks) | ~21 MB each side |
| Texture array, 32 × 512², mips | ~43 MB |
| Texture array, 32 × 256², mips | ~11 MB |
| On disk per chunk | KB-range (RLE; a uniform chunk is a few bytes) |

For scale: the probe volume is 56 MB × 3 copies. Start the array at 256²
with a palette setting (decision 5); patterns at 0.25–3 m tiling don't
need more.

**Brush cost per frame.** A 50 m brush covers ~125 k tiles; the sculpt
brush already visits that many *and* remeshes the chunk and rebuilds its
collider. Paint only rewrites bytes and uploads the touched chunks'
textures (256 KB each; first version re-uploads the whole chunk image,
dirty-rect `write_texture` like the probe uploads if that ever shows up
in a trace). Strictly cheaper than sculpting, including on the web's
main thread. Rule re-evaluation after a sculpt stroke is 65 k samples of
arithmetic per dirty chunk, run at stroke end, not per frame.

**Chunk seams.** Chunks overlap by one tile row (`CHUNK_SIZE` 256 with
overlap 1), and the brush already writes a shared tile into every chunk
touching it (`chunks_touching_tile`). The paint grid is 256² per chunk
for the same reason and gets the same treatment, so the two chunks agree
on the seam row by construction. Sampling the paint map with clamp
addressing never reaches across a chunk.

**Undo.** 4 bytes per tile, same as a height; the existing 64 MB
per-world terrain cap covers both.

**Bake (Phase 4).** The GPU terrain bake binds the chunk's paint map +
a palette-albedo uniform (32 × vec4) and the CPU bakers read the same
grid; `BakeUniforms` grows, so `bake_uniforms_match_the_shader_struct`
has to move with it. Lightmap + probe format bump, as noted.

**What not to do.** No weight-per-layer fallback "for three-way blends"
(doubles the sampler count and brings back normalisation); no texture
atlas (mip bleed, no hardware repeat); no per-vertex paint (mesh LODs
would each need the attribute and the 4× downsampled LODs would smear it).

## 5. Phasing

| Phase | Delivers | Visible result |
|---|---|---|
| **0 — Foundation** ✅ | Palette data in `world.meta` + Paint tool skeleton with the palette strip in the Tool tab; default grey-triangle texture; texture array upload; paint grid in `.terrain` v3 + legacy v2 reader (all default); shader samples ids + blend with the Soft profile via `textureSampleGrad`; current stone look migrated to a layer. A/B measurement: see §8. | Worlds look the same as today, but the palette exists and a hand-edited file can show two textures. |
| **1 — Brush** ✅ | Paint mode with Paint / Fill / Erase, disc shows texture, eyedropper, stroke undo, save, web. | You can paint a world. |
| **2 — Blends & layers** ✅ | Blend profiles (Hard, Height, Scatter, Stepped), per-layer tint / tiling / no-tile, Smooth / Jitter / Replace sub-tools, replace-everywhere, lock, delete-with-fallback. | Transitions look intentional; palette is a real workflow. |
| **3 — Rules** ✅ | Rule stack UI with preview, Apply-rules command, auto flag + re-evaluation after sculpt, brush masks (slope / height / layer / noise), Auto sub-tool. | Sculpt a mountain and it textures itself; hand-paint on top. |
| **4 — Lighting & polish** ✅ | Paint-aware bounce albedo in all bakers, debug views, export, `surface_at` for plugins, docs (`GRAPHICS.md`, `LIGHTING.md`, `CLAUDE.md`). | The whole thing is coherent with the rest of the engine. |
| **Later** | Imported masks as rules; per-sample UV rotation / scale variation; triplanar on cliffs; material-boundary outlines; decals / splines (roads); holes (caves). | |

Phases 0–1 are the minimum useful product and the point at which the
default texture stops being the only texture. Each phase is its own
design pass before implementation.

## 6. Decisions (resolved 2026-11-10)

1. **ID + blend over weight-per-layer** — agreed; two-way blends are
   enough.
2. **Textures are flat patterns, colour is the layer tint** (§4.2). The
   alternative was textures with light and shadow painted into them
   (photo-sourced rock with dark crevices, baked ambient occlusion),
   which fights the band quantiser. The current "pattern darkens a tint"
   idiom is kept and generalised.
3. **Default blend profile: Soft.**
4. **Rules are opt-in.** New worlds are flat default-layer (grey
   triangles) everywhere, empty rule stack.
5. **Palette texture size: 256 × 256 pixels per layer** as the default
   array size, a per-world palette setting (256 / 512 / 1024). "Size"
   here is the pixel resolution every layer texture is resized to for
   the texture array; it has nothing to do with world extent — tiling
   scale in metres is per layer.
6. **Paint is its own editor tool** (key `4`); palette, brush, masks and
   rules all live in the Tool tab while it is active. Nothing goes in
   the inspector.

Still open, small: whether the eyedropper is Alt+LMB (matches the
group-select convention) or `I`; and the Paint tool's key hints for the
status bar. Both are Phase 1 details.

## 7. Research notes (survey detail and sources)

Per-engine notes collected 2026-11-09 from documentation and talks.
Caveat: gathered from search-result excerpts rather than full page reads;
the headline claims above were cross-checked across more than one source
each.

**Unity Terrain.** `TerrainLayer` assets (diffuse, normal, mask map,
tiling, remaps) referenced from `TerrainData`; weights in RGBA
"alphamaps", one channel per layer, normalised by the paint tool.
URP/Built-in: 4 layers per pass, unlimited additive passes; HDRP: 8 in one
pass, more are ignored. Height-based blend uses the mask map's blue
channel. Holes are a separate one-channel texture (aliased edges). The
Terrain Tools package adds eyedropper, layer palettes applied scene-wide,
in-place layer swap, and brush mask filters (slope, height, concavity,
noise). MicroSplat (third party) is the reference for anti-tiling:
texture-array layers, height blend instead of linear, distance
resampling, texture clusters, stochastic mode.

**Unreal Landscape.** Per-component weightmaps, one texel per vertex,
four layers per RGBA texture, allocated per component. Layer Info Objects
are weight-blended or not. `LandscapeLayerBlend` offers LB_WeightBlend,
LB_HeightBlend, LB_AlphaBlend; all-height-blend materials show black where
no layer wins. Slope/height automation in the material graph; Edit Layers
stack non-destructive height and weight edits with procedural Blueprint
brushes. Brushes: Circle / Alpha / Pattern / Component with Smooth /
Linear / Spherical / Tip falloffs; tools Paint, Smooth, Flatten, Noise,
Visibility (holes).

**Godot Terrain3D.** 32-bit control map per vertex: base id (5 bits),
overlay id (5), blend (8), UV rotation (4), UV scale (3), autoshader /
navigation / hole bits. Two textures per pixel, constant cost. Brushes:
Paint (base = overlay = chosen, blend 0), Spray (nudges blend, Ctrl
lowers), autoshader paint/erase, holes, per-pixel rotation/scale. Shader
blends the four corner samples; uses explicit gradients because per-pixel
rotation breaks derivatives. Maintainers note the two-material limit and
considered a virtual texture to lift it.

**Source / Source 2.** Source 1: 2-way and 4-way (CS:GO displacements)
vertex-alpha blends with a luminance-driven blend term. Source 2 VR
Standard: 2- or 3-layer blends with per-layer reveal masks and softness,
weights per vertex on subdivided meshes, plus a separate tint mask.

**CryEngine.** Weight layers per terrain painted with the Layer Painter;
each layer carries altitude and slope min/max, "mask by altitude and
slope" constrains the brush, "mask by material layer" protects a layer.

**Gaea / World Machine.** Export slope, height, flow, wear, deposition and
curvature masks; World Machine's Splat Converter normalises and packs four
per RGBA. Far Cry 5 derives texture ids from erosion masks.

**AAA id maps (Far Cry 5, Ghost Recon Wildlands).** Procedural
distribution writes a material-index map over a 256 palette, the 32
nearest materials cached in a texture array via an indirection table;
shading cached in an adaptive virtual texture with decals rasterised in;
triplanar on steep triangles.

Sources: Unity Terrain Layers manual, Terrain Tools Paint Texture /
Toolbox docs, MicroSplat 80.lv interview; Unreal Landscape Materials,
Paint Mode, Brushes, Edit Layers, Technical Guide; Terrain3D Control Map
Format, Texture Painting, Shader Design; Valve Developer Wiki VR Standard
and `$texture2_blendmode`; CryEngine 3/5 terrain editor docs; World
Machine "Export to Unity"; GDC 2017 Ghost Recon Wildlands terrain talk;
Far Cry 5 terrain GDC transcript; polycount and pixelwolf atlas-vs-array
notes.

## 8. Phase 0 as built (2026-11-10)

- **Data.** `terrain/paint.rs`: `PaintSample` (u32, little-endian =
  `Rgba8Unorm` texel: base, overlay, blend, flags), `PaintData`
  (`Uniform` | `Rle`) inside `.terrain` **v3** (`legacy::ChunkDataV2` reads
  v2 as all-default), `TerrainPalette` / `TerrainLayerData` in `world.meta`
  **v7** (`legacy::WorldMetadataV6` → `TerrainPalette::legacy_stone()`, so
  an existing world keeps its stone look; `create_world_metadata` gives new
  worlds the grey-triangle default). `PaintmapData` holds each loaded
  chunk's dense grid + paint-map `Image` handle; streaming inserts/removes
  it with the chunk, `save::extract_chunk_data` encodes it.
- **GPU.** `TerrainMaterial` bindings: 100/101 palette `2d_array`, 105
  paint map (`textureLoad`, no sampler), 106 `TerrainPaletteGpu` (32
  entries). `palette_sync::sync_palette` loads the palette's textures,
  builds the array (bilinear resample + box mips, layer-major, built with
  `Image::new_uninit` because `Image::new` asserts single-mip data) once
  they are all in, and writes the uniform into every terrain material on a
  palette change. `TerrainShading` lost `base_color`; the per-layer tint
  replaced it.
- **Shader.** `terrain_toon.wesl` as described in `GRAPHICS.md` §5.2.
  Validated by `shader_check` (naga) and `tools/tint-check.mjs` on both
  dumped variants (Tint, i.e. Chrome's uniformity analysis).
- **Editor.** `EditorTool::Paint` (`4`), `editor/paint_tool.rs`
  (`PaintToolState.selected`), `ui/paint.rs` (palette strip with tinted
  pattern cells, per-layer settings, "+" from catalog textures, texture
  size, brush radius/falloff), Assets ▸ texture footer "Add to terrain
  palette". Palette edits are `Change::Palette` with `MergeKey::Palette`
  (undo/redo through `LevelOps`, applied by writing the resource, which
  `sync_palette` watches). The brush disc and `[`/`]` run under Paint too.
- **Not in Phase 0, by design:** strokes (Phase 1); removing a layer
  (Phase 2 "delete with fallback" — samples referencing a removed id would
  silently show the clamped last layer, so the UI doesn't offer it yet);
  blend profiles other than Soft (shown disabled); the auto flag; bake
  albedo (field saved, unused).
- **Measurement still owed.** The before/after A/B of terrain fragment
  cost needs the app running (`PERFORMANCE.md`: Performance panel, A/B
  bench). Expected from the tap count: ~4 → ~5 pattern taps per pixel on a
  one-layer world (the overlay is skipped at blend 0), plus four
  `textureLoad`s.

## 9. Phase 1 as built (2026-11-10)

- **Brush rule** (`paint_tool::paint_step`, unit-tested): painting the
  *overlay* raises the blend by `flow × weight × dt`; at 255 the tile
  becomes `(layer, layer, 0)`. Painting the *base* lowers the blend
  (back to `(base, base, 0)` at 0); Ctrl+LMB does the same whatever layer
  is selected. A *third* layer snaps the dominant one (blend ≥ ½ → overlay,
  else base) to base and starts the new overlay from 0 — the §4.7 rule that
  keeps a transition on one layer pair. Hand-painted tiles clear the flags
  byte (no auto flag until Phase 3).
- **Sub-tools**: Paint (flow 0.2–8 /s, default 2) and Fill (the disc
  becomes exactly the layer, no falloff). "Fill loaded chunks" in the
  panel fills every loaded chunk as one transaction. Alt+LMB picks the
  dominant layer under the cursor.
- **Writes** go through `PaintmapData::write_tile`: grid + `Image` texel in
  every loaded chunk sharing the tile (edge rows are duplicated, like
  heights), and the chunks are marked dirty for Shift+S. Whole-image
  re-upload per touched chunk per frame; dirty-rect uploads if a trace
  ever shows it.
- **Undo**: `Change::Paint { chunks: Vec<PaintChunkChange { chunk, before,
  after }> }` with `PaintData` (RLE) snapshots taken on a chunk's first
  touch in a stroke and at stroke end. Undo/redo `replace_chunk`s (grid +
  image) and marks dirty. Counted against the 64 MB terrain cap by run
  count. Validation skips a transaction whose chunk isn't loaded, like
  sculpt strokes; pinned chunks follow `Change::chunks()`.
- **Disc**: dressed in the selected layer's pattern × tint at 55 % alpha
  (`style_paint_disc`); torn down on leaving the tool so Terrain gets its
  plain disc back.
- **Default layer** samples plainly — the stochastic no-tile offsets turned
  the triangle grid into overlapping shards. No-tile stays on for the
  legacy stone and is per layer.
- **Status bar**: per sub-tool rows (`controls.rs`: Paint layer / Erase /
  Fill / Pick layer / Brush size).
- **Not yet**: Smooth / Jitter / Replace sub-tools, brush masks, layer
  removal, blend profiles beyond Soft (Phase 2); rules and the auto flag
  (Phase 3); paint-aware bake albedo (Phase 4).

## 10. Phase 2 as built (2026-11-10)

- **Blend profiles** live on the *overlay* layer (`blend_profile` +
  `blend_param` 0..1, `world.meta` v8 with a v7 reader). In the shader
  (`blend_amount`): Soft = linear; Hard = step at ½, width from one pixel
  (param 1) to ½ (param 0); Height = `b + (h_over − h_base) × param`, so the
  overlay lands first where its pattern is lighter than the base's (the
  pattern value doubles as the height channel — no extra texture); Scatter =
  overlay where `b` exceeds smooth value noise with a 25 cm–4 m patch size;
  Stepped = 2–8 bands. Every edge is resolved over ~one pixel from a single
  `fwidth(blend)` taken at the top of `fragment`, in uniform flow, and passed
  down — the profile code itself never takes a derivative, which is what
  keeps Tint happy. Both WGSL variants pass `tint-check.mjs`.
- **Sub-tools**: Smooth (blend → 3×3 neighbourhood average over tiles with
  the same layer pair, from a pre-write snapshot), Jitter (blend → a fixed
  per-tile hash, so holding the button converges on one ragged edge rather
  than boiling), Replace (`replace_from` → selected under the disc;
  Alt+LMB picks *from* in this mode). All through `step_blend`, which
  collapses to one layer at either end like `paint_step`.
- **Lock**: a locked layer's tiles (dominant layer locked) are skipped by
  every sub-tool and by Replace-everywhere.
- **Remove with fallback** is a tombstone (`removed` + `fallback` on the
  slot), because chunk files not loaded at removal time still carry the id.
  `TerrainPalette::resolve` → `TerrainLayerGpu.misc.x` redirects it in the
  shader; `apply_paint_requests` rewrites loaded chunks to the fallback
  (`Change::Paint`) and retires the slot (`Change::Palette`) in one
  transaction. `TerrainPalette::add` reuses the first removed slot, so an
  unmigrated chunk would show the new layer there — stated in the Remove
  hover text. Layer 0 can't be removed; a fallback can't be a removed layer
  or the layer itself.
- **Replace everywhere** and **Fill loaded chunks** are `PaintRequest`s the
  panel queues and `apply_paint_requests` runs (the panel has no paint-map
  access); loaded chunks only.
- **Panel**: five-mode row (Fill shows "Fill loaded chunks", Replace shows
  From + "Replace everywhere"), live-layer strip with a 🔒 badge, per-layer
  Blend + its param slider (label per profile), Locked, "Remove layer…"
  menu listing fallbacks; Flow shown for Paint/Smooth/Jitter.
- **Not yet**: brush masks and rules (Phase 3), paint-aware bake albedo
  (Phase 4), per-layer pattern dark colour (constant `PATTERN_DARK`).

## 11. Phase 3 as built (2026-11-10)

- **Rules** (`terrain/rules.rs`, `WorldMetadata.rules`, `world.meta` v9
  with a v8 reader): an ordered `Vec<PaintRule { name, layer, enabled,
  height: Option<Band>, slope: Option<Band>, noise: Option<NoiseMask> }>`.
  `Band { min, max, soft }` is 1 inside and smoothsteps to 0 over `soft`
  outside each end; `NoiseMask { scale, threshold, soft }` is bilinear value
  noise over world xz. A rule's strength is the product of its conditions
  (a rule with none is a base coat). `rules_sample` applies the stack top
  to bottom like strokes — each rule covers the current dominant layer by
  its strength — and flags the result `PAINT_FLAG_AUTO`. Evaluation is
  Rust-side only (the shader never sees rules); slope comes from
  `HeightmapData::sample_normal`.
- **Auto flag lifecycle.** Rules write it; every hand sub-tool clears it on
  the tiles it writes; Auto + Ctrl clears it without changing the look
  ("release from the rules"). `reevaluate_auto` re-runs a chunk's flagged
  tiles after a sculpt stroke (`TerrainBrushHit::just_finalized`, drained by
  `reevaluate_auto_after_sculpt`), inside `LevelOps` when a terrain change
  is undone/redone, and 0.4 s after the rule stack last changed
  (`reevaluate_auto_on_rules_change`, so a slider drag runs once). That
  re-evaluation is *not* recorded in history — auto tiles are derived from
  heights + rules, and re-running after an undo converges on the same
  grid. "Apply rules" (disc / hovered chunk / all loaded) *is* recorded,
  because it changes which tiles are auto.
- **Brush masks** (`BrushMasks` on `PaintToolState`): height, slope, noise
  (the same `Band`/`NoiseMask` types) and only-over-layer; multiplied into
  every sub-tool's weight. Header shows ● while any is active.
- **Panel**: Rules section (enable, name, layer, ▲▼ reorder, ×, the three
  conditions, "+ Rule" taking the selected layer, Apply to chunk under
  cursor / to loaded chunks) with a 32×12 height-by-slope swatch in layer
  tints (noise left out so it reads); Brush masks section.
- **Format note**: the user has said format bumps are free in this phase of
  the project (`CLAUDE.md` Development Notes); the v8 reader was kept only
  because it was ten lines.
- **Not yet**: imported greyscale masks as rules (later), paint-aware bake
  albedo and debug views (Phase 4).

## 12. Phase 4 as built (2026-11-14)

- **Bounce albedo** lives in the bake's terrain snapshot, not in the ray
  code: `BakeHeightCache` (raycast.rs) gained a packed per-tile albedo grid
  on exactly its height grid. `extract(...)` fills it with the neutral 0.5
  grey the bakers always assumed (so every unit test is untouched);
  `.with_paint(&PaintmapData, &TerrainPalette)` overwrites it from the paint
  map — base and overlay layers' bake albedos through
  `TerrainPalette::resolved_bake_albedos()` (removed slots → fallback, the
  same redirect the shader applies), lerped by the tile's blend byte.
  Nearest tile, never bilinear: bounce light has no use for sub-tile paint.
  Tiles outside every loaded chunk read as the default sample (layer 0).
  `.with_uniform_albedo(v)` is the test knob the old `terrain_albedo: 0.0`
  configs became.
- **All four bakers** go through it. `BakeLighting::reemitted` takes a
  `Vec3` albedo and the bounce walk's throughput is a colour;
  `compute_indirect_bounce` lost its `terrain_albedo` parameter and reads
  `cache.albedo_at(hit)`; the probe gather's `terrain_radiance` likewise.
  The GPU bake uploads the packed grid verbatim as binding 6 and
  `terrain_albedo_at` in the WGSL decodes the same bytes, so CPU and GPU
  quantise identically (`the_albedo_grid_follows_the_paint` covers the CPU
  half end to end). `BakeConfig::terrain_albedo` is gone; `scenery_albedo`
  stays one grey scalar — the BVH carries triangles, not materials.
  Lighting-model change: lightmaps v13, probes v14 (old bakes are skipped
  with a warning, as always).
- **`bake_albedo` is derived, never authored** — a deliberate narrowing of
  §4.2's "derived from the texture's average, overridable". `sync_palette`
  measures each pattern's mean *dark factor* — `smoothstep(0.05, 0.45,
  1 − pattern)`, what `layer_eval` in the shader mixes the tint towards
  `PATTERN_DARK` by — when the array is rebuilt and writes
  `mix(tint, PATTERN_DARK, factor × pattern_strength)`
  (`paint::layer_bake_albedo`) into every layer's `bake_albedo` (through
  `bypass_change_detection`: derived bookkeeping, not an edit, not
  undoable). The field still lives in `world.meta`, so the bakers never
  need textures in memory and a future override is a UI change only. No
  format bump.
- **Staleness.** Paint strokes, paint requests (fills, replace-everywhere,
  apply-rules) *and sculpt strokes* now set `LightingState.stale` — the
  plan said "as sculpting does", but sculpting never actually did.
- **Debug views** (View menu): "Paint layer ids" draws flat golden-ratio
  hue colours per resolved dominant layer; "Paint auto tiles" tints
  rule-owned tiles magenta over the real shading. `TerrainDebugMode`
  (editor resource, not persisted) → `TerrainShading.debug` on every
  terrain material; both read the *nearest* paint tile so what is shown is
  exactly one stored sample.
- **World export ships no palette textures, on purpose.** §4.6 assumed
  user-supplied textures; as built, the palette can only reference
  manifest textures (`assets/textures/`, "Add to terrain palette" in the
  Assets tab), which ship with the engine on every platform. Paint itself
  travels inside the chunk files the zip already carries. If user texture
  import ever lands, revisit.
- **`surface_at` stays deferred** (§4.6 already said "later"): nothing
  consumes it yet, and a plugin-facing API wants its own design pass —
  `PaintmapData` is a host type, so the clean shape is an engine-api
  resource the host keeps current, not a casual addition to this phase.

