# Baked lighting — reference and review

Status: **current as of 2026-10-28.** Describes the system as implemented at
`LIGHTMAP_FORMAT_VERSION = 6`, before illuminance volumes / per-object dynamic
lighting. §7 is a review of the issues found while writing this, with the
numbers behind each one; §7.1, §7.2, §7.6 and §7.7 were fixed the same day
(shader-only and comment changes, no re-bake needed) and are kept as the
record of *why*.

Everything here is one pipeline: **bake-time** code turns lights into a texture
of outgoing radiance, and **runtime** code samples that texture, quantises it,
and multiplies it by base colour. There is no runtime light evaluation
anywhere — no shadow maps, no clustering, no light loops. That is what keeps
the WebGPU path cheap and what makes a `PointLight` cost zero frames.

---

## 1. Units, and the one equation

A lightmap texel is **outgoing radiance in lightmap units**: linear RGB where
`1.0` means "this surface shows its full base colour". Not lux, not lumens,
not candela. `EnvironmentData.sun_intensity` and `PlacedKind::PointLight
{ intensity }` are in the same units, so they can be added together directly.

Every baker computes the same thing (`BakeLighting`, `baking/lightmap.rs`):

```
radiance = sun_rgb * (ndotl * shadow + bounce_strength * bounce)   // direct sun + indirect
         + ambient_rgb * ao                                         // sky, shaped by occlusion
         + Σ_lights  color_i * ndotl_i * atten_i * spot_i * vis_i   // placed point/spot lights
```

| Term | Range | Who computes it | Notes |
|---|---|---|---|
| `shadow` | 0 or 1 | `raycast::cast_shadow_ray` | Heightmap DDA out to `ao_radius × 4` (80 m), then an exact BVH test. Binary per texel; the blur is what makes a penumbra |
| `ao` | [0, 1] | `raycast::compute_ao` | Mean over `ao_rays` cosine-weighted hemisphere rays of `hit_t / ao_radius` (1 if nothing is hit within `ao_radius`, 20 m), against terrain **and** the BVH — an occluder's weight falls off linearly with distance |
| `bounce` | [0, ∞) | `raycast::compute_indirect_bounce` | Terrain-only multi-bounce. Scenery absorbs bounce rays but never re-emits. **Terrain bake only — objects pass `0.0`** |
| `atten` | [0, 1] | `BakedLight::sample` | `(1 − d²/r²)²`. Smooth and bounded, deliberately *not* inverse-square |
| `spot` | [0, 1] | `BakedLight::sample` | `clamp(cosθ·scale + offset, 0, 1)`; a point light stores `scale = 0, offset = 1` so the same branchless maths yields exactly 1 |
| `vis` | 0 or 1 | `raycast::cast_shadow_ray_segment` | Segment ray that *stops at the light*, unlike the sun's fixed reach |

`ao` shapes only the ambient term. Direct sun is already attenuated by a real
shadow ray; darkening it again with AO would double-count the occlusion.

### Three implementations, one model

| Path | File | When |
|---|---|---|
| CPU terrain | `baking/terrain.rs` | WebGL2, or `use_gpu = false`, or no `TerrainChunkRegistry` |
| GPU terrain | `baking/gpu.rs` + `assets/shaders/lightmap_bake.wgsl` | Any adapter reporting compute, **including WebGPU** — gated on the runtime `gpu::GpuBakeSupported` flag, never a `cfg` |
| CPU objects | `baking/objects.rs` | Every platform. There is no GPU object path (`docs/GPU_OBJECT_BAKE_PLAN.md`) |

The three must agree texel for texel, which is why `BakedLight` is `#[repr(C)]`
and uploaded verbatim, the BVH is built once and shared, and the WGSL mirrors
`ChunkBakeGrid::world_xz` / `texel_seed` operation for operation. Two known
intentional divergences:

- `cpu_lowfreq_downsample` (default 4) is CPU-only. Set it to `1` for parity.
- The CPU marches have a `BakeHeightCache::above_terrain` early-out the GPU
  lacks. Exact, so it changes speed only.

---

## 2. Bake pass, end to end

