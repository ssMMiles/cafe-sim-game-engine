# Atmosphere: distance fog and god rays

Two screen-space effects sharing one fullscreen pass, both driven from the
depth prepass. Fog tints distant surfaces towards a haze colour; god rays
add light shafts streaming from the sun. Either can be switched off on its
own, and with both off the pass doesn't run at all.

| Piece | Where |
|-------|-------|
| Shader | `assets/shaders/atmosphere_post.wesl` |
| Pass, pipeline, settings | `src/game/gfx/atmosphere.rs` |
| Knobs (`AtmosphereStyle`) | Inspector → nothing selected → **Fog** / **God rays** |
| Cost readout | Inspector → nothing selected → **GPU timings** (§7) |

## 1. Where it runs, and why it is a post-process

```
main pass → tonemapping → edge detection → ATMOSPHERE → FXAA
```

Fog is in-scattered light along the view ray. All it needs is the
fragment's world position, and the depth buffer already has that. Doing it
here instead of inside `toon_lighting.wesl` and `terrain_toon.wesl` buys
four things:

- **No engine-api ABI bump.** `ToonShading` is shared with plugin dylibs;
  growing it costs a version gate and a rebuild of every plugin.
- **No third copy of a shared shader block.** The toon curve is already
  duplicated between the two material shaders with a test holding the
  copies in step; fog would have been a second such pair.
- **It covers everything that wrote depth**, not just the two toon
  materials — so editor props on `StandardMaterial` fog identically.
- **The sky is excluded for free.** `SkyDomeMaterial` disables depth writes,
  and because the prepass runs the material's own `specialize`, it writes no
  prepass depth either. Sky pixels keep the cleared reverse-Z value of 0,
  which the shader treats as "leave alone". That is what we want:
  `SkyColors` already paints the horizon.

Ordering is fixed by `PostProcessPass` (`gfx/mod.rs`), a system set shared by
the two passes so neither has to expose its internals. Edges are drawn
*first*, so outlines fog along with the geometry they trace rather than
staying crisp in the haze. The edge pass keeps its own distance fade; with
fog on they want roughly matching ranges.

Replaces bevy's `DistanceFog` and `VolumetricFog`, both removed from the
camera. See §5 for why the volumetric one was never going to work here.

## 2. Reconstructing the world position

The shader binds the screen texture and the multisampled depth prepass, and
reads both with `textureLoad` — no sampler, no implicit LOD, no
derivatives, so every line is safe under WebGPU's uniform control flow rule
even inside the god-ray loop. Checked with `node tools/tint-check.mjs`.

