# Illuminance volumes + dynamic shadows — investigation

Status: **investigation, 2026-10-28.** No code written. Everything in §2 was
checked against the actual sources (bevy 0.20.0 / wgpu 30.0.1 in the cargo
registry, and this repo) rather than recalled; §4–§5 are options with a
recommendation, not decisions.

Background: `docs/LIGHTING.md` describes the baked system this extends.

---

## 1. What is actually broken today

Two separate gaps, often confused:

1. **Dynamic objects are lit by nothing in particular.** The player is on
   `StandardMaterial`, lit by the single `BakedSun` `DirectionalLight` and
   `GlobalAmbientLight` (`world/mod.rs`). Those are derived from
   `EnvironmentData` and are *global constants*. The player walks into the
   cafe, under a tree, or into the beam of a placed point light and looks
   exactly the same. Placed lights never touch them at all —
   `spawn_placed_entity` attaches no bevy light component by design.
2. **Dynamic objects cast no shadow.** `BakedSun` has
   `shadow_maps_enabled: false`, and the toon shaders don't sample a shadow
   map anyway — they sample a lightmap and band it. A character standing on
   open ground appears to float.

These need different solutions. (1) is a data problem — get baked lighting to
a moving point. (2) is a render-pass problem — get an occluder into the
shading of a surface that was baked without it.

---

## 2. Hard constraints (verified)

These rule out the obvious answers, so they come first.

| Fact | Where | Consequence |
|---|---|---|
| **`IRRADIANCE_VOLUMES_ARE_USABLE = cfg!(not(target_arch = "wasm32"))`** | `bevy_pbr-0.20.0/src/light_probe/irradiance_volume.rs:161` | Bevy's built-in `IrradianceVolume` is compiled out on **all** wasm — WebGPU included, not just WebGL2. The comment cites texture-binding overflow with deferred (bevy #11885). It is also `pub(crate)`, so we can't even query it. **Bevy's light probes are not an option for us.** |
| Bevy irradiance volumes only feed the PBR lighting path | `pbr_functions.wesl` | Even natively, our toon shaders bypass that path entirely. We'd get nothing without reimplementing the sampling ourselves. |
| **`WEBGL2` is a compile-time cfg**, and `webgpu` wins when both features are on | `bevy_pbr/src/render/mesh.rs:3620` (`#[cfg(all(feature="webgl", target_arch="wasm32", not(feature="webgpu")))]`), `bevy_render/src/settings.rs:73` | One wasm binary **cannot** serve both WebGL2 and WebGPU. WebGL2 support means a second artifact built with different features. |
| We currently build `features = ["web", "webgpu"]` | `Cargo.toml:9` | **There is no WebGL2 build today.** The `GpuBakeSupported` runtime flag is forward-looking defence, not evidence that a WebGL2 build works. |
| `VolumetricFog` "Requires using WebGPU on Wasm builds" | `bevy_light-0.20.0/src/volumetric.rs:22`, used at `gfx/camera.rs:120` | A WebGL2 build already needs runtime gating beyond the bake. Worth knowing before promising WebGL2. |
| WebGL2 downlevel: `max_storage_buffers_per_shader_stage: 0`, all `max_compute_* : 0`, `max_inter_stage_shader_components: 31` | `wgpu-types/src/limits.rs` `downlevel_webgl2_defaults()` | No storage buffers, no compute, few varyings. Anything we design must run on uniforms + textures only. |
| WebGL2 downlevel still allows `max_texture_dimension_3d: 256`, `max_uniform_buffer_binding_size: 16 KiB`, 16 sampled textures/stage | same, `downlevel_defaults()` | 3D textures and generous uniforms **are** available. A hand-rolled probe volume is viable where bevy's is not. |
| Bevy shadow maps do work on WebGL2, minus PCSS | `shadow_sampling.wesl:34,343` — WEBGL2 branches return early from the blocker search | Shadow mapping itself is not the blocker. |
| `shadows::fetch_directional_shadow(light_id, frag_position, surface_normal, view_z, frag_coord_xy)` is a plain importable function | `bevy_pbr/src/render/shadows.wesl:222` | Our toon shaders already import `mesh_view_bindings`, so they can sample the sun's shadow map with one call. |
| The camera **already has `DepthPrepass` + `NormalPrepass`** (for edge detection) | `gfx/camera.rs:98` | Screen-space techniques are pre-paid. Also satisfies bevy 0.20's new `ContactShadows` requirement. |
| **The lightmap's alpha channel is dead.** `to_rgba16_bytes` writes a constant `f16::ONE`; `to_exr_bytes` uses `SpecificChannels::rgb`; `from_exr_bytes` binds `(r, g, b, _a)` and drops it; both shaders read `.rgb` | `baking/lightmap.rs:436,634,661`; `toon_lighting.wesl:160`, `terrain_toon.wesl:178` | **One free f16 channel per texel, already allocated in VRAM.** This is the enabler for §5. |

