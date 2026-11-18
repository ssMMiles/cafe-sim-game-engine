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
Scene pass (colour + depth + normals, MSAA) → tonemapping → edge detection → atmosphere → upscaling
```

The pass is a `Core3d` system in `Core3dSystems::PostProcess`,
`.after(tonemapping)` and before the atmosphere through the
`PostProcessPass` set. There is no FXAA after it any more (removed
2026-11-17): MSAA antialiases the geometry in the scene pass, and the lines
antialias themselves (below), so all FXAA added was a blur over texture
detail.

The camera (`game/gfx/camera.rs`) carries `DepthPrepass`, `NormalPrepass`
and `EdgeDetectionSettings`, and keeps bevy's default `Msaa::Sample4`, so
the prepass textures are multisampled.

**The lines are antialiased by evaluating the edge test per MSAA sample**
and averaging the results. This pass runs after the colour MSAA resolve, so
the geometry's own MSAA cannot touch a line drawn here — read at one sample
and hard-thresholded, outlines came out quantised to whole pixels (the
jagged staircase FXAA used to paper over). Each sample index sits at the
same sub-pixel offset in every pixel, so each iteration of the loop traces
the same line shifted by a fraction of a pixel, and the average is N+1
coverage levels across it at N× MSAA — the same information MSAA gives a
polygon edge. The pipeline is specialized on the view's sample count
(`MSAA_SAMPLES`, the shader's loop bound) and carries a single-sampled
layout variant (`MULTISAMPLED` off) for a view without MSAA, where the loop
runs once and the look is the pre-2026-11-17 one. Cost: the pass's texture
loads scale with the sample count (~18 → ~72 loads/px at 4×); the A/B bench
prices it.

## 2. The inputs

**Depth.** Bevy's perspective projection is reverse-Z with an infinite far
plane, so the prepass stores

    depth = near / distance        (near = 0.1 m; 1 at the near plane, 0 at infinity)

where `distance` is along the view axis. Pixels nothing was drawn to
(sky) stay at the clear value, 0. The sky dome writes no depth.

**Normals.** World-space normals, packed `n * 0.5 + 0.5` into bevy's
`ViewPrepassTextures.normal` (`Rgb10a2Unorm`). Since 2026-11-08 there is no
normal *prepass*: the scene pass writes this texture as a second colour
target of its one geometry pass (`scene_pass/pipeline.rs`), from
`toon_lighting.wesl` — which flips a double-sided material's back faces
towards the viewer, or leaves would outline along every fold — and
`terrain_toon.wesl`. Normal maps don't reach this texture — toon materials
have none. The sky dome and the translucent overlays have the target masked
off, so where only they drew it keeps the clear value.

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
away) fades instead of flickering — tonal softness, distinct from the
geometric antialiasing, which is the per-sample average (§1): the whole
block above runs once per MSAA sample and `edge × fade` is averaged before
the colour mix. The fade uses the *nearest* depth in the
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
| Crease fade start / end | 40 / 80 m | metres | nearby surface detail vanishes too soon | mid-distance geometry is scribble (creases are surface detail; silhouettes keep the longer fade above) |
| Auto weight small / large | 0.6 / 1.5 m | metres of mesh size | furniture-sized props look too faint | grass-sized scenery is still too busy |
| Colour | black, α 1 | linear RGBA | α is opacity | |

The style is saved **per user** in `editor_settings.json`, next to the dock
layout (`editor/ui/persist.rs`: debounced 1 s, flushed on exit to the menu,
an OPFS key on web). Every field defaults, so a file from before a knob
existed still loads. "Reset" in the panel restores the defaults.

### 4.1 Per-object outline weight (2026-11-18)

Both tests are scale-free — a 60° fold between grass blades reads exactly
like a 60° fold on a roof — so small, busy scenery (grass tufts, foliage)
used to draw as dense scribble while buildings looked right. Each mesh
instance now carries a 4-level **outline weight** that scales its whole edge
amount:

- **Transport.** The weight rides the two otherwise-unused bits of the
  normal target's alpha (`Rgb10a2Unorm`): 2 bits of `SceneInstance::flags`
  (`OUTLINE_LEVEL_SHIFT`, set at extraction in `scene_pass/material.rs`) →
  written by `toon_lighting.wesl` as `alpha = level / 3` → read back here.
  Free at every step: the flags field, the alpha bits and the normal fetch
  all already existed. Terrain hardcodes 1.0 (it is the canvas, never the
  clutter), and the attachment's clear value has alpha 1.0, so sky reads as
  full weight too.
- **Automatic rule.** `auto_outline_level` over the mesh's world-space AABB
  size: under `auto_weight_small` → ⅓ strength, under `auto_weight_large` →
  ⅔, else full. Level 0 (no lines) is never assigned automatically.
- **Override.** engine-api's `OutlineWeight` component (ABI 20) on the mesh
  or any ancestor — a plugin's scene root, a placed object — pins the level;
  the nearest one up the hierarchy wins.
- **Silhouettes survive.** The pass takes the *strongest* weight among the
  texels it samples, so an edge between a faint object and anything
  full-weight (terrain, a wall, the sky) is somebody's silhouette and draws
  in full. Only edges whose whole neighbourhood is faint — a tuft's interior
  creases and blade-over-blade depth steps — are scaled down. Two faint
  objects overlapping also suppress their shared edge, which reads as clumps
  merging into one mass.

Separately, **creases fade with their own distance band**
(`crease_fade_start/end`, default 40–80 m vs the silhouettes' 120–240 m):
a crease is surface detail, and dense geometry turns to scribble at range if
its creases persist as far as its silhouette. With the two bands set equal
the split is exactly the old behaviour (the max of two smoothsteps of the
same softness is the smoothstep of the max).

Also worth knowing: the crease test is purely normal-based, so *flat-shaded*
foliage outlines every facet no matter what. Smoothing the mesh's normals in
the modelling tool removes those lines at the source and costs nothing.

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
   threshold, and the geometric antialiasing is the per-sample evaluation
   (§1; it was FXAA until 2026-11-17).
4. **FXAA and the edge pass had no defined order.** Fixed 2026-11-04 with
   an explicit `.before(fxaa)`; moot since 2026-11-17 — FXAA is gone.

Also: at the screen border the old kernel clamped samples onto the centre
pixel. Harmless for a first difference, but it would have made the new
second difference fire on every sloped plane, so that line is now skipped
at the border.

## 7. Open ends

- **Per-world looks.** `EdgeStyle` is a user setting (`editor_settings.json`),
  so every world shares it and a world zip doesn't carry it. If worlds ever
  need their own outline style, it moves into `EnvironmentData` next to the
  toon curve (`world.meta` v7 plus a legacy reader).
- ~~**Terrain skirts** have zero-length normals in the mesh.~~ Fixed
  2026-11-07. Terrain draws through our own scene pass now
  (`docs/SCENE_PASS_PLAN.md` Phase 1) and both of its passes share one vertex
  shader, so the skirt fix-up reaches the prepass. Nothing can hand the
  normal test a normalised zero vector any more.
- **Thickness above 1** skips pixels between taps rather than dilating the
  line. A proper thick line would dilate the 1-px result instead.
- **One-sided lines.** The sign of the second difference says which side of
  a step is nearer, so the depth test could mark only the object's own
  silhouette pixel (1 px, never bleeding onto the background). Not done,
  to keep the current line weight.