`Shift+B` → `BakeMessage` → `run_bake` (`baking/mod.rs`):

1. **Resolve the sun** from the `BakedSun` entity's forward vector.
2. **Collect every loaded placed light** into `Vec<BakedLight>` — including
   ones outside the bake region, since a light just past the edge still shines
   in. (Lights in *unloaded* chunks are not collected; see §7.8.)
3. **Build one BVH** (`bvh::collect_occluder_triangles`) over every toon-shaded
   mesh whose ancestry includes a `PlacedObject` or a `RigidBody::Static`, so
   editor props and plugin scenery occlude but a plugin's moving mesh does not.
   Real triangles, not convex hulls — objects shadow themselves.
4. **Terrain**: GPU dispatch (via `GpuBakeTrigger`) or the CPU rayon path.
5. **Objects**: `collect_object_bake_jobs` on the main world (it mutates `Mesh`
   assets and may add materials), then `run_object_bake_jobs` on
   `AsyncComputeTaskPool`; `apply_object_bake` binds the results.

`BakeConfig::bake_radius` defaults to `Some(1)` — a 3×3 chunk block around
`WorldFocus`, plus the objects standing on it. `None` bakes all 81 loaded
chunks and takes minutes.

### Terrain texels

One `terrain_resolution²` (1024², = 16 texels/m over a 63.75 m chunk) lightmap
per chunk, baked on a **padded** grid (`ChunkBakeGrid`): `blur_radius` extra
texels on every side, blurred on the padded grid, then cropped. Without the
margin the box blur's clamped window biased each side of a seam toward its own
edge texel in opposite directions — a ~20% brightness step that the quantiser
turned into band edges jumping across chunk boundaries. Texel positions and
hemisphere seeds derive from the **world** texel index, so two chunks bake
bit-identical texels along a shared edge (`neighbouring_chunks_agree_along_
their_shared_edge`).

Two ray origins per texel: `surface + 0.5·Y` for the coarse DDA (steps are
~1.25 m, so a surface-level origin would re-hit the ground it left) and
`surface + normal·0.02` for the exact BVH test (so a crate resting on the
ground still shadows it).

### Object texels

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
- Post: `blur_cells` (confined to each triangle's cell and to covered texels —
  atlas neighbours are not surface neighbours), then `dilate` to flood the
  gutter so bilinear taps never reach a black texel.

### Storage

```
worlds/{id}/chunks/chunk_X_Y_lightmap.v7.exr
worlds/{id}/objects/obj_{id}_{k}_lightmap.v7.exr     # k = sub-mesh index
```

OpenEXR, f16 RGB, ZIP16. The format version is in the **file name**, so an
older bake is skipped rather than misread, and `warn_stale_lightmaps` says why.
Native writes a file; wasm hands bytes to OPFS fire-and-forget.

**`LightmapBuffer::into_image()` sets `ImageSampler::linear()` explicitly and
must keep doing so.** `lib.rs` sets `ImagePlugin::default_nearest()`, and a
point-sampled lightmap is piecewise-*constant*: band edges snap to texel
blocks, the shader's `fwidth` antialiasing measures a rate of change that is
zero inside a texel, the bake-side blur becomes a staircase of flat blocks, and
extra resolution only shrinks them. Guarded by
`baked_lightmaps_carry_an_explicit_linear_sampler`.

---

## 3. Runtime: sample, band, multiply

`toon_lighting.wesl` (objects) and `terrain_toon.wesl` (terrain) both do:

```wgsl
radiance = textureSample(lightmap, lightmap_sampler, in.uv_b).rgb;   // UV1 on the CPU side
out.color = base_color.rgb * toon_shade(radiance);
```

`toon_band` quantises into `TOON_BANDS = 5` levels, **antialiased with
`fwidth`**: a hard `floor()` threshold on a filtered texture reconstructs its
contour at texel granularity and aliases against the pixel grid no matter how
many texels you spend. The ramp sits in the top of each band and ends exactly
on the boundary, so the function stays continuous across it.