---

## 3. The one structural advantage we have

The toon shaders do not evaluate lighting. They take **one RGB radiance
value** and band it:

```wgsl
out.color = base_color.rgb * toon_shade(radiance);
```

For a static surface that value comes from a lightmap texel. **Nothing
requires it to come from a texture.** For a dynamic object it can come from a
uniform, evaluated against the surface normal. That makes "illuminance
volumes" dramatically cheaper here than in a PBR engine: we need an
*irradiance* estimate, not a light list, and we need it per *object*, not per
pixel.

Everything downstream — banding, the floor, hue preservation, `fwidth`
antialiasing — is unchanged and automatically consistent with the baked world.

---

## 4. Part A — lighting dynamic objects

### Option A1 — Bevy `IrradianceVolume`
**Rejected.** Compiled out on all wasm (§2), PBR-path only, `pub(crate)` gate.
Dead end for this project, and worth writing down so nobody re-investigates.

### Option A2 — Per-object probe sampled on the CPU ← **recommended**

Bake a grid of irradiance probes; at runtime sample it on the CPU at each
tagged object's position and write the result into that object's material
uniform. The shader reconstructs radiance from the vertex normal.

- **Storage per probe:** L1 spherical harmonics, 4 coefficients × RGB = 12
  floats (48 B f32, 24 B f16). L1 gives a soft directional gradient — bright
  from the sky side, dark from the ground side — which is all the toon
  quantiser can show anyway. An ambient cube (6 × RGB) is the alternative and
  is slightly cheaper to evaluate; L1 interpolates better between probes.
- **Shader cost:** ~10 ALU, no texture fetch, no new varyings (world normal is
  already there). Comfortably inside the 31-component WebGL2 varying budget.
- **CPU cost:** one trilinear lookup per tagged object per frame.
- **Works identically** on native, WebGPU, WebGL2 and mobile. No new bind
  group, no 3D texture, no compute.

Bake cost is the pleasant surprise. A probe every 2 m over a 63.75 m chunk,
8 levels vertically, is 32×32×8 ≈ 8k probes. At ~32 rays each that is ~260k
rays per chunk against **~17M** for the chunk's 1024² lightmap — roughly 1.5%
on top of a bake we already run. At 4 m spacing it is a rounding error.

Reuses `BakeLighting`, `TriangleBvh`, `BakeHeightCache`, `cast_shadow_ray` and
`local_lights` as they stand.

**Known problems to design around:**
- *Probes inside geometry.* A probe under the terrain or inside a wall bakes
  black and then bleeds into neighbours. Needs a validity flag per probe and
  interpolation that renormalises over valid neighbours only. This is the
  single fiddliest part and is where most of the implementation risk sits.
- *Per-object material churn.* Bevy materials are assets, so each tagged
  object needs its own material instance, and each sub-mesh of a glTF scene is
  its own material. Updating them every frame re-prepares their bind groups.
  There is precedent for the cloning (`unique_material` in
  `object_lightmaps.rs` already does exactly this for the object bake), but
  the per-frame write is unmeasured. Mitigation if it bites: update only when
  the object has moved more than some fraction of the probe spacing.