The uniform carries `world_from_clip` (the camera's `world_from_view ×
clip_from_view⁻¹`) and the camera position, both computed on the CPU from
the camera's `GlobalTransform` and `Projection`. Then:

```
ndc        = (uv.x·2 − 1, 1 − uv.y·2, depth)
world_pos  = (world_from_clip · vec4(ndc, 1)).xyz / .w
```

Unprojecting a full matrix rather than rebuilding a ray from fov/aspect/near
means the pass cannot drift from whatever projection the camera actually
used. `depth <= 0` is the cleared value — sky — and the whole fog branch is
skipped there, which also keeps the division away from the infinite far
plane.

## 3. The fog model

Beer–Lambert against a density that may fall off with height:

```
ρ(y)     = density · exp(−(y − base) / falloff)
optical  = ∫ ρ along the segment from `start` to the surface
fog      = (1 − exp(−optical)) · max_opacity
```

The integral has a closed form, `ρ(y₀) · (1 − e^−k)/k · length` with
`k = Δy/falloff`, whose `k → 0` limit is the uniform case; `falloff = 0`
takes the uniform path directly. **`optical_depth` is mirrored in Rust and
`height_fog_matches_numerical_integration` checks the closed form against
brute-force integration** over ascending, descending and level segments —
that test is why height fog can be trusted. Two saturation clamps (optical
depth ≤ 50, height exponent ±30) keep `exp` away from overflow;
`extreme_inputs_stay_finite` pins them.

Then two stylistic touches:

- **Sun tint.** `pow(max(dot(view_dir, to_sun), 0), exponent)` mixes the
  sun's colour into the haze when you look towards it.
- **Steps.** Quantising the fog *amount* into flat bands, the way the toon
  curve quantises brightness. 0 leaves it smooth, which is the default
  because it is the familiar result; the banded version is the one that
  matches the art style, so it is worth a look.

Fog is applied **after** the toon curve, not before. Before would mean
distance changed how bright a surface reads and therefore which band it
lands in — distant geometry would step between bands as you walked. After,
the bands stay put and the haze lies over them.

## 4. The god-ray model

For each pixel, march `samples` steps towards the sun's screen position,
counting how much of the path is open sky:

```
step   = (sun_uv − uv) · spread / samples
weight = 1
for each step:  if depth(probe) == 0 { open += weight };  weight *= decay
rays   = sun_colour · intensity · (open / samples) · visibility
```

Occlusion comes from the depth buffer, so anything that wrote depth blocks
a shaft. No shadow map is involved, which is the whole point (§5).

`visibility` is computed on the CPU (`sun_on_screen`): 0 when the sun is
behind the camera, 1 while it is on screen, fading over `RAY_OFFSCREEN_FADE`
(0.35 of a screen) as it leaves — without that the shafts would pop off the
instant the sun crossed the frame edge.
`god_rays_follow_the_sun_across_the_screen` covers all four cases.

The loop bound is the constant `MAX_RAY_SAMPLES` (64) with an early break on
the uniform count; `ray_sample_cap_matches_the_shader` keeps the Rust and
WGSL copies of that constant in step, since a mismatch would silently cap
the slider.

**This is a cheap approximation, not volumetrics.** Shafts are built from
screen-space occlusion, so they only appear where the sun is on or near
screen, they inherit whatever is in the depth buffer (the sky dome writes
none, so the sky always counts as "open"), and geometry outside the frame
casts nothing.

## 5. Why not bevy's `VolumetricFog`

It raymarches `step_count` samples per pixel (64 by default) and samples a
directional light's **cascaded shadow map** at each one. `BakedSun` has
`shadow_maps_enabled: false` because everything in this engine is baked, so
using it would mean switching real-time shadow passes back on over the whole
visible world — re-rendering geometry per cascade, every frame — purely to
feed the fog, on top of ~130 M shadow samples per frame at 1080p. On the web
build, where the renderer shares one thread with the game, that is not a
trade worth making.

Both `VolumetricFog` and `DistanceFog` were on the camera until 2026-11-04
and neither did anything useful: the volumetric one was inert (bevy strips
it when no `VolumetricLight` exists, and none did), and `DistanceFog` only
reached `StandardMaterial` meshes because our own shaders never called
bevy's fog function. `docs/GRAPHICS.md` §17 has the history.

## 6. The knobs

`AtmosphereStyle`, persisted per user in `editor_settings.json` next to the
dock layout and `EdgeStyle`. Every field defaults, so a file written before
a knob existed still loads. "Reset atmosphere" restores the defaults.

### Fog

| Knob | Default | Unit | Raise it when… |
|------|---------|------|----------------|
| Colour | warm haze | linear RGB | — (match it to the sky's horizon) |
| Density | 0.0012 | per metre | the distance is too clear |
| Start | 15 | m | nearby geometry is picking up haze |
| Max opacity | 0.9 | 0–1 | the far distance should flatten completely |
| Height falloff | 0 (uniform) | m | you want mist pooling in valleys |
| Height base | 0 | m | the mist should sit at a different altitude |
| Sun tint | 0.35 | × sun colour | looking into the sun should glow more |
| Sun tightness | 6 | exponent | that glow should be a tighter halo |
| Steps | 0 (smooth) | bands | you want the haze banded like the shading |

### God rays

| Knob | Default | Unit | Note |
|------|---------|------|------|
| Enabled | **off** | | this is the experiment, not the baseline |
| Intensity | 0.35 | × sun colour | |
| Decay | 0.96 | per sample | below ~0.9 the shafts stay short |
| Spread | 0.7 | fraction | how far towards the sun each pixel marches |
| Samples | 32 | taps/pixel | **the entire cost** — see §7 |

**Where this should live.** Like `EdgeStyle`, this is a user setting, so
every world shares it and a world zip doesn't carry it. Fog is really
*world* data — a cave and a desert want different haze, and the colour
wants to match that world's sky. The natural home is `EnvironmentData` next
to the toon curve (`world.meta` v7 plus a legacy reader). Left as a user
setting for now because god rays may not survive the experiment, and a
format bump for something that might be deleted is the wrong order to do
things in.

## 7. Measuring what it costs

Inspector → nothing selected → **GPU timings** lists each render pass with
its time in milliseconds, worst first. To price an effect: note the number,
toggle the effect, look again.

This comes from bevy's `RenderDiagnosticsPlugin`, which brackets each
recorded span in GPU timestamp queries. Bevy only adds it under
`tracing-tracy`, so `src/lib.rs` adds it explicitly; our two passes are
wrapped in `pass_span`s ("edge detection", "atmosphere") and bevy's own
passes come with theirs, so the list covers the whole frame.

Two caveats worth knowing:

- Timestamps need the `TIMESTAMP_QUERY` wgpu feature. Bevy's default
  `WgpuSettingsPriority::Functionality` requests every feature the adapter
  supports, so this is present on Metal/Vulkan/DX12. Where it is missing,
  bevy falls back to CPU *encode* time, which measures how long it took to
  record the commands, not to run them. The panel marks those rows `ᶜ` and
  changes its heading, because the two are not comparable.
- The numbers are smoothed over recent frames, so give them a second to
  settle after a toggle.

The god-ray cost should scale close to linearly with **Samples**: it is
`samples` depth taps per pixel and almost nothing else. Halving the slider
should roughly halve the pass. If it doesn't, the pass is bound by something
else — bandwidth on the full-screen read/write, most likely.

## 8. Open ends

- **Per-world fog** (§6).
- **The god-ray march is unjittered**, so at low sample counts the shafts
  can show banding along the march direction. The usual fix is a per-pixel
  dither offset; not done, since the effect may not stay.
- **Blended surfaces don't fog.** Blob shadows, the placement preview and
  the terrain brush disc write no depth, so they take the fog of whatever
  is behind them. For the preview and the brush that is arguably right; for
  blob shadows it means a distant shadow stays slightly too dark.
- **Fog and the edge fade are tuned separately** and can disagree — the
  outline fade is in `EdgeStyle`, in metres, and does not know the fog
  exists.