`toon_shade` bands **luminance**, spreads the banded value over
`[TOON_FLOOR, 1]` (`level = mix(0.18, 1.0, band)`), and rescales RGB by
`level / lum`, so coloured light keeps its hue instead of shifting at each band
edge. The bottom band is faded to neutral so a trace of tinted light can't
paint a dark corner; both ends of that fade have luminance `level`, so the
curve is monotone (§7.2).

Both functions are **duplicated verbatim** between the two shaders — there is
no shared import module for asset-path shaders. Keep them in step by hand.

### The transfer function, tabulated

This is the single most important table in this document. Lightmap luminance
in, displayed fraction of base colour out (flat region, `fwidth ≈ 0`):

| lightmap lum | 0 – 0.199 | 0.2 – 0.399 | 0.4 – 0.599 | 0.6 – 0.799 | 0.8 – 0.999 | ≥ 1.0 |
|---|---|---|---|---|---|---|
| **output** | 0.18 | 0.34 | 0.51 | 0.67 | 0.84 | 1.00 |
| chroma | neutral | tinted | tinted | tinted | tinted | tinted |
| *before §7.1* | *0.18* | *0.20* | *0.40* | *0.60* | *0.80* | *1.00* |

The second row is what the shaders do now; the last row is what they did until
2026-10-28, and §7.1 explains why the first two entries of it were a problem.

### Other runtime facts

- Camera uses `Tonemapping::Linear` — no tone curve, but it keeps the negative
  clamp. Bevy 0.20 made `None` a full passthrough that drops that clamp; never
  put it in front of the quantiser.
- Placed `PointLight` / `SpotLight` entities spawn **no bevy light component**
  at all. They are bake inputs, drawn in the editor by `draw_light_gizmos`.
  Consequence: baked light does not fall on dynamic objects.
- The one `DirectionalLight` (`BakedSun`, shadows off) and `GlobalAmbientLight`
  exist only for meshes still on `StandardMaterial` — the player character and
  editor thumbnails. `world/mod.rs` converts lightmap units to PBR units
  (`PBR_LUX_PER_UNIT = 10000`, `PBR_AMBIENT_BRIGHTNESS_PER_UNIT = 320`).
- `Shift+L` swaps to a flat unlit debug view (`use_lightmap = 0` → radiance 1).

---

## 4. Knobs

### Per world — `EnvironmentData` (inspector with nothing selected, saved in `world.meta` v5)

| Field | Default | Meaning |
|---|---|---|
| `sun_rotation` | aimed at origin from `(20, 120, −80)` | Only the rotation is baked; position drives the debug cone |
| `sun_color` / `sun_intensity` | white / `1.0` | Radiance an unshadowed surface facing the sun receives |
| `ambient_color` / `ambient_intensity` | `(0.9, 0.9, 1.0)` / `0.25` | Radiance a fully unoccluded surface gets from the sky, before AO |

### Per light — `PlacedKind::PointLight` / `SpotLight`

Defaults: point `(1.0, 0.85, 0.6)`, intensity `1.0`, range `14`; spot
`(1.0, 0.95, 0.85)`, intensity `1.5`, range `20`, cone `0.4 / 0.7` rad.
`shadows: bool` is retained for record stability and currently unused.

### Per bake — `BakeConfig` (quality/cost, not look)

`terrain_resolution 1024`, `mesh_texels_per_metre 8`, `mesh_resolution_max
2048`, `ao_rays 16`, `ao_radius 20.0`, `shadow_steps 64`, `ao_steps 32`,
`bounce_rays 8`, `bounce_strength 1.0`, `terrain_albedo 0.5`, `bounce_count 1`,
`cpu_lowfreq_downsample 4`, `blur_radius 4`, `bake_radius Some(1)`.

---

## 5. Invariants

Each of these has cost real debugging time. Breaking one is silent.

1. Lightmap `Image`s carry an explicit **linear** sampler (§2).
2. `toon_band` / `toon_shade` / `luminance` are identical in both shaders (§3).
3. Object texels use the **sub-mesh's** transform, not the placed root's (§2).
4. Lightmap UVs never overlap (§2).
5. Terrain chunks bake with a `blur_radius` margin and world-keyed texel
   positions/seeds, or seams reappear (§2).
