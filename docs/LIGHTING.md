# Lighting

Status: **current as of 2026-11-02**, at `LIGHTMAP_FORMAT_VERSION = 12` and
`WORLD_FORMAT_VERSION = 6`. This is the reference for the whole lighting
system: how a light becomes a number at bake time, how that number becomes a
pixel at runtime, which knob to turn for which problem, and what is still
wrong with it.

Read §3 first if you are trying to make the game look different. Almost
every "the lighting looks odd" question has turned out to be the transfer
function, not the bake.

---

## 0. The system in one paragraph

**Nothing is lit at runtime.** The sun, the sky, every placed point and spot
light, and the indirect bounce between them are all integrated at bake time
into a texture of outgoing radiance — one lightmap per terrain chunk and per
placed sub-mesh, plus a lattice of irradiance probes for anything that
moves. At runtime a fragment does three things: sample its radiance, quantise
it into five toon bands, multiply by base colour. There are no shadow maps,
no light loops, no clustering. That is what makes a `PointLight` cost zero
frames and what keeps the WebGPU path cheap.

Two halves, and the seam between them is where most bugs live:

| | static geometry | anything that moves |
|---|---|---|
| receives light from | a baked lightmap, through UV1 | the irradiance probe volume, by world position |
| casts shadows by | being in the bake BVH | a blob shadow disc, and not at all into the bake |
| resolution | 16 texels/m | one probe per metre, cubic B-spline |
| shares | **the same radiance units and the same toon curve** | ditto |

The two must agree about how bright things are, or a character reads as
pasted onto the floor. Most of §6's defect list is some version of them
disagreeing.

---

## 1. Units, and the one equation

A lightmap texel is **outgoing radiance in lightmap units**: linear RGB where
`1.0` means "this surface shows its full base colour". Not lux, not lumens,
not candela. `EnvironmentData::sun_intensity` and `PlacedKind::PointLight
{ intensity }` are in the same units, so they add directly.

Every baker computes the same thing (`BakeLighting`, `baking/lightmap.rs`):

```
radiance = sun_rgb * (ndotl * shadow)                     // direct sun
         + bounce                                          // indirect, a colour
         + ambient_rgb * ao                                // sky, shaped by occlusion
         + Σ_lights  color_i * ndotl_i * atten_i * spot_i * vis_i
```

| Term | Range | Who computes it | Notes |
|---|---|---|---|
| `shadow` | 0 or 1 | `raycast::cast_shadow_ray` | Heightmap DDA out to `ao_radius × 4` (80 m), then an exact BVH test. Binary per texel; the blur is what makes a penumbra |
| `ao` | [0, 1] | `raycast::compute_ao` | Mean of `hit_t / ao_radius` over `ao_rays` cosine-weighted hemisphere rays, against terrain **and** the BVH. Distance-weighted, so a chair leg 2 cm away is full occlusion and a canopy 10 m up on a 20 m radius is half |
| `bounce` | [0, ∞) RGB | `raycast::compute_indirect_bounce` | See §1.1. A colour, not a scalar times `sun_rgb`, because a lamp's colour is its own |
| `atten` | [0, 1] | `BakedLight::sample` | `(1 − d²/r²)^falloff`, `falloff` per light (default 2). Smooth and bounded, deliberately *not* inverse-square |
| `spot` | [0, 1] | `BakedLight::sample` | `clamp(cosθ·scale + offset, 0, 1)`; a point light stores `scale = 0, offset = 1`, so the same branchless maths yields exactly 1 |
| `vis` | 0 or 1 | `raycast::cast_shadow_ray_segment` | Segment ray that *stops at the light*, unlike the sun's fixed reach |

`ao` shapes only the ambient term. Direct sun is already attenuated by a real
shadow ray; darkening it again with AO would double-count the occlusion.

### 1.1 Re-emission — the one definition

When somebody's bounce ray, or a probe's environment ray, lands on a surface,
what that surface sends back is `BakeLighting::reemitted`:

```
reemitted = (sun_rgb * ndotl * sun_vis + Σ_lights ... + ambient_rgb) * albedo
```

One function, called by the lightmap bounce walk, by the probe environment
gather, and mirrored by hand in `compute_bounce` in the WGSL. Albedo is
`terrain_albedo` or `scenery_albedo` — one grey number each, because the BVH
carries triangles and not materials, so there is no colour bleed.

Two things to know about it:

- **The sky term is unoccluded.** The honest answer needs the hit surface's
  own AO, which is a second hemisphere of rays at every hit of every bounce
  ray of every texel. So a sealed room is brighter than it physically should
  be — in the same direction, and for the same reason, as the receiving
  texel's own `ambient × ao` already is.
- **It is shared deliberately.** Through v11 the probe gather included the
  sky here and the lightmap bounce did not, so a sky-lit wall filled a
  passing character but not the ground underneath them, and static scenery
  read consistently darker in shade than the dynamic objects standing on it.
  Guarded by `a_wall_bounces_sky_onto_a_surface_the_sun_never_reaches`.