- *Large dynamic objects* sample one probe at their origin and so are lit
  uniformly. Fine for a character; wrong for a moving platform.

### Option A3 — Probe volume as a 3D texture, sampled per pixel

Same bake, but upload the grid as a 3D texture and sample it in the fragment
shader. 256³ fits even WebGL2 downlevel limits, so it is *possible*
everywhere.

Costs a texture binding and a 3D sample per pixel, and needs the volume to
follow the streaming ring. **Only worth it if there are hundreds of dynamic
objects, or objects large enough that per-object uniform lighting looks
wrong.** Start with A2; A3 is a drop-in replacement for the sampling step
later, with the same baked data.

### Option A4 — Sample the existing lightmaps under the object

Raycast down, read the terrain lightmap texel, use that as the object's
radiance. Nearly free and needs no new bake data.

It is, however, wrong in exactly the interesting cases: it has no directional
information, it cannot light a character from a point light at head height,
and it reads the *floor's* radiance including the floor's own albedo
assumptions. Worth knowing as a half-day fallback if the probe bake slips, not
as the design.

---

## 5. Part B — dynamic shadows

### The compositing problem, and the free channel

A lightmap texel is `sun + ambient×ao + bounce + Σ lights`, already summed. A
dynamic shadow must remove **only the sun term**, and only where the sun was
actually reaching. You cannot recover that from the summed value.

Multiplying the whole radiance by the shadow factor is the tempting shortcut
and it is visibly wrong: a character's shadow falling across ground that is
*already* in a tree's shadow darkens it a second time, below the ambient floor
the rest of the shade sits at.

**The fix is free.** The bakers already compute the scalar `ndotl * shadow`
(`terrain.rs`, `objects.rs`, `lightmap_bake.wgsl` all build
`sun_rgb * (ndotl * shadow + bounce)`). Store that scalar in the lightmap's
unused alpha channel. Then at runtime, with `sun_rgb` as a uniform:

```wgsl
let s = textureSample(lightmap_texture, lightmap_sampler, in.uv_b);
let radiance = s.rgb - sun_rgb * s.a * (1.0 - dynamic_shadow);
```

Exact: when `dynamic_shadow = 1` nothing changes; when it is 0 it removes
precisely the sun contribution the baker added, leaving ambient, bounce and
local lights intact. Costs **zero extra bytes on the GPU** (the channel is
already allocated in `Rgba16Float`), one extra float in the EXR, and a
`LIGHTMAP_FORMAT_VERSION` bump. `LightmapBuffer::texels` would move from
`Vec3` to `Vec4` (or gain a parallel channel) — a contained refactor touching
the blur, dilate and EXR paths.

This is worth doing **regardless of which shadow option is chosen below**, and
it is the thing I'd nail down first.

### Option S1 — Blob shadow
A dark ellipse projected under each tagged object, conformed to the heightmap.
Effectively free, works everywhere, and honestly suits a 5-band toon look.
Gives no silhouette — a character with outstretched arms casts an oval.
**Good mobile/WebGL2 fallback, and a good first milestone** since it needs
none of the §5 compositing work.

### Option S2 — Projected (flattened) geometry
Squash the caster's mesh onto the ground plane and draw it dark. Cheap, keeps
the silhouette, classic for this art style. Breaks on uneven terrain and needs
stencil work to avoid double-blending overlapping triangles. Our terrain is
rarely flat, so this is weaker here than it looks.

### Option S3 — Dedicated dynamic-only sun shadow map ← **recommended**
Turn shadows on for `BakedSun`, mark **everything static** `NotShadowCaster`
(terrain chunks, placed objects, sky), and leave only tagged dynamic casters
in the pass. Configure one cascade, a short `maximum_distance`, and a small
`DirectionalLightShadowMap { size }` — 512–1024, not the 2048 default. The
toon shaders call `shadows::fetch_directional_shadow` once and feed the result
into the compositing expression above.