6. `BakedLight` stays 64 bytes with every `vec3` 16-byte aligned, matching the
   WGSL (`baked_light_matches_the_wgsl_layout`).
7. `LIGHTMAP_FORMAT_VERSION` is bumped whenever the *meaning* of a texel
   changes. It lives in the file name.
8. Shaders branch on a `let` read straight from a uniform, never on a field of
   a `var` built from fragment inputs — WebGPU's uniformity analysis rejects
   `textureSample` under the latter and native silently does not.

---

## 6. Where the dynamic-lighting work plugs in

For the illuminance-volume / dynamic-object work, the contact surface is small:

- `BakeLighting::local_lights` is already the only place a placed light turns
  into radiance, and `BakedLight::sample` is already shared by all three
  bakers. A volume baker wants exactly `sample` minus the surface normal.
- Dynamic objects currently get sun + ambient from `GlobalAmbientLight` and
  `BakedSun` via `StandardMaterial`, so they are lit *nothing like* the static
  world. A probe sampled at the object's position and fed into a toon material
  would close that gap.
- **Read §7.1 first.** Any dynamic-lighting work inherits the transfer function
  in §3; fixing it is a prerequisite, not a follow-up.

---

## 7. Issues found in this review

Ordered by how much they affect what you see.

### 7.1 The two darkest bands are visually identical — lights tint shadows instead of lighting them

**Fixed 2026-10-28** — the bands now span `[TOON_FLOOR, 1]`; see the end of
this section. The analysis below describes the shaders as they were.

`toon_shade` in `assets/shaders/toon_lighting.wesl` and `terrain_toon.wesl`.

`TOON_FLOOR` is `0.18` and the first non-zero band value is `1/5 = 0.20`. So
anything that lifts a texel out of total darkness but not past lightmap
luminance `0.4` renders **11% brighter than pitch black** — about two
percentage points of displayed grey. Meanwhile `toon_shade` rescales RGB by
`band / lum`, so the *hue* of the result tracks the added light continuously.
Brightness is quantised into steps that start with a dead one; colour is not.
The result is that a light which doesn't reach band 2 changes the colour of a
shadow and nothing else.

Measured, with stock defaults and a default warm point light over a shaded
patch (`ao = 0.4`, no sun):

| light placement | light contributes | total lum | band | output |
|---|---|---|---|---|
| 2 m up, 1 m away | 0.734 | 0.844 | 4 | 0.80 |
| 2 m up, 3 m away | 0.418 | 0.528 | 2 | 0.40 |
| **2 m up, 6 m away** | **0.173** | **0.284** | **1** | **0.20** ← vs 0.18 unlit |
| 2 m up, 9 m away | 0.060 | 0.171 | 0 | 0.18 |

That third row is the reported symptom exactly. The light *is* reaching the
surface — the bake is correct, the shadow ray is clear, the falloff is sane.
The quantiser is throwing the answer away.

It is worse than it looks, because the scene's two most common states sit at
the two extremes of the table. With stock defaults on flat ground:

| situation | lightmap lum | band | output |
|---|---|---|---|
| open sunlit | 1.10 | clipped to 5 | 1.00 |
| sunlit, `ao 0.8` | 1.04 | clipped to 5 | 1.00 |
| tree shade, `ao 0.5` | 0.14 | 0 | 0.18 |
| deep shade, `ao 0.25` | 0.067 | 0 | 0.18 |
| contact, `ao 0.1` | 0.023 | 0 | 0.18 |

**Of five bands the default scene occupies two**, and they are the endpoints.
Bands 2–4 appear only inside the ~25 cm blurred penumbra. The sunlit value is
*above* 1.0, so it is clipped — adding any light to a sunlit surface is also
invisible, from the other end.

Two independent fixes, both small:

- **Spread the bands over `[TOON_FLOOR, 1]` instead of `[0, 1]`** —
  **done**: `level = mix(TOON_FLOOR, 1.0, band)` → levels `0.18, 0.34, 0.51,
  0.67, 0.84, 1.0`. Band 0 → 1 is now a 1.9× jump that reads instantly, and
  the floor stopped being a special case glued to the bottom band. The 6 m
  row in the table above now renders at 0.34 instead of 0.20.
