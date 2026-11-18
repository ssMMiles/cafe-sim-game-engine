# Transparency — plan

Status: **Phases 1 + 2 built 2026-11-17** (ABI 19), same day the draft was
reviewed. Decisions from review: **Blend-only** (no `Mask` until something
needs it — §6.1 stands as the trigger), **glass stays lightmapped** (§4.2 as
proposed), **no outline on glass** (frames are opaque geometry in the glTF).
Written for the new cafe shell model, which has glass windows and is the
first real consumer of a lit transparent surface.

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

## 3. Phase 1 — draw it (built)

Goal: the cafe shell's windows are visibly glass.

As built, it is smaller than drafted: there is **no `ToonAlpha` enum, no
cutoff, no shader change at all** — Blend-only collapsed it to one `bool
alpha_blend` on `ToonMaterial` + `ToonMaterialKey`, and the fragment shader
already computed and wrote `base_color.a × texture.a`; the opaque pipeline's
blend-off state was the only thing discarding it.

1. **`SceneLayer` is per-material.** `SceneMaterial::LAYER` became
   `fn layer(key: &Self::Data) -> SceneLayer` (default `Opaque`);
   `queue_draws` files each draw under `M::layer(&material.key)` and **every
   specializer passes its layer through the same function**, so a draw's
   layer and its pipeline's blend/depth/mask state cannot disagree.
2. **`ToonMaterial::alpha_blend: bool`** (engine-api → **ABI 18 → 19**),
   mirrored into `ToonMaterialKey`. True → `SceneLayer::Transparent`:
   `ALPHA_BLENDING`, no depth write, normal target masked — all of which
   `SceneShaders::descriptor` already derives from the layer, so toon's
   `specialize` only passes the layer from the key. The `ToonAlpha` enum
   (with `Mask(cutoff)` in the uniform) is what this grows into if masking
   ever converts; nothing shipped blocks that shape.
3. **Shader**: untouched. It already multiplied the texture's alpha into
   `base_color` and wrote `base_color.a` out; opaque pipelines ignore it
   exactly as before.
4. **Conversion**: `with_surface_of` sets `alpha_blend` from
   `gltf.alpha_mode == Blend`; `dropped_gltf_features` now reports only the
   unsupported modes (`Mask` and the exotic ones). `ProbeToonMaterials`
   sharing is keyed per glTF material, so alpha flows through untouched for
   dynamic pieces too.
5. **Tests**: `only_blend_converts_to_a_transparent_material` (engine-api)
   covers the conversion; no new shader defs, so `shader_check` is
   unchanged.

Known limitation, accepted: `order_scene_draws` sorts *draws* (a
`(material, mesh)` run) by the farthest instance, and panes inside one mesh
don't sort against each other at all. Flat, non-overlapping window panes
can't show it; two glass objects overlapping in view can pop. The fix
(single-instance draws in the transparent layer) trades batching and waits
for a sighting (§6). Same for double-sided blended *closed* shapes
self-sorting — window panes don't care.

## 4. Phase 2 — light comes through (built)

Goal: Shift+B in the cafe gives a sunlit interior.

1. **Blend materials leave the BVH.** `collect_occluder_triangles` reads
   `Assets<ToonMaterial>` and skips meshes whose material has
   `alpha_blend` (a material not yet in `Assets` counts as opaque — the
   conservative default). The filter is **Blend only** — if masking ever
   converts, masked foliage should keep occluding (per-texel mask sampling
   in the BVH is firmly out of scope). Everything downstream is free: sun
   shadow rays, AO (no contact shadow under glass), bounce, probe gathers,
   *and the GPU bake*, which uploads the same filtered triangle list.
2. **Glass keeps its lightmap.** A static blend sub-mesh still gets UV1,
   a bake job and an EXR like any toon surface — a pane catching the sun
   shows its tint banded like everything else, which is the look. This also
   dodges a real trap: `obj_{id}_{k}` lightmap files are numbered by the
   sub-mesh's *position* in `collect_toon_meshes`'s vec, so skipping jobs
   would have to preserve `k` or every later sub-mesh wears the wrong
   lightmap. We skip nothing, so the numbering never moves.
3. **Tests**: `alpha_blended_materials_do_not_occlude` (bvh.rs) proves a
   Blend material under static scenery contributes no occluder triangles
   while an opaque one beside it does.

No save-format change anywhere: alpha lives in the glTF material, nothing
new is persisted. Existing worlds need a **rebake** (Shift+B) to see light
through windows — stale lightmaps are just stale, not broken, and the
existing stale-bake warning already covers it.

## 5. Deferred, with triggers

| Deferred | Trigger to revisit |
|---|---|
| Per-instance sorting in the transparent layer | visible popping between overlapping translucent draws |
| Transmission (tinted, dimmed shadows): `occluded()` → accumulated transmittance, mirrored in `lightmap_bake.wgsl` | glass reads as "hole in the wall" rather than glass in the baked light |
| Fog/god rays affecting the glass surface | big exterior windows into fogged distance look pasted-on |
| Alpha-to-coverage for Mask under MSAA | foliage edges alias |
| Editor override of a placed mesh's alpha (inspector) | first time someone wants glass not authored in the glTF |

## 6. Review questions (answered 2026-11-17)

1. `Mask` in Phase 1? **Blend-only.** Masking waits for a consumer; the
   §5 trigger stands.
2. Blend surfaces lightmapped or unlit? **Lightmapped** — one code path,
   toon-banded panes.
3. No outline on glass? **Agreed** — frames are opaque geometry in
   `Cafe.gltf`.
