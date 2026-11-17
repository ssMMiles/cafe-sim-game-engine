# Transparency — plan

Status: **draft for review, 2026-11-17**. Nothing is built. Written for the
new cafe shell model, which has glass windows and is the first real consumer
of a lit transparent surface.

## 1. Goal

A glTF material authored with `alphaMode: BLEND` (window glass) or
`alphaMode: MASK` (foliage cards, grilles) renders as such under the scene
pass, and — the part that is easy to miss — **light treats it accordingly in
the bake**: sunlight comes through the cafe's windows instead of the
interior baking as dark as if they were bricked up.

Out of scope, deliberately (§6): tinted/dimmed transmission (glass casting a
coloured shadow), refraction or reflections, per-instance sorting inside one
draw, fog applied to the glass surface itself.

## 2. What exists today

- **One transparent path**: `SceneLayer::Transparent`, used only by
  `UnlitMaterial` (placement ghost, brush disc, blob shadows). Alpha-blended
  over the finished image, depth-*tested* but not written, **no normal
  write** (masked with `ColorWrites::empty()`), drawn after opaque + sky,
  back to front per draw (`order_scene_draws`).
- **The layer is a per-*type* const** — `SceneMaterial::LAYER` — so a
  `ToonMaterial` is opaque by construction. Transparency is per *material*
  (one glTF has opaque walls and glass panes), so this must become a
  function of the material's key.
- **`ToonMaterial` drops alpha on conversion.** `with_surface_of` ignores
  `gltf.alpha_mode`; `dropped_gltf_features` lists "transparency (toon is
  always opaque)" and warns. The shader writes `base_color.a` out but the
  pipeline never blends.
- **The bake treats every toon mesh as a wall.** `collect_occluder_triangles`
  (baking/bvh.rs) takes every `MeshMaterial3d<ToonMaterial>` mesh under
  static scenery into the BVH — it never reads the material asset. Sun,
  AO, bounce and probe rays all stop at a window pane.
- **Post passes key off depth + normals.** A surface that writes neither
  gets no outline and no fog, and whatever is behind it keeps both. That is
  the trade `UnlitMaterial` already makes, and it is *right* for glass:
  outlines belong to the window frame (separate opaque geometry in the
  glTF), and fog applies to what you see through the pane.

## 3. Phase 1 — draw it

Goal: the cafe shell's windows are visibly glass. Light is still wrong
(interior stays dark) until Phase 2.

1. **`SceneLayer` becomes per-material.** Replace
   `SceneMaterial::LAYER: SceneLayer` with
   `fn layer(key: &Self::Data) -> SceneLayer` (default: `Opaque`).
   `queue_draws` already holds `material.key` when it builds the
   `SceneDraw`, so this is a contained change in `scene_pass/material.rs`;
   terrain/sky/unlit implement it as the constant they are today.
2. **`ToonMaterial` gains an alpha mode** (engine-api → **ABI 18 → 19**):

   ```rust
   pub enum ToonAlpha { Opaque, Mask(f32), Blend }   // the glTF triad
   ```

   - `ToonMaterialKey` carries the *discriminant* (the pipeline differs);
     the mask **cutoff stays in `ToonSurfaceUniform`** (new field, mirrored
     in `toon_lighting.wesl`'s `ToonSurface` — a cutoff edit must not mint
     a new pipeline). The uniform grows; the WESL struct and the Rust
     `ShaderType` move together.
   - `Blend` → `SceneLayer::Transparent`: `ALPHA_BLENDING`, no depth write,
     normal target masked — all of which `SceneShaders::descriptor` already
     derives from the layer, so toon's `specialize` only has to pass the
     layer from the key.
   - `Mask` → stays `Opaque` (writes depth, normals, gets outlines, MSAA's
     alpha-to-coverage is a possible later nicety) + a `TOON_ALPHA_MASK`
     def: `if (alpha < surface.alpha_cutoff) { discard; }`. Discard applies
     to every target, so the outline follows the mask shape.