- **Give the scene somewhere to sit** — *open, a level-authoring decision*.
  Either drop `sun_intensity` to ~0.8 so open sun lands mid-band-4 rather
  than clipped, or raise `ambient_intensity` so shade lands in band 1 rather
  than crushed at 0. With stock defaults the middle bands are still only
  reached by penumbrae and placed lights.

### 7.2 The `TOON_FLOOR` cross-fade is non-monotonic — a dark line along every shadow terminator

**Fixed 2026-10-28**, together with §7.1. The old code in the same two
functions was:

```wgsl
return mix(vec3(TOON_FLOOR), tinted, smoothstep(0.0, TOON_FLOOR, band));
```

`tinted` has luminance exactly `band`, so the mix interpolates between a
constant `0.18` and a value climbing `0 → 0.2`. In the middle it is **darker
than both ends**:

```
band:    0.00   0.04   0.08   0.105  0.14   0.18   0.20
output:  0.180  0.162  0.138  0.133  0.145  0.180  0.200
                              ^ minimum, 26% below the floor
```

The ramp is ~1 screen pixel wide by construction, so this draws a one-pixel
line, a quarter-stop dark, along every contour where lightmap luminance
crosses 0.2 — which, per §7.1, is *exactly* the boundary of every shadow in
the game. It also aliases, because the dip's depth depends on where in the
ramp a pixel lands.

This is very likely a large part of "contact shadows are a bit intense": the
shadow edge is outlined. The fix is to give both mix endpoints the same
luminance, so the hue fade cannot change brightness:

```wgsl
let level  = mix(TOON_FLOOR, 1.0, band);          // §7.1's remap; never below the floor
let tinted = radiance * (level / max(lum, 1e-4));  // luminance == level
return mix(vec3<f32>(level), tinted, smoothstep(0.0, TOON_FLOOR, band));
```

`level` is monotone in `band` and the mix only moves chroma, so the curve is
monotone by construction.

### 7.3 `ao_radius` is 20 m, so "ambient occlusion" is not contact-scale

**Fixed 2026-10-28** (lightmap format v7): each ray now contributes
`hit_t / ao_radius`, so an occluder's weight falls off linearly with distance.
A ceiling 0.5 m up still reads as near-total occlusion; one 10 m up on the
20 m radius reads as about half instead of zero. Both CPU and WGSL; the BVH
gained a closest-hit query (`nearest_hit` / `bvh_nearest`) for it. The
analysis below is of the binary count it replaced.

`BakeConfig::ao_radius = 20.0`, used unchanged for the hemisphere march and
the BVH test, with **no distance weighting**: a ray that hits a wall 19 m away
counts exactly as much as one hitting a chair leg 2 cm away.

AO is the *only* light in shadow (sun is 0, objects get no bounce — §7.4), so
a tree canopy knocks the whole ground under its 20 m footprint down to
`ambient × 0.2…0.4`, uniformly, not just where things touch. The term is
really doing sky-visibility duty, which 20 m is right for, while being asked
to also read as contact shadow, which it is not.

Options, cheapest first: weight each occluded ray by `1 − (t/radius)` so near
hits dominate (**taken**); or split into a short contact radius (~1 m) and
keep the long one for sky visibility — still available if the linear weight
turns out to leave large canopies too dark, since it only halves them.

### 7.4 Objects get no indirect bounce; terrain does

`baking/objects.rs:565` passes `bounce = 0.0` to `sun_and_ambient`, while
`baking/terrain.rs:226` passes `config.bounce_strength * bounce`.

A box standing on sunlit terrain has a shadowed side lit by `ambient × ao`
alone, where the terrain 10 cm away also receives `sun_rgb × bounce`. With
`terrain_albedo 0.5` and `bounce_strength 1.0` that is a real gap, and it
lands precisely at the object/terrain junction where the eye is looking. It
makes objects read darker than their surroundings in shade and compounds §7.3.