### 1.2 Three implementations, one model

| Path | File | When |
|---|---|---|
| CPU terrain | `baking/terrain.rs` | WebGL2, or `use_gpu = false`, or no `TerrainChunkRegistry` |
| GPU terrain | `baking/gpu.rs` + `assets/shaders/lightmap_bake.wgsl` | Any adapter reporting compute, **including WebGPU** — gated on the runtime `gpu::GpuBakeSupported` flag, never a `cfg` |
| CPU objects | `baking/objects.rs` | Every platform. There is no GPU object path (`docs/GPU_OBJECT_BAKE_PLAN.md`) |
| CPU probes | `baking/probes.rs` | Every platform. Reuses `BakeLighting`, the BVH and the height cache unchanged |

They must agree texel for texel, which is why `BakedLight` is `#[repr(C)]`
and uploaded verbatim, the BVH is built once and shared, and the WGSL mirrors
`ChunkBakeGrid::world_xz` / `texel_seed` operation for operation. Known
intentional divergences are in §6.

---

## 2. The bake, end to end

`Shift+B` → `BakeMessage` → `run_bake` (`baking/mod.rs`):

1. **Resolve the sun** from the `BakedSun` entity's forward vector.
2. **Collect every loaded placed light** into `Vec<BakedLight>` — including
   ones outside the bake region, since a light just past the edge still
   shines in. (Lights in *unloaded* chunks are missed; §6.)
3. **Build one BVH** (`bvh::collect_occluder_triangles`) over every
   toon-shaded mesh whose ancestry includes a `PlacedObject` or a
   `RigidBody::Static` and no `DynamicLit`, so editor props and plugin
   scenery occlude but a plugin's moving mesh does not. Real triangles, not
   convex hulls — objects shadow themselves.
4. **Terrain**: GPU dispatch (via `GpuBakeTrigger`) or the CPU rayon path.
5. **Objects**: `collect_object_bake_jobs` on the main world (it mutates
   `Mesh` assets and may add materials), then `run_object_bake_jobs` on
   `AsyncComputeTaskPool`; `apply_object_bake` binds the results.
6. **Probes**: `baking/probes.rs` on the same lattice, written per chunk.

`BakeConfig::bake_radius` defaults to `Some(1)` — a 3×3 chunk block around
`WorldFocus`, plus the objects standing on it. `None` bakes all 81 loaded
chunks and takes minutes.

### 2.1 Terrain texels

One `terrain_resolution²` (1024², = 16 texels/m over a 63.75 m chunk)
lightmap per chunk, baked on a **padded** grid (`ChunkBakeGrid`):
`blur_radius` extra texels on every side, blurred on the padded grid, then
cropped. Without the margin the box blur's clamped window biased each side of
a seam toward its own edge texel in opposite directions — a ~20% brightness
step that the quantiser turned into band edges jumping across chunk
boundaries. Texel positions and hemisphere seeds derive from the **world**
texel index, so two chunks bake bit-identical texels along a shared edge
(`neighbouring_chunks_agree_along_their_shared_edge`).

**Two ray origins per texel**, a pattern every baker now follows:
`surface + 0.5·Y` for the coarse heightmap DDA (steps are ~1.25 m, so a
surface-level origin re-hits the ground it left) and `surface + normal·0.02`
for the exact BVH test (so a crate resting on the ground still shadows it).

### 2.2 Object texels

One lightmap per **toon-shaded sub-mesh entity**, not per object and not per
mesh asset — shared mesh assets share UVs, so a per-asset atlas would collide.

- `uv_unwrap` gives **every triangle its own atlas cell** with a gutter, and
  de-indexes the mesh so corners are unshared. Overlapping UVs are the one
  thing a lightmap cannot tolerate; the old triplanar projection bucketed by
  *unsigned* dominant axis, so a cube's top and bottom wore each other's light.
- Cell size is budgeted by **world-space** triangle size
  (`mesh_texels_per_metre`, floored by `mesh_texels_per_triangle`, capped by
  `mesh_resolution_max`) and computed **per placed instance**, so the cafe's
  20×12 m two-triangle floor gets a real atlas instead of 32 px.
- Texels are positioned with **each sub-mesh entity's own `GlobalTransform`**,
  never the placed root's — glTF nodes carry their own scale (the rock is 2.5×
  inside its file), and a texel placed by the root transform sits inside the
  real mesh where self-shadowing bakes it black.
- Each triangle is rasterised with a `blur_radius` **margin of extrapolated
  surface**, the atlas counterpart of `ChunkBakeGrid`: without it the kernel
  clamped at every triangle edge and biased it in opposite directions across
  a shared edge, which the quantiser drew as band contours jumping along
  triangle seams.
- Post: `blur_cells` (confined to each triangle's cell and to texels some
  triangle actually covered — atlas neighbours are not surface neighbours),
  then `dilate` to flood the gutter so bilinear taps never reach a black texel.