- The shadow pass contains a handful of skinned meshes, not the world.
- One extra texture sample per toon fragment.
- Works on WebGL2 (PCSS is skipped there, which we don't want anyway).
- Uses bevy's cascade/bias machinery instead of hand-rolling matrices.

The discipline cost is real: every static mesh spawn site must set
`NotShadowCaster`, and a missed one silently drags the whole terrain into the
shadow pass. Spawn sites are few and centralised (`terrain/lod.rs`,
`world/objects.rs`, `sky.rs`, `gltf_loading.rs`) and `NotShadowCaster` is
already imported in three of them, so this is tractable — but it wants a test
or a debug assertion that counts shadow casters.

### Option S4 — Bevy's full cascaded shadow maps
Leave everything casting, 4 cascades at 2048². Redundant with the bake (we'd
be shadow-mapping geometry whose shadows are already in the lightmap, at much
lower quality), and the wrong order of magnitude for mobile. **Rejected.**

### Option S5 — Screen-space contact shadows
Bevy 0.20 ships `ContactShadows` (`bevy_pbr/src/contact_shadows.rs`,
screen-space raymarch, requires a depth prepass — which we already have). But
it only produces small-scale contact darkening, not a cast shadow, and it is
wired into the PBR lighting path we bypass. At best a later complement to S3,
not a substitute.

---

## 6. Cost sketch

Per frame, per tagged dynamic object, assuming A2 + S3:

| Work | Native | WebGPU | WebGL2 / mobile |
|---|---|---|---|
| Probe trilinear lookup (CPU) | negligible | negligible | negligible |
| Material uniform write | 1 per sub-mesh | same | same |
| Shadow pass draw | 1 draw, small depth target | same | same |
| Toon fragment extra | 1 texture sample + ~10 ALU | same | same |

The parts that genuinely need measuring before committing: the per-frame
material bind-group churn (A2), and the shadow pass cost on a real phone.
Neither is estimable from reading code.

---

## 7. Suggested phasing

1. **Sun term into lightmap alpha** (format bump, re-bake). Independent of
   everything else, unblocks every shadow option, and is verifiable on its own
   — with `dynamic_shadow = 1` hardcoded, output must be bit-identical.
2. **`DynamicLit` / `DynamicShadowCaster` marker components** and the
   `NotShadowCaster` sweep over static spawn sites, with a guard test.
3. **Blob shadows (S1)** — ships a visible improvement immediately and is the
   permanent low-end fallback.
4. **Probe bake + per-object SH uniform (A2)** — the bigger piece; the toon
   shaders gain a "radiance from SH" path beside the lightmap path.
5. **Dynamic-only shadow map (S3)**, composited through the alpha channel.
6. Measure on a phone. If it holds, make S1 a quality setting rather than a
   fallback.

Steps 1–3 are low-risk and independently useful. Step 4 is where the real
design work is (probe validity, interpolation, streaming the grid).

---

## 8. Open questions for you

- **Is WebGL2 actually a target?** It is a second build with its own feature
  set, and `VolumetricFog` already doesn't work there. If "web" means WebGPU
  only, several constraints above relax and A3 becomes more attractive. If
  WebGL2 is real, we should stand up that build *before* building against its
  assumptions, because nothing currently proves it runs.
- **How many dynamic objects, and how big?** A handful of characters points
  firmly at A2. Dozens of moving props, or large ones, pushes toward A3.
- **Should placed point/spot lights affect dynamic objects?** The probe bake
  gives this for free (they're in `BakeLighting::local_lights`), but it means
  a character's lighting changes only at probe resolution — a lamp passed
  closely will pop between probes rather than sweep.
- **Do dynamic objects need to shadow each other**, or only the static world?
  Only-the-world is cheaper and simpler.
