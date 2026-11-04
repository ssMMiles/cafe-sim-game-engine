# Edge detection (outlines)

The black outlines are a screen-space post-process: one fullscreen pass that
reads the camera's depth and normal prepasses and darkens every pixel where
either one breaks. It draws no geometry and knows nothing about meshes. A
line exists only where two neighbouring pixels disagree about *how far away*
the surface is or *which way it faces*.

| Piece | Where |
|-------|-------|
| Shader | `assets/shaders/edge_detection_post.wesl` |
| Pass, pipeline, settings | `src/game/gfx/post_process.rs` |
| Knobs (`EdgeStyle` resource) | Inspector → nothing selected → **Edges** |
| On/off | F2, or View ▸ Edge detection (`UserSettings::edge_detection_enabled`) |

## 1. Where it runs

```
Prepass (depth + normals) → main pass → tonemapping → edge detection → FXAA → upscaling
```

The pass is a `Core3d` system in `Core3dSystems::PostProcess`,
`.after(tonemapping).before(fxaa)`. It runs before FXAA so the lines get
antialiased like the rest of the image. Until 2026-11-04 it only said
`.after(tonemapping)`, which FXAA also says, so the scheduler picked their
order.

The camera (`game/gfx/camera.rs`) carries `DepthPrepass`, `NormalPrepass`
and `EdgeDetectionSettings`, and keeps bevy's default `Msaa::Sample4`. The
prepass textures are therefore multisampled. The bind group layout
hard-codes that, and the shader reads sample 0. Every pixel reads the same
sample position, so this doesn't distort anything below. Turning MSAA off
would need a non-multisampled layout variant.

## 2. The inputs

**Depth.** Bevy's perspective projection is reverse-Z with an infinite far
plane, so the prepass stores

    depth = near / distance        (near = 0.1 m; 1 at the near plane, 0 at infinity)

where `distance` is along the view axis. Pixels nothing was drawn to
(sky) stay at the clear value, 0. The sky dome writes no depth.

**Normals.** World-space unit normals, packed `n * 0.5 + 0.5`, written by
bevy's standard prepass shader for every opaque material. Both toon
materials are `ExtendedMaterial<StandardMaterial, …>` and use the base
prepass, so a glTF normal map *does* reach this texture. The sky dome writes
its sphere normals, never in front of real geometry.

**Not in the prepass:** anything alpha-blended (blob shadows, by design,
which is how they stay outline-free), gizmos, and egui. These never get
outlines and never block them.

## 3. The two tests

Each pixel `c` looks along four lines through itself: horizontal, vertical
and both diagonals. On each line it reads the neighbours `a` and `b` at
`±thickness` pixels. Each test yields a *strength*: 1 is exactly at the
threshold, higher is a stronger edge.

### 3.1 Depth discontinuity: silhouettes

    step  = |a + b − 2c| / max(a, b, c)
    slope = min(|a − c|, |b − c|) / max(a, b, c)
    depth strength = step / (depth_threshold + grazing_bias × slope)

This is a **second difference**, and it relies on one fact: `near / distance`
is *affine in screen space across any plane*. Write a plane as `n·p = d` and
a pixel's ray as `p = t·r(x, y)`, where `r` is linear in the screen
coordinates. Then `1/t = n·r(x, y) / d`, also linear. So on any flat
surface, at any angle and any distance, `a + b − 2c = 0` exactly. It is
non-zero only where the surface breaks: an occlusion boundary (a jump) or a
crease (a kink).

Dividing by the nearest depth of the three makes the measure
**distance-independent**. At a step from a surface at distance `z` to one
behind it at `z′`, the pixel next to the boundary reads

    step = 1 − z / z′

So `depth_threshold = 0.04` means "draw a line where the surface behind is
at least ~4% further away", whether that's at 2 m or 200 m. A 1-pixel-thin
object (a pole) reads about twice that, because both neighbours are behind
it.

**Grazing bias.** Real ground isn't a plane. Seen nearly edge-on, a small
bump shifts where the next pixel's ray lands by a lot, and the second
difference picks it up. The `slope` term is the flatter side's own
pixel-to-pixel depth change. A frontal surface has almost none, so the
threshold stays at `depth_threshold`. Ground at a grazing angle has a large
one, so its bumps must out-step its own slope before they draw. A genuine
silhouette still passes easily: across the boundary, one side is a different
surface entirely. With `grazing_bias = 0` you get the raw second difference.

`planes_read_as_no_edge_at_any_angle` and
`depth_steps_read_as_relative_distance` (post_process.rs) check both claims
against a transliteration of the shader function. The first sweeps 0–88°
tilt and 2–200 m.

### 3.2 Normal crease: folds

    normal strength = (1 − n_c · n_neighbour) / (1 − cos(crease_angle))

The angle between the centre normal and each of the eight neighbours, as
`1 − cos`. With `crease_angle = 30°`, two faces meeting at 30° or more
draw a line. World-space normals don't depend on the camera, so a crease
reads the same from every angle. Whether you *see* it is a separate matter:
a crease seen exactly edge-on occupies no pixels.

The normal test also catches most silhouettes, because the object and
whatever is behind it usually face different ways. The depth test exists
for the cases where they don't (§5).

### 3.3 Combining

    strength = max over all lines/neighbours of both tests
    edge     = smoothstep(1 − softness, 1 + softness, strength)
    fade     = 1 − smoothstep(fade_start, fade_end, distance of the nearest sample)
    colour   = mix(scene, edge_color.rgb, edge × fade × edge_color.a)

`softness` gives a band around the threshold, so an edge that hovers near it
(a crease of exactly `crease_angle`, a depth step that grows as you walk
away) fades instead of flickering. The fade uses the *nearest* depth in the
neighbourhood, so a silhouette against the sky (depth 0, "infinitely far")
fades with the object and not with the sky.