### 2.3 Probes

A 1 m world-aligned lattice (`PROBE_CELL_SIZE`; 2 m until 2026-11-02), 13 `f16` per cell: L1 in the
linear basis for sky, bounce and placed-light lobes, plus the sun's
**visibility** as a scalar. The sun is deliberately kept out of the L1 part —
a lobe's negative back cancelled the ground bounce — and is evaluated exactly
in the shader against the stored visibility, which is the only way four
coefficients can hold a hard terminator.

Bevy's own `IrradianceVolume` is `cfg!(not(target_arch = "wasm32"))`, i.e.
unusable on *any* wasm target, which is why this is hand-rolled. Streamed
like lightmaps into `world/probe_volume.rs`, which fills four fixed-size
`Rgba16Float` 3D textures (`PROBE_VOLUME_DIMS`, toroidal: cell `i` at texel
`i mod dims`, repeat sampler, so the shader needs no origin). Bevy rewrites a
same-shaped `Image` in place, so re-centring never touches a bind group.

**Probe L1 is not read back linearly.** Placed lights are stored as
peak-preserving `¼+¾cos` lobes, which a linear read makes negative over a
third of the sphere; clamped, that drew a hard planar black edge across every
probe-lit object near a lamp. `ProbeSh::eval_l1` / WESL `probe_channel` read
linearly below `a = |c|/c0 = ½`, as isotropic + a `q³` lobe above 1, and
smoothstep between (`PROBE_LOBE_BLEND`) — mean-preserving, never negative.
Plain deringing was measured and rejected (rms 0.42 → 0.48, i.e. worse; the
lobe read gives 0.22). The Rust and WESL copies must agree; the Rust one is
what the tests measure.

**The volume is read with a cubic B-spline, not trilinear filtering.**
Trilinear is C0 but not C1 — its gradient jumps at every probe plane. A smooth
gradient hides that, but the toon quantiser draws one hard contour per band
boundary, and each contour *kinks* where it crosses a plane. With a lamp's
pool only two or three cells across, those kinks are four or eight visible
corners: the lamp pool on the tops of the cafe's furniture read as a diamond
with a point along each axis. A cubic B-spline is C2, so there is no slope
break left to kink on. Four taps per axis would be 64 fetches; pairing
adjacent samples and placing one off-centre *hardware* tap per pair (Sigg &
Hadwiger) buys the whole 4×4×4 kernel for 8 trilinear fetches.
`bspline_axis` exists in Rust solely so `bspline_two_tap_form_is_exact` can
prove the pairing reproduces the direct weights — an off-by-half there
silently shifts all dynamic lighting by half a cell.

Measured on the cafe's own lamp at tabletop height (`dump_probe_plane`;
`kink%` is the rms second difference of the contour radius as a percentage of
its mean, so a circle scores ~0 and every polygon corner is a spike):

| read | centre lum | kink% b1 | b2 | b3 | axis/diag b1 |
|---|---|---|---|---|---|
| trilinear (through v11) | 0.99 | 1.34 | 4.04 | 1.64 | 0.896 |
| smoothstep-warped trilinear | 1.08 | 1.45 | 3.11 | 1.24 | 0.906 |
| Catmull-Rom | 1.07 | 1.17 | 4.94 | 1.49 | 0.900 |
| **cubic B-spline** | 0.73 | **0.09** | **1.10** | **0.03** | **0.989** |

