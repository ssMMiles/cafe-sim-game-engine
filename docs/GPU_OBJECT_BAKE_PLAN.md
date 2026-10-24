# Moving the object lightmap bake to the GPU

Status: **stage 1 done** (instrumentation + rasterise/shade split + rayon).
Stage 2 (the GPU path itself) is **not started, and deliberately not yet
committed to** — run a bake, read the numbers it now prints, and decide.

## Where the time actually goes

Terrain already bakes on the GPU (`baking/gpu.rs` + `lightmap_bake.wgsl`).
Placed objects do not — `mod.rs::run_bake` calls
`objects::bake_object_lightmaps` directly, under a comment that says
"always CPU". Two things make that the whole of the problem:

1. **It is single-threaded.** `rasterize_mesh_lightmap` is a plain nested
   `for` loop over the triangle's pixel bounding box. The CPU *terrain* path,
   by contrast, is `into_par_iter()` over texels (`terrain.rs:66`). Objects
   never got the same treatment.
2. **It runs inline in `Update`.** `run_bake` is an ordinary system and calls
   the object bake synchronously, so the whole app stalls until it returns.
   The GPU terrain path dispatches and returns immediately, picking results up
   later through a readback observer. So even after parallelising, Shift+B
   would still freeze the frame — just for less time.

### Why it got dramatically worse recently

Object atlases used to be sized by triangle *count* alone, which gave the
cafe's two-triangle floor a 32 px texture. They are now sized by world-space
area, so the same floor bakes at 512². That was the right fix — it is what
made shadows on the floor usable — but it is a **256× increase in texels** for
exactly the geometry the player stands closest to.

Per-texel ray budget, at the current defaults (`ao_rays: 16`,
`shadow_steps: 64`, `ao_steps: 32`):

| | per texel |
|---|---|
| Sun shadow ray | 1 (64 DDA steps + BVH) |
| AO rays | 16 (32 steps each + BVH) |
| Local light rays | 1 per light in range (4–48 steps) |

Objects skip indirect bounce (`sun_and_ambient(..., 0.0)`), so they are
cheaper per texel than terrain — but there is no parallelism to hide it.

A 512² atlas is ~262k texels, of which ~20% are covered: the gutter leaves an
inset of `1 - 2 × 0.18` per axis, and the triangle fills half of that, so
`0.64² / 2 ≈ 0.205`. That is ~54k shaded texels × ~17 rays ≈ **0.9M rays per
sub-mesh**, single-threaded. The cafe shell alone is five sub-meshes, and the
rasteriser walks each triangle's full bounding box (~2× the covered area) to
find them.

These are back-of-envelope figures derived from the config, not measured —
**step 1 below is to measure, not to assume.**

## What makes this tractable

The lighting model is already shared and already proven on the GPU. Compare
the CPU object texel loop (`objects.rs:305–353`) with the compute shader's
entry point (`lightmap_bake.wgsl:623`): after the first few lines they do the
same thing, calling the same-named functions.

```
                     CPU object loop          lightmap_bake.wgsl main()
  derive position    barycentric interp       heightmap sample
  derive normal      barycentric interp       heightmap gradient
  ───────────────────────────────────────────────────────────────────
  shadow ray         cast_shadow_ray          cast_shadow_ray
  AO                 compute_ao               compute_ao
  local lights       local_lights             local_lights
  pack + store       texels[i] = radiance     output[i] = pack_f16x2(...)
```

Only the first two lines differ, and only in *where the texel's world position
and normal come from*. Everything expensive is identical and already written
in WGSL, traversing a BVH that is already uploaded.

Note that object rays need the terrain heightmap too (`cast_shadow_ray` takes
a `BakeHeightCache` so objects are shadowed by hills), and that buffer is
already part of the GPU bind group.

## Stage 1 — measure, and split rasterise from shade — **done**

`rasterize_mesh_lightmap` is now two passes:

```rust
struct TexelGeom { world_pos: Vec3, normal: Vec3, face_normal: Vec3 }

fn rasterize_geometry(mesh, world_mat, res) -> Vec<Option<TexelGeom>>   // serial, cheap
fn shade_geometry(&[Option<TexelGeom>], res, cache, bvh, config, lighting)
    -> LightmapBuffer                                                   // par_iter
```

The `written` mask now falls out of `Option::is_some` instead of being
threaded through the rasteriser. That also removed a wart: the old guard
implied triangles might contend for texels, when in fact every triangle's
bounding box lies inside its own disjoint atlas cell.

`BakeStats` reports per pass at the end of each bake:

```
Object bake: 12 sub-mesh(es), 134217 of 1310720 texels covered (10%),
             largest atlas 512², ≥2.3M rays
  rasterize 0.04s | shade 11.82s | blur+dilate 0.21s | exr 0.35s | total 12.42s
```

Per-sub-mesh detail (resolution, triangle count, coverage) is at `debug`
level so it is available without spamming a normal run.

**Read these numbers before starting stage 2.** The thing to check is that
`shade` really does dominate. If `exr` or `blur+dilate` turns out to be a
meaningful share, moving shading to the GPU buys proportionally less than the
arithmetic above suggests — and the EXR writes in particular are much easier
to fix (they are serial and synchronous, and could be moved off-thread).

**This does not fix the frame stall.** The bake still runs inline in `Update`.
Stage 2 fixes that as a side effect of going async; if the stall is the real
complaint rather than total time, that moves stage 2 up regardless of what
the timings say.

`Vec<Option<TexelGeom>>` is the g-buffer stage 2 uploads, so none of this is
throwaway.

## Stage 2 — GPU object bake

### Shader

Add a second entry point to `lightmap_bake.wgsl`, not a second shader:

```wgsl
@group(0) @binding(6)
var<storage, read> texel_geom: array<TexelGeom>;   // pos.xyz, normal.xyz, face_normal.xyz

@compute @workgroup_size(16, 16, 1)
fn main_object(@builtin(global_invocation_id) id: vec3<u32>) {
    // bounds check + pixel_idx as in main()
    let g = texel_geom[pixel_idx];
    if g.covered == 0u { return; }           // gutter: left for CPU dilation

    let origin = g.world_pos + g.face_normal * 0.02;
    let shadow = cast_shadow_ray(origin, origin, sun_dir);   // single origin
    let ao     = compute_ao(origin, origin, g.normal);
    let ndotl  = max(dot(g.normal, sun_dir), 0.0);
    var radiance = sun_rgb * ndotl * shadow + ambient_rgb * ao;   // no bounce
    radiance += local_lights(origin, origin, g.normal);
    output[pixel_idx] = /* pack as in main() */;
}
```

Two details that must match the CPU path exactly or objects will shade
differently from terrain:

- Objects pass the **same origin twice** (`origin, origin`), where terrain
  passes a lifted DDA origin and a surface origin. The lift exists so the
  terrain march cannot re-hit the ground it left; an object texel is not on
  the ground.
- Objects get **no bounce term**. `main()` adds
  `u.bounce_strength * bounce`; `main_object` must not.

Both entry points compile from one source, so `cast_shadow_ray`, `compute_ao`,
`local_lights` and the BVH traversal cannot drift between terrain and objects
— which is the failure mode that matters most here.

### Buffers

Reuse the existing bind group layout and add one binding. Per object job:

| Binding | Contents | Shared? |
|---|---|---|
| 0 heightmap | terrain heights | **one for the whole bake region**, not per object |
| 1 uniforms | `BakeUniformsData` + atlas res | per job |
| 2 output | `res² × vec2<u32>` | per job |
| 3 bvh_nodes | scene BVH | shared, already built once per bake |
| 4 lights | `BakedLight[]` | shared |
| 5 bvh_tris | scene triangles | shared |
| 6 texel_geom | **new**: `res²` × 3 `vec3` + coverage flag | per job |

The heightmap is the one change worth calling out. Terrain extracts a
`BakeHeightCache` per chunk; objects currently extract one per *object*
(`objects.rs:90`), which on GPU would mean re-uploading overlapping heightmaps
dozens of times. Extract **one cache covering the whole bake region plus ray
reach** and upload it once. For a 3×3 chunk region at `ao_radius * 4` margin
that is roughly 1400² floats ≈ 8 MB — a single upload, not per job.

`BakeUniformsData` needs an atlas-resolution field distinct from
`terrain_resolution`, or a rename to something neutral. Mind the existing
`bake_uniforms_are_vec4_aligned` test.

### Dispatch and readback

Mirror the terrain path rather than inventing a second mechanism:
`GpuBakeQueue` → `top_up_gpu_bake` → `BakeComputeNode` → `Readback` component
→ `handle_bake_complete` observer. The serial queue exists to bound VRAM to
one output buffer at a time; object atlases are smaller than terrain chunks,
so several could be in flight, but start serial and only relax it if measured
throughput justifies it.

`BakeReadbackTag` currently carries `chunk_x`/`chunk_y`. Generalise it to an
enum (`Terrain { x, y }` / `Object { id, sub_mesh }`) so the completion
handler can pick the right EXR path (`object_lightmap_path`) and the right
material.

### What stays on the CPU

Deliberately, in the completion handler:

- **UV unwrap** (`uv_cache.ensure`) mutates `Assets<Mesh>` and must happen in
  the main world before dispatch. It is once per mesh asset and cheap.
- **Rasterisation** into the g-buffer. Doing it on the GPU means a render pass
  that draws the mesh with UV1 as clip position into an MRT — the classic
  technique, and genuinely better, but it is a whole extra pipeline for a step
  that is already negligible next to the rays. Not now.
- **`blur_cells` and `dilate`.** Both are small neighbourhood passes over an
  atlas that is already being read back, and both are fiddly to port
  (cell-confined, mask-aware). The terrain path blurs on the CPU after
  readback too.
- **EXR write and material swap**, as terrain does today.

## Risks

- **Divergence between the two entry points.** The single biggest risk, and
  the reason for one shader with two entries rather than two shaders. Worth a
  test that bakes one small mesh both ways and asserts the texels agree within
  tolerance — the CPU path is not going away (WASM has no compute path and
  falls back to it), so the two must stay in step regardless.
- **Many small dispatches.** A scene with lots of small props means lots of
  tiny atlases, where per-job overhead (buffer allocation, bind group,
  readback round-trip) could dominate. If measurement shows this, batch
  several sub-meshes into one atlas-array dispatch — but measure first.
- **WASM.** No compute path there; the CPU path remains the fallback. Stage 1
  therefore benefits WASM and stage 2 does not, which is another argument for
  doing stage 1 properly rather than treating it as scaffolding.
- **The frame stall is a separate problem from throughput.** Stage 2 fixes it
  as a side effect of going async; stage 1 does not. If the stall is the real
  complaint rather than total time, that reorders the priorities.

## Suggested order

1. ~~Instrument the existing bake.~~ **Done** — `BakeStats`.
2. ~~Stage 1 split + rayon.~~ **Done.**
3. **Next: run a bake and read the numbers.** Decide from them whether stage 2
   is worth it, and whether the frame stall or the total time is the actual
   complaint. *(—)*
4. Stage 2: shader entry point, shared heightmap, g-buffer binding, tag enum,
   wire into the existing queue. *(large)*
5. CPU/GPU agreement test. *(small, but do not skip it)*