Both sides of a boundary are marked, so lines are about `2 × thickness`
pixels wide, centred on the boundary.

## 4. The knobs

All of these live in `EdgeStyle`. `EdgeDetectionSettings::new` turns them
into the uniform and keeps every divisor and smoothstep edge away from zero,
so no slider position can produce NaN.

| Knob | Default | Unit | Raise it when… | Lower it when… |
|------|---------|------|----------------|----------------|
| Depth step (`depth_threshold`) | 0.04 | `1 − near/far` | small props or terrain folds sprout lines | stacked surfaces facing the same way (table over floor, ridge over valley) lose their outline |
| Grazing bias | 1.0 | × own slope | distant or edge-on ground is noisy | edge-on silhouettes (a wall seen along its length) go missing |
| Crease angle | 30° | degrees | faceted low-poly meshes outline every facet | real folds (roof ridges, box edges, bevels) aren't drawn |
| Softness | 0.25 | fraction of threshold | edges flicker as you move | lines look grey or washed out |
| Thickness | 1 | px offset | you want heavier lines | thin things (fences, poles) break up: taps at ±r skip anything thinner than r |
| Fade start / end | 120 / 240 m | metres | — | the horizon is busy with lines under the fog |
| Colour | black, α 1 | linear RGBA | α is opacity | |

The style is saved **per user** in `editor_settings.json`, next to the dock
layout (`editor/ui/persist.rs`: debounced 1 s, flushed on exit to the menu,
an OPFS key on web). Every field defaults, so a file from before a knob
existed still loads. "Reset" in the panel restores the defaults.

## 5. Symptoms

| You see | Likely cause | Knob / fix |
|---------|--------------|------------|
| No line between a top face and the floor seen from above | same normal on both sides: depth only | lower Depth step |
| No line along a soft fold | crease shallower than Crease angle | lower Crease angle |
| Lines on every triangle of a rock | flat-shaded mesh: every facet is a real crease | raise Crease angle, or smooth the normals in the asset |
| A visible corner on a smooth-shaded low-poly mesh has no line | vertex normals interpolate *across* the corner, so per-pixel normals barely change there | author a hard edge (split normals) in the asset |
| Two boxes touching flush show no seam | same depth, same normal: nothing differs | nothing in screen space can see it |
| Speckles inside a textured surface | the material's normal map is in the prepass | raise Crease angle, or drop the normal map |
| Distant ground covered in lines | grazing bumps / fold silhouettes | raise Grazing bias, pull Fade in |
| Lines drawn over fog | lines are drawn after the scene, fog included | pull Fade in to match the fog |
| A line flickers on and off | edge sitting on a threshold | raise Softness |
| Transparent or blended objects have no outline | not in the prepass | by design (§2) |

## 6. What was wrong before 2026-11-04

The look you had was not the one the code described. These four problems
explain "edges not picked up, especially from above or at certain angles":

1. **The depth test was dead beyond about two metres.** It thresholded the
   raw difference `|d_neighbour − d_centre| × depth_sensitivity` against
   0.03. With `d = 0.1/z`, a step from `z` to `z + Δ` is
   `0.1·Δ / (z(z + Δ))`. For a 0.75 m table top over the floor seen from
   5 m: `0.0026 × 2 = 0.005`, a sixth of the threshold. Everything was left
   to the normal test, which can't see a silhouette between two surfaces
   facing the same way. Looking down from above is exactly that case.
2. **The normal threshold silently changed.** The camera spawned with
   `normal_sensitivity = 1.5` (lines from ~11.5° creases).
   `sync_edge_detection_settings` overwrote it with `UserSettings`' 0.1
   (~46° creases) on its first run, because a system's first run sees every
   resource as changed. Later it ran only when *some* user setting changed.
   So on first entry nothing under ~46° was outlined, and after
   exit-to-menu and re-entry the 1.5 value could come back until any
   toggle. The duplicate fields are gone from `UserSettings` (ABI 12);
   `EdgeStyle` is the one source, compared every frame.
3. **The antialiasing ramp faded real edges.** It sized a smoothstep from
   `dpdx/dpdy` of the edge strength. That value is a max over discrete
   neighbours, so it is discontinuous. Its derivative is huge exactly at
   edges, and it is shared across a 2×2 pixel quad. A legitimate weak edge
   next to a strong one was faded to ~60%, depending on quad alignment, so
   lines shimmered as the camera moved. Now there's a fixed band around the
   threshold, and FXAA does the geometric antialiasing.
4. **FXAA and the edge pass had no defined order** (§1).

Also: at the screen border the old kernel clamped samples onto the centre
pixel. Harmless for a first difference, but it would have made the new
second difference fire on every sloped plane, so that line is now skipped
at the border.

## 7. Open ends

- **Per-world looks.** `EdgeStyle` is a user setting (`editor_settings.json`),
  so every world shares it and a world zip doesn't carry it. If worlds ever
  need their own outline style, it moves into `EnvironmentData` next to the
  toon curve (`world.meta` v7 plus a legacy reader).
- **Terrain skirts** have zero-length normals in the mesh. The main pass
  fixes them up in `terrain.wesl`, but the prepass uses bevy's own vertex
  shader, which normalises a zero vector. Skirts are only visible through
  LOD cracks, but a NaN normal there would make that pixel's normal test
  undefined.
- **Thickness above 1** skips pixels between taps rather than dilating the
  line. A proper thick line would dilate the 1-px result instead.
- **One-sided lines.** The sign of the second difference says which side of
  a step is nearer, so the depth test could mark only the object's own
  silhouette pixel (1 px, never bleeding onto the background). Not done,
  to keep the current line weight.