Two alternatives were measured and rejected. Warping the sample coordinate by
a smoothstep (which makes the hardware's own lerp C1 for free) trades the
slope break for a *plateau* at every probe plane — a staircase instead of a
kink. Catmull-Rom interpolates, so it keeps the peak, but it is only C1 and
its negative lobes overshoot; it left the contours as kinked as trilinear.

The cost is real and worth knowing: a B-spline approximates rather than
interpolates, so it is a low-pass and the peak of a pool this narrow drops
about a quarter (measured on the 2 m lattice of the time). That is the honest price of smoothing a coarse lattice, and it
means lamp intensities may want raising. It also low-passes the stored sun
visibility, so a probe-lit object's sun terminator is softer still — it was
already vague by construction (§6.2), so this costs little.

### 2.4 Storage

```
worlds/{id}/chunks/chunk_X_Y_lightmap.v12.exr
worlds/{id}/chunks/chunk_X_Y_probes.v13.bin
worlds/{id}/objects/obj_{id}_{k}_lightmap.v12.exr     # k = sub-mesh index
```

OpenEXR f16 RGB ZIP16 for lightmaps, an rkyv frame for probes. The format
version is in the **file name**, so an older bake is skipped rather than
misread, and `warn_stale_lightmaps` says why. Probes have their own
`PROBE_FORMAT_VERSION` (13): they shared the lightmap version while every
change was a lighting-model change that invalidated both, and split off when
the lattice halved — a change to what a stored cell *index* means, which no
lightmap texel cares about. A lighting-model change still bumps both. Native
writes a file; wasm hands bytes to OPFS fire-and-forget.

**`LightmapBuffer::into_image()` sets `ImageSampler::linear()` explicitly and
must keep doing so.** `lib.rs` sets `ImagePlugin::default_nearest()` for the
game's pixel-art look, and a point-sampled lightmap is piecewise-*constant*:
band edges snap to texel blocks, the shader's `fwidth` antialiasing measures
a rate of change that is zero inside a texel, the bake-side blur becomes a
staircase of flat blocks, and extra resolution only shrinks them. Guarded by
`baked_lightmaps_carry_an_explicit_linear_sampler`.

---

## 3. The runtime: the toon curve

This is the entire runtime lighting model and the thing you almost certainly
want to change. `engine-api/src/toon_curve.rs` is the reference
implementation; `toon_lighting.wesl` (objects) and `terrain_toon.wesl`
(terrain) carry byte-identical copies of it between `// ─── TOON CURVE`
markers, held in step by `toon_curve_matches_the_shaders`.

```
radiance = lightmap sample, or probe reconstruction
out      = base_color * toon_shade(radiance, curve)
```

### 3.1 Four parameters, which used to be one

`ToonCurve { bands, floor, white, gamma }` lives in the material uniform and
is a **per-world setting** (`EnvironmentData::toon_curve`, inspector →
nothing selected → Shading). Changing it needs no re-bake.

| Field | Default | What it decides |
|---|---|---|
| `bands` | 5 | How many discrete shading levels |
| `floor` | 0.04 | The darkest displayed fraction of base colour. May be 0 |
| `white` | 1.0 | Exposure: the lightmap radiance that reads as fully lit |
| `gamma` | 2.2 | How the bands are spread between black and `white` |

The curve, in full:

```
x      = clamp(lum / white, 0, 1) ^ (1/gamma)        // quantise perceived brightness
band   = quantise(x) into `bands` steps              // antialiased, see 3.3
floor_x = floor ^ (1/gamma)
level  = (floor_x + (1 - floor_x) * band) ^ gamma    // evenly spaced in the same space
out    = mix(level, radiance * level/lum, fade)      // hue, see 3.4
```

`white` appears **only** on the input. It chooses which radiance reaches the
top band, not how bright the top band is: `level` runs from `floor` to
exactly `1.0` whatever the exposure.

### 3.2 Why `gamma`, and why the floor used to be stuck at 0.18

These two are the same story. Until 2026-11-02 the curve was a single
constant, `TOON_FLOOR = 0.18`, doing three unrelated jobs: the darkest level,
the bottom of the output range, *and* the width of a chroma fade measured in
band units. Lowering it broke two things at once.

**It made the shader return NaN.** The fade was
`smoothstep(0.0, TOON_FLOOR, band)`, which at `TOON_FLOOR = 0` is
`smoothstep(0, 0, band)` — undefined in WGSL when the edges are equal, and
lowered by naga to a divide. Not just dark pixels: every fragment on screen.
The fade is now one *band* wide (`smoothstep(0, 1/bands, band)`), which is
the dimensionally correct quantity and is never degenerate.

**And the bands had nowhere to put a dark value.** With `gamma = 1` the
boundaries sit at 0.2/0.4/0.6/0.8 of white. A sunlit surface receives about
`1.0` and a shaded one about `0.1`, so *everything in shade landed in band 0*
and four of the five bands covered the brightest octave alone. The floor
wasn't the colour of a shadow's darkest corner — it was the colour of every
shadow there was, which is why it had to be bright enough to read as a
surface. Drop it to zero and the entire shaded world goes black together.

Spacing the bands by perceived brightness fixes the cause. Boundaries move to
`white · (k/bands)^gamma`, so at the defaults:

| | band 0 | 1 | 2 | 3 | 4 | 5 |
|---|---|---|---|---|---|---|
| **displayed** | 0.040 | 0.123 | 0.257 | 0.446 | 0.693 | 1.000 |
| **reached at radiance** | 0 | 0.029 | 0.133 | 0.325 | 0.612 | 1.000 |
| *gamma 1, as shipped through v11* | *0.18* | *0.34* | *0.51* | *0.67* | *0.84* | *1.00* |
| *…reached at radiance* | *0* | *0.2* | *0.4* | *0.6* | *0.8* | *1.0* |

A stock scene (sun 1.0, ambient 0.25) now lands: contact shade → floor, tree
shade → 1, open shade → 2, bounce-lit → 3, grazing sun → 4, open sun → 5. All
six, instead of the two endpoints.

Measured over every baked texel in the cafe world
(`WORLD=worlds/cafe cargo test band_occupancy -- --ignored --nocapture`),
object lightmaps, 2.87 M texels:

| curve | band 0 | 1 | 2 | 3 | 4 | 5 |
|---|---|---|---|---|---|---|
| gamma 1 (as shipped) | **67%** | 21% | 8% | 4% | 0% | 0% |
| gamma 2.0 | 17% | 40% | 29% | 11% | 4% | 0% |
| gamma 2.2 (default) | 14% | 35% | 35% | 12% | 4% | 0% |
| gamma 2.6 | 10% | 20% | 49% | 14% | 6% | 0% |

### 3.3 The band antialiasing

`toon_band` is not `floor(x * bands) / bands`. A hard threshold on a
bilinearly filtered texture reconstructs its contour at *texel* granularity
and then aliases against the pixel grid — the jagged staircase along every
shadow edge. Raising lightmap resolution only shrinks the steps; there is
always exactly one texel of staircase, so it stays visible up close no matter
how many texels you spend.

Instead the boundary is ramped over `fwidth(x)`, the rate of change of the
banded coordinate per screen pixel, which makes every transition about one
pixel wide at any resolution, surface size or viewing distance. Flat regions
have `fwidth ≈ 0` and stay perfectly flat, so the banded look survives
intact. At grazing angles a whole band can fall inside one pixel, where the
clamp lets the bands dissolve into smooth shading instead of shimmering. The
ramp sits in the top of each band and ends exactly on the boundary, which
keeps the function continuous across it.

### 3.4 Hue, and why the curve is monotone

Banding each channel separately would shift hue at every boundary, so
`toon_shade` bands **luminance** and rescales RGB by `level / lum`. The
bottom band fades to neutral grey instead, so a trace of strongly tinted
light cannot paint a dark corner its own colour; the fade is one band wide,
so above the first boundary hue is preserved exactly.

Both ends of that fade have luminance exactly `level`, so it moves chroma and
never brightness, and the whole curve is monotone by construction
(`the_curve_is_monotone_in_radiance`). An earlier form mixed a *constant*
0.18 against a value climbing from 0 and dipped 26% below the floor in
between — a one-pixel dark line, a quarter-stop dark, along every contour
where the lightmap crossed the first boundary, which was the edge of every
shadow in the game.

### 3.5 Other runtime facts

- Camera uses `Tonemapping::Linear` — no tone curve, but it keeps the
  negative clamp. Bevy 0.20 made `None` a full passthrough that drops that
  clamp; never put it in front of the quantiser.
- `Shift+L` swaps to a flat unlit debug view (`unlit = 1` / `use_lightmap =
  0` → radiance 1 → full base colour). Orthogonal to the radiance source.
- An unbaked surface binds the shared 1×1 white `DefaultToonLightmap`, so it
  also renders at full base colour rather than black.
- Placed `PointLight` / `SpotLight` entities spawn **no bevy light
  component** at all. They are bake inputs, drawn in the editor by
  `draw_light_gizmos`.
- The one `DirectionalLight` (`BakedSun`, shadows off) and
  `GlobalAmbientLight` exist only for meshes still on `StandardMaterial` —
  editor thumbnails and previews. `world/mod.rs` converts lightmap units to
  PBR units (`PBR_LUX_PER_UNIT = 10000`,
  `PBR_AMBIENT_BRIGHTNESS_PER_UNIT = 320`).
- `ToonLook` (`gfx/toon_material.rs`) is the single path from the level's
  environment into every toon material, terrain and object alike. One
  resource for both: a curve that differed between them would draw a step
  along every place they meet.
- The sky dome (`world/sky.rs`) is derived from the same environment —
  ambient, sun colour and direction, and the curve's `white` — by
  `sync_sky_colors`, so it dims with the lights and re-exposes with the
  surfaces. It is the one displayed thing that does not pass through the
  toon curve's bands; it is already a stylised gradient. See §6.1 for the
  derivation.

---

## 4. Knobs: what to turn for what

### 4.1 Per world — `EnvironmentData`

Inspector with nothing selected; saved in `world.meta` v6.

| Field | Default | Meaning | Re-bake? |
|---|---|---|---|
| `sun_rotation` | aimed at origin from `(20, 120, −80)` | Only the rotation is baked; position drives the debug cone | yes |
| `sun_color` / `sun_intensity` | white / `1.0` | Radiance an unshadowed surface facing the sun receives | yes |
| `ambient_color` / `ambient_intensity` | `(0.9, 0.9, 1.0)` / `0.25` | Radiance a fully unoccluded surface gets from the sky, before AO | yes |
| `toon_curve` | `(5, 0.04, 1.0, 2.2)` | §3 | **no** |

### 4.2 Symptom → knob

| Symptom | Reach for |
|---|---|
| Shadows are a washed-out grey, not dark | `floor` down (0 is legal) |
| Everything is a flat mid-tone with one wandering contour | the scene is sitting inside one band: `white` down to its `Auto` value, or `gamma` up |
| Sunlit surfaces all pin at full base colour | `white` up |
| Only the brightest areas show any shading steps | `gamma` up |
| Too few / too many steps | `bands` |
| Shadow edges are jagged | not the curve — check the lightmap's sampler is linear, and the atlas resolution (`mesh_texels_per_metre`) |
| Interiors are dark and featureless | the bake, not the curve: place lights, and check `scenery_albedo` |
| A character looks pasted onto the floor | §6 — the two halves disagreeing |

**Do not fix an exposure problem with the light intensities.** It is the
trap the cafe world fell into: with the old linear ramp every sunlit surface
pinned at the top band, so the sun and ambient were dialled down ~5× until
they stopped clipping. That left `sun_radiance` luminance at 0.128 against
`ambient_radiance` 0.130 — a sun *dimmer than the sky*, so turning it off
barely changed the picture and its shadows had nothing to be darker than.
Measured, its terrain lightmap has p25 = p50 = p75 = 0.219: half the world is
one value. Intensities are physical inputs to the bake; `white` is the
exposure. Keep the sun the brightest thing in the world and grade with the
curve.

### 4.3 Per light — `PlacedKind::PointLight` / `SpotLight`

Defaults: point `(1.0, 0.85, 0.6)`, intensity `1.0`, range `14`; spot
`(1.0, 0.95, 0.85)`, intensity `1.5`, range `20`, cone `0.4 / 0.7` rad. Both
have `falloff` (default `2.0`, the curve every light had before it was a
knob): attenuation is `(1 − (d/r)²)^falloff`, so lower spreads the light out
towards its range and higher pulls it in around the lamp, with the endpoints
fixed. The half-brightness radius is `r·√(1 − 0.5^(1/falloff))` — 0.54 r at
2, 0.71 r at 1, 0.41 r at 4. The probe bake uses the same `sample`, so
dynamic objects see the same shape. `shadows: bool` is retained for record
stability and currently unused.

### 4.4 Per bake — `BakeConfig` (quality and cost, not look)

`terrain_resolution 1024`, `mesh_texels_per_metre 8`, `mesh_resolution_max
2048`, `ao_rays 16`, `ao_radius 20.0`, `shadow_steps 64`, `ao_steps 32`,
`bounce_rays 8`, `bounce_strength 1.0`, `terrain_albedo 0.5`,
`scenery_albedo 0.5`, `bounce_count 1`, `cpu_lowfreq_downsample 4`,
`blur_radius 4`, `bake_radius Some(1)`, `probe_rays 48`.

**The CPU terrain bake runs two passes.** AO and indirect bounce were ~95% of
its ray cost (measured: 329 ns sun / 3347 ns AO / 2829 ns bounce per texel)
and both vary over metres, so they are evaluated on a grid
`cpu_lowfreq_downsample` (default 4) times coarser and bilinearly
interpolated, while the sun and local-light shadows — the sharp terms the
bands key off — stay per-texel. `blur_radius: 4` already smears the result
over ~25 cm at 16 texels/m, so the lost detail was being discarded anyway.
Separately, every terrain march early-outs once a climbing ray clears
`BakeHeightCache::max_height` (exact, not an approximation). Together ~39×
(a 3×3 region: ~61 s → ~1.6 s single-threaded). Re-measure with
`cargo test --release terrain_bake_cost_breakdown -- --ignored --nocapture`.

**The CPU bake is single-threaded on WASM and blocks the tab**, which is why
the GPU path matters most there: rayon falls back to a one-thread pool on
`wasm32-unknown-unknown`, so every `into_par_iter` is serial, and bevy's wasm
task pool is `spawn_local` on the main thread. The **object** bake is CPU on
every platform, so it stays subject to this.

### 4.5 Diagnostics

| Command | Shows |
|---|---|
| `WORLD=worlds/cafe cargo test dump_world_meta -- --ignored --nocapture` | A world's saved sun/ambient/curve, with radiances |
| `WORLD=worlds/cafe cargo test band_occupancy -- --ignored --nocapture` | How a whole world's texels distribute over the ramp, for several candidate curves. `V=11` reads an older bake |
| `LIGHTMAP=<path> cargo test dump_lightmap -- --ignored --nocapture` | One lightmap as an ASCII map, plus percentiles. `BANDS=1`, `CROP=x,y,w` |
| `cargo test dump_probes -- --ignored --nocapture` | Probe coefficients and how hard the L1 fit is ringing |
| `AT=x,z Y=1.1 cargo test dump_probe_plane -- --ignored --nocapture` | The probe volume reconstructed over a horizontal plane from a world's baked files, as a band map plus per-contour anisotropy and kink — the tool for "the light pool on the furniture is the wrong shape" |
| `cargo test lamp_pool_shape -- --ignored --nocapture` | The same question against an analytic lamp, so there is a ground truth to compare the lattice against |

---

## 5. Invariants

Each of these has cost real debugging time. Breaking one is silent.

1. Lightmap `Image`s carry an explicit **linear** sampler (§2.4).
2. The toon curve block is byte-identical in both shaders and mirrors
   `engine-api`'s `ToonCurve` (§3). `toon_curve_matches_the_shaders`.
3. `ToonCurve::floor` may be 0; nothing may derive a `smoothstep` width, a
   divisor, or a range from it.
4. Object texels use the **sub-mesh's** transform, not the placed root's (§2.2).
5. Lightmap UVs never overlap (§2.2).
6. Terrain chunks bake with a `blur_radius` margin and world-keyed texel
   positions and seeds, or seams reappear (§2.1).
7. Every heightmap DDA starts from an origin lifted 0.5 up; only the BVH test
   starts at the surface (§2.1).
8. Re-emission goes through `BakeLighting::reemitted` (§1.1).
9. `BakedLight` stays 64 bytes with every `vec3` 16-byte aligned, matching the
   WGSL (`baked_light_matches_the_wgsl_layout`), and `BakeUniformsData` stays
   the same size as the shader's `BakeUniforms`
   (`bake_uniforms_match_the_shader_struct` — the alignment test alone will
   not catch a mismatch).
10. `LIGHTMAP_FORMAT_VERSION` is bumped whenever the *meaning* of a texel
    changes. It lives in the file name.
11. `ProbeSh::reconstruct_channel` and WESL `probe_channel` are the same
    function (§2.3).
12. Shaders branch on a `let` read straight from a uniform, never on a field
    of a `var` built from fragment inputs — WebGPU's uniformity analysis
    rejects `textureSample` under the latter and native silently does not.

A trap that has caught people twice: **`toon_band` splits its input at
0.2/0.4/0.6/0.8 of `white`**. The familiar 0.04/0.123/0.257/… are the
*remapped output*. They are different numbers in different spaces.

---

## 6. Known defects and open questions

Ordered by how much they affect what you see.

### 6.1 The sky is derived from the lights, but the derivation is a guess at a look

Resolved 2026-11-02: `sky_dome.wesl` used to be a hardcoded midday-blue
gradient that read nothing from the environment, so the dome — which the
brain takes as the light source — disagreed with the shading under any sun
or ambient but the default, and a night scene was impossible. The dome's
stops are now `SkyColors::from_environment` (`world/sky.rs`): zenith
`0.9 × ambient`, horizon `1.6 × ambient + 0.12 × sun` while the sun is up, a
hard-edged disc with a halo in the sun's colour, a dark floor below the
horizon, all divided by the toon curve's `white` so the sky and the ground
it lights re-expose together. The direction comes from the sphere's radial
normals, which also fixed the horizon tilting whenever the camera climbed
(it was keyed to world `y = 0`).

What is still a guess: the gains. They were chosen so that a sky of radiance
`A` displays near `A` — coherent with a white surface lit only by it, and
several times dimmer than sunlit white, as a real clear sky is — but nobody
has looked at them yet. They are five named constants at the top of
`sky.rs`. The sky's *hue* is the ambient's: a blue sky wants a bluer
`ambient_color` than the default `(0.9, 0.9, 1.0)`, which is barely tinted.

### 6.2 Static and dynamic still disagree in three ways

§1.1 closed the big one (re-emission). Remaining:

- **`bounce_strength` is applied to lightmaps and not to probes.**
  `terrain.rs`, `objects.rs` and the WGSL all scale `bounce` by it; the probe
  gather accumulates re-emitted radiance directly. Latent at the default 1.0;
  set it to 0.5 and dynamic objects keep full indirect while the static world
  halves.
- **Dynamic objects have no AO and no self-shadowing.** A probe is a point in
  open air; nothing accounts for the object's own body occluding it. A
  character's underside is lit as brightly as their shoulders, minus whatever
  the L1 directionality gives.
- **Probe shadows are coarse by design, not blurred** — one *binary*
  sun-visibility sample per cell, reconstructed with the B-spline kernel.
  A probe-lit object's terminator cannot resolve anything smaller than
  `PROBE_CELL_SIZE`. There is no blur knob; the only lever is the lattice
  size, at cubic cost. So "make baked shadows look like probe shadows" means
  throwing away detail the lightmap has and the probe volume never had.
- **A lamp's pool is near the lattice's resolution limit.** The cafe's lamp is
  2.5 m up and a tabletop is ~1.1 m, so the lit pool is a couple of metres
  across. The B-spline (§2.3) makes what the lattice can represent smooth and
  round, but it cannot add detail that was never sampled, and a lamp much
  closer to a surface than `PROBE_CELL_SIZE` will always read as a vague
  glow. The cell was halved to 1 m on 2026-11-02 for exactly this; the next
  halving costs another 8× in probes baked, bytes per chunk, `ProbeStore`
  memory and volume textures. The 1 m lattice already costs, measured on the cafe: 2.9 MB per
  chunk file (was 0.36), so a full 81-chunk streaming ring would hold
  ~230 MB of probes in `ProbeStore` (was ~29); the runtime volume was trimmed to ±96 m
  horizontally (`PROBE_VOLUME_DIMS` 192×48×192, 56.6 MB per copy, three
  copies) rather than paying 100 MB per copy for the old ±128 m; and a
  window re-centre rewrites all of it — 1.77 M cells, which is 17.8 ms
  single-threaded and so a dropped frame every ~32 m of movement, since it
  runs inline in `Update`. `ProbeVolume::rebuild` is parallel over the
  texture's z layers (sound because the mapping is toroidal: one layer is
  written by exactly one world z), which brings it to 3.0 ms; `rebuild_cost`
  measures both. Filling only the slab a move exposes would be the real
  fix. Anything probe-lit outside the window samples wrapped texels from
  the far side.