3. **Shader**: `alpha = base_color.a × texture.a` (today the texture's
   alpha is ignored); `Blend` outputs it, `Opaque` keeps forcing 1.0 so
   existing content cannot change appearance from a stray texture alpha.
4. **Conversion**: `with_surface_of` maps `gltf.alpha_mode` →
   `ToonAlpha`; `dropped_gltf_features` stops listing transparency.
   `ProbeToonMaterials` sharing is keyed per glTF material, so alpha flows
   through untouched for dynamic pieces too.
5. **Tests**: `shader_check` gains `TOON_ALPHA_MASK` in `TOON_DEFS` (the
   def-combination sweep covers it both ways); `with_surface_of` conversion
   tests for the three modes; a uniform-size test if one doesn't already
   cover `ToonSurfaceUniform`.

Known limitation, accepted: `order_scene_draws` sorts *draws* (a
`(material, mesh)` run) by the farthest instance, and panes inside one mesh
don't sort against each other at all. Flat, non-overlapping window panes
can't show it; two glass objects overlapping in view can pop. The fix
(single-instance draws in the transparent layer) trades batching and waits
for a sighting (§6). Same for double-sided blended *closed* shapes
self-sorting — window panes don't care.

## 4. Phase 2 — light comes through

Goal: Shift+B in the cafe gives a sunlit interior.

1. **Blend materials leave the BVH.** `collect_occluder_triangles` reads
   `Assets<ToonMaterial>` and skips meshes whose material is
   `ToonAlpha::Blend`. The filter is **Blend only** — `Mask` keeps
   occluding (foliage should shadow; per-texel mask sampling in the BVH is
   firmly out of scope). Everything downstream is free: sun shadow rays, AO
   (no contact shadow under glass), bounce, probe gathers, *and the GPU
   bake*, which uploads the same filtered triangle list.
2. **Glass keeps its lightmap.** A static blend sub-mesh still gets UV1,
   a bake job and an EXR like any toon surface — a pane catching the sun
   shows its tint banded like everything else, which is the look. This also
   dodges a real trap: `obj_{id}_{k}` lightmap files are numbered by the
   sub-mesh's *position* in `collect_toon_meshes`'s vec, so skipping jobs
   would have to preserve `k` or every later sub-mesh wears the wrong
   lightmap. We skip nothing, so the numbering never moves.
3. **Tests**: a `bvh` unit test in the style of
   `dynamic_lit_ancestry_is_excluded_from_static_scenery`, proving a Blend
   material under static scenery contributes no occluder triangles while a
   Mask one does.

No save-format change anywhere: alpha lives in the glTF material, nothing
new is persisted. Existing worlds need a **rebake** (Shift+B) to see light
through windows — stale lightmaps are just stale, not broken, and the
existing stale-bake warning already covers it.

## 5. Order of work

Phase 1 and 2 land together or 1 first — a glass window that reads as glass
but keeps the room dark is reviewable on its own. Each phase is a commit.

## 6. Deferred, with triggers

| Deferred | Trigger to revisit |
|---|---|
| Per-instance sorting in the transparent layer | visible popping between overlapping translucent draws |
| Transmission (tinted, dimmed shadows): `occluded()` → accumulated transmittance, mirrored in `lightmap_bake.wgsl` | glass reads as "hole in the wall" rather than glass in the baked light |
| Fog/god rays affecting the glass surface | big exterior windows into fogged distance look pasted-on |
| Alpha-to-coverage for Mask under MSAA | foliage edges alias |
| Editor override of a placed mesh's alpha (inspector) | first time someone wants glass not authored in the glTF |

## 7. Review questions

1. `ToonAlpha::Mask` in Phase 1, or ship Blend-only first? Nothing in the
   cafe needs Mask today; it is ~15 lines + one def, and skipping it would
   also skip the BVH asymmetry in §4.1 until wanted.
2. Should Blend surfaces stay lightmapped (§4.2) or render unlit at base
   colour? Lightmapped is proposed — it keeps one code path and the toon
   look — but it does spend atlas texels and bake time on every pane.
3. Happy with "no outline on glass"? Frames must then be modelled as
   opaque geometry in `Cafe.gltf` (they almost certainly already are).