Objects already carry everything needed (`origin`, `normal`, the cache, the
BVH); `compute_indirect_bounce` would work as-is. The cost is the catch — it
was ~45% of the CPU terrain bake's rays, and the object bake is CPU on every
platform and single-threaded on wasm. A cheap middle ground is a constant
bounce approximation for objects rather than rays.

### 7.5 The object bake's terrain march starts at the surface, not 0.5 m up

`baking/objects.rs:542` uses one origin for both the DDA and the BVH test:

```rust
let origin = g.world_pos + g.face_normal * 0.02;
```

The comment reasons that "an object texel is not on the ground, so there is
nothing to step off" — true of the object's own geometry, but the ray still
marches over *terrain* with 1.25 m steps, and plenty of object texels sit at
or slightly below the bilinearly-sampled heightmap: anything resting on a
slope, half-buried props, a building floor. For those, `terrain_h > sample.y`
fires on the first step and both the sun *and* every local light are killed,
baking the texel to the floor value.

Terrain's bake lifts its DDA origin 0.5 m for exactly this reason, and the GPU
shader mirrors it. The object path should take the same two origins. Confidence
here is lower than 7.1–7.4 — I haven't reproduced it against a specific prop —
but the asymmetry with the other two paths is real and unexplained.

### 7.6 `light_shadow_steps` hardcodes its clamp in WGSL

**Fixed 2026-10-28.** The WGSL clamped to a `48u` literal while
`BakeConfig::light_shadow_steps` clamps to `max_light_shadow_steps`; they
agreed only because the default *is* 48, and changing the config silently
desynced the GPU terrain bake from the CPU terrain and object bakes. The value
now travels in `BakeUniforms::max_light_shadow_steps` (it took one of the pad
slots, so the layout is unchanged).

### 7.7 Stale comments claiming lights aren't baked yet

**Fixed 2026-10-28.** Placed lights have been baked since
`LIGHTMAP_FORMAT_VERSION` v5, but three module headers still promised them as
future work:

- `assets/shaders/toon_lighting.wesl:4` — "and — soon — point/spot lights"
- `baking/lightmap.rs:6` — "the sun and ambient today, point/spot lights next"
- `baking/mod.rs:4` — lists the terms and omits local lights entirely

Also `BakeStats::report` computed `rays_per_texel = 1 + ao_rays`, which no
longer matched its own comment (local lights each cost a segment ray), so the
logged ray estimate under-reported once a world had lights. It now takes the
light count.

### 7.8 Minor

- Lights in **unloaded** chunks are not baked, though `run_bake`'s comment says
  lights outside the bake region are included. True for loaded ones only — a
  light 5 chunks away that is in range of nothing doesn't matter, but a world
  baked with a small streaming ring differs from one baked with a large one.
- `collect_object_bake_jobs` tests `region.contains_point` on the placed
  **root's origin**, so a large object whose origin falls just outside the 3×3
  block is skipped entirely even though most of it is inside.
- `compute_ao` counts a ray with `cos_w <= 0` as occluded (it `continue`s
  without incrementing `unoccluded`). Unreachable with the current
  cosine-weighted generator, which always returns `local.y > 0`, but it is a
  trap for anyone who changes the distribution.
- HDR headroom above `1.0` is stored (f16 EXR) and then clipped by
  `toon_band`'s `clamp(value, 0, 1)`. Worth knowing before tuning intensities:
  the lightmap keeps the detail, the shader discards it.

---

## 8. Suggested order of work

1. ~~§7.2 (monotone floor) and §7.1 (band remap) together~~ — **done.** A
   handful of lines in two shaders, no re-bake. Judge the result in the
   viewport before touching anything else: the shadow outline should be gone,
   and a placed light in shade should now visibly brighten it, not just tint
   it.
2. ~~§7.7 comments, §7.6 uniform~~ — **done.**
3. §7.5 object ray origin — needs a repro case first.
4. ~~§7.3 AO weighting~~ — **done** (v7). §7.4 object bounce — a bake-model
   change that needs another `LIGHTMAP_FORMAT_VERSION` bump and a re-bake.
5. The open half of §7.1: pick default `sun_intensity` / `ambient_intensity`
   so a stock scene actually occupies the middle bands.