### 6.3 Bake coverage gaps

- **Lights in unloaded chunks are not baked**, though `run_bake`'s comment
  says lights outside the bake region are included — true for loaded ones
  only. A world baked with a small streaming ring differs from one baked with
  a large one.
- **`collect_object_bake_jobs` tests `region.contains_point` on the placed
  root's origin**, so a large object whose origin falls just outside the 3×3
  block is skipped entirely even though most of it is inside.
- **The GPU terrain path has no `cpu_lowfreq_downsample`** and no
  `above_terrain` early-out, so native GPU and CPU bakes of the same world
  differ very slightly in AO. Set `cpu_lowfreq_downsample: 1` for parity.

### 6.4 Smaller things

- HDR headroom above `white` is stored (f16 EXR) and then clipped by the
  quantiser. The lightmap keeps the detail; the shader discards it.
- `terrain_albedo` and `scenery_albedo` are single grey numbers, so indirect
  light carries no colour from the surface it bounced off — only from the
  lamp that lit it.
- There is no GPU object bake (`docs/GPU_OBJECT_BAKE_PLAN.md`). Object
  bounce in particular runs per texel with no coarse-grid shortcut, which
  that document argues is the cheap speed-up nobody has taken.
- The editor thumbnail rig renders `StandardMaterial` under a real
  `DirectionalLight`, so catalogue thumbnails are lit nothing like the game.
  Deliberate (a thumbnail wants even lighting), but worth knowing before
  comparing one to the viewport.

---

## 7. Version history

What changed in the baked data, and why. Bumping
`LIGHTMAP_FORMAT_VERSION` means old files are skipped, never misread.

| v | Change |
|---|---|
| 1 | 8-bit single-channel PNG, sun occlusion only |
| 2 | f16 RGB EXR radiance: sun + ambient×AO (+ terrain bounce) |
| 3 | Terrain texels on the true chunk footprint; object lightmaps per sub-mesh |
| 4 | Per-triangle atlas UVs replace the overlapping triplanar projection |
| 5 | Placed point and spot lights are baked in |
| 6 | Object texels use each sub-mesh's own transform |
| 7 | AO is distance-weighted — an occluder at `t` counts `1 − t/ao_radius`, so a 20 m radius can do sky-visibility duty without reading as contact shadow |
| 8 | Objects receive indirect bounce, as terrain always had |
| 9 | AO rotates its hemisphere sample set per texel. Sixteen *fixed* directions are a quadrature whose error is a smooth function of position, so the blur could not remove it: mean error 0.0159 → 0.0028 against a 512-ray reference |
| 10 | Scenery re-emits bounce, and probe gathering shades a scenery hit by its own normal instead of returning a flat `ambient × 0.5`. An enclosed interior previously received no indirect light at all, and an L1 fit of a constant has no directional term — which is why probe-lit furniture rendered completely flat |
| 11 | Placed lights re-emit, so a lamp lights the room off the surfaces it falls on. Indirect light became a colour. Object atlases gained a shaded margin around each triangle |
| 12 | Re-emission includes the sky, through one shared `BakeLighting::reemitted` — the probe gather already did and the lightmap bounce did not. The object bake lifts its heightmap DDA origin 0.5 up, as the other bakers do; object texels at ground level were losing the sun and every local light on the first step |

Runtime changes, which need no re-bake:

| When | Change |
|---|---|
| 2026-10-28 | Bands spread over `[floor, 1]`; the hue fade made monotone, removing a dark outline along every shadow edge |
| 2026-10-31 | Probe L1 read as isotropic + lobe above `a = 1` instead of linearly, removing the hard planar black edge across objects near a lamp (rms 0.42 → 0.22) |
| 2026-11-02 | `ToonCurve`: the ramp became a four-parameter per-world setting (`world.meta` v6, ABI 11), quantising perceived brightness rather than radiance. The floor is decoupled from the chroma-fade width and may now be 0 |
| 2026-11-02 | The probe volume is read with a cubic B-spline instead of trilinear filtering, so a lamp's pool on a dynamic object stops reading as a lattice-aligned diamond (§2.3). Shader only |
| 2026-11-02 | `PROBE_CELL_SIZE` 2 m → 1 m, with its own `PROBE_FORMAT_VERSION` (13) so lightmaps need no re-bake; the runtime window trimmed to ±96 m (§6.2) |
| 2026-11-02 | The sky dome is derived from the environment instead of hardcoded (§6.1) |
| 2026-11-02 | Lights gained a per-light `falloff` exponent (`.objects` v7, prefabs v2; re-bake to see it) |
