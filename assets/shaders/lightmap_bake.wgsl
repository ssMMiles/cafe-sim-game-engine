// GPU compute shader for terrain lightmap baking.
//
// Each invocation corresponds to one texel in the output lightmap. It samples
// the heightmap cache (a flat f32 array extracted on the CPU for the chunk +
// surrounding ray-reach region), computes a directional shadow factor and
// hemisphere AO factor, then writes the combined u32 value (0–255) to the
// output storage buffer.
//
// Bind group layout (group 0):
//   binding 0 – heightmap  : read-only storage buffer, Vec<f32>
//   binding 1 – uniforms   : uniform buffer, BakeUniforms
//   binding 2 – output     : storage buffer, Vec<u32>  (one entry per texel)

// ----------------------------------------------------------------------------
// Uniforms
// ----------------------------------------------------------------------------

struct BakeUniforms {
    // World-space origin of the extracted heightmap cache (min corner).
    cache_min_x: f32,
    cache_min_z: f32,
    // Physical size of one tile in world units.
    tile_size_x: f32,
    tile_size_z: f32,
    // Dimensions of the flat heightmap cache array.
    cache_stride: u32,
    cache_depth: u32,
    // Output lightmap resolution (pixels per side).
    terrain_resolution: u32,
    // Number of AO hemisphere rays.
    ao_rays: u32,
    // World-space origin of the chunk being baked.
    chunk_origin_x: f32,
    chunk_origin_z: f32,
    // Physical side length of the chunk in world units.
    chunk_world_size: f32,
    // AO hemisphere radius in world units.
    ao_radius: f32,
    // Normalised sun direction (points FROM surface TOWARD sun).
    sun_dir_x: f32,
    sun_dir_y: f32,
    sun_dir_z: f32,
    // Number of DDA steps for shadow and AO rays.
    shadow_steps: u32,
    // Tile-space minimum of the cache (i32 reinterpreted as u32 via bitcast).
    cache_tile_min_x: i32,
    cache_tile_min_z: i32,
    // Indirect lighting parameters.
    bounce_rays: u32,
    bounce_strength: f32,
    terrain_albedo: f32,
    bounce_count: u32,
    // Starting row of this dispatch strip within the full lightmap.
    // Strip-based dispatch writes `strip_rows` rows at a time to avoid TDR.
    row_offset: u32,
    // Number of hull triangles in the hull_tris binding.
    num_hull_tris: u32,
}

// ----------------------------------------------------------------------------
// Bindings
// ----------------------------------------------------------------------------

@group(0) @binding(0)
var<storage, read> heightmap: array<f32>;

@group(0) @binding(1)
var<uniform> u: BakeUniforms;

@group(0) @binding(2)
var<storage, read_write> output: array<u32>;

@group(0) @binding(3)
var<storage, read> hull_tris: array<f32>;
// 9 floats per triangle: [v0.xyz, v1.xyz, v2.xyz]

// ----------------------------------------------------------------------------
// Constants
// ----------------------------------------------------------------------------

const PI: f32 = 3.14159265358979;
const TAU: f32 = 6.28318530717959;
const GOLDEN: f32 = 0.6180339;

// ----------------------------------------------------------------------------
// Hash helpers
// ----------------------------------------------------------------------------

/// MurmurHash3 finalizer — maps any u32 to a well-distributed u32.
fn hash_u32(x_in: u32) -> u32 {
    var x = x_in;
    x = x ^ (x >> 16u);
    x = x * 0x45d9f3bu;
    x = x ^ (x >> 16u);
    x = x * 0x45d9f3bu;
    x = x ^ (x >> 16u);
    return x;
}

/// Map a u32 hash to f32 in [0, 1).
fn float01(h: u32) -> f32 {
    return bitcast<f32>(0x3f800000u | (h >> 9u)) - 1.0;
}

// ----------------------------------------------------------------------------
// Heightmap helpers
// ----------------------------------------------------------------------------

/// Look up a tile from the flat heightmap cache by tile coordinates.
/// Returns 0.0 for out-of-bounds tiles.
fn sample_tile(tx: i32, tz: i32) -> f32 {
    let lx = tx - u.cache_tile_min_x;
    let lz = tz - u.cache_tile_min_z;
    if lx < 0 || lz < 0 {
        return 0.0;
    }
    let ux = u32(lx);
    let uz = u32(lz);
    if ux >= u.cache_stride || uz >= u.cache_depth {
        return 0.0;
    }
    return heightmap[uz * u.cache_stride + ux];
}

/// Bilinearly interpolate world height at world position (wx, wz).
fn world_height(wx: f32, wz: f32) -> f32 {
    let fx = wx / u.tile_size_x;
    let fz = wz / u.tile_size_z;
    let ix = i32(floor(fx));
    let iz = i32(floor(fz));
    let tx = fx - f32(ix);
    let tz = fz - f32(iz);
    let h00 = sample_tile(ix,     iz);
    let h10 = sample_tile(ix + 1, iz);
    let h01 = sample_tile(ix,     iz + 1);
    let h11 = sample_tile(ix + 1, iz + 1);
    return h00 * (1.0 - tx) * (1.0 - tz)
         + h10 * tx          * (1.0 - tz)
         + h01 * (1.0 - tx) * tz
         + h11 * tx          * tz;
}

/// Estimate the surface normal at world position (wx, wz) using central differences.
fn world_normal(wx: f32, wz: f32) -> vec3<f32> {
    let ex = u.tile_size_x;
    let ez = u.tile_size_z;
    let hl = world_height(wx - ex, wz);
    let hr = world_height(wx + ex, wz);
    let hb = world_height(wx, wz - ez);
    let hf = world_height(wx, wz + ez);
    let dx = (hr - hl) / (2.0 * ex);
    let dz = (hf - hb) / (2.0 * ez);
    return normalize(vec3<f32>(-dx, 1.0, -dz));
}

// ----------------------------------------------------------------------------
// Shadow ray
// ----------------------------------------------------------------------------

/// Möller–Trumbore ray-triangle intersection.
/// Returns t > 0.001 if hit, else -1.0.
fn ray_tri_hit(ro: vec3<f32>, rd: vec3<f32>, i: u32) -> f32 {
    let b  = i * 9u;
    let v0 = vec3<f32>(hull_tris[b],       hull_tris[b + 1u], hull_tris[b + 2u]);
    let v1 = vec3<f32>(hull_tris[b + 3u],  hull_tris[b + 4u], hull_tris[b + 5u]);
    let v2 = vec3<f32>(hull_tris[b + 6u],  hull_tris[b + 7u], hull_tris[b + 8u]);
    let e1 = v1 - v0;
    let e2 = v2 - v0;
    let h  = cross(rd, e2);
    let a  = dot(e1, h);
    if abs(a) < 0.0001 { return -1.0; }
    let f  = 1.0 / a;
    let s  = ro - v0;
    let u_  = f * dot(s, h);
    if u_ < 0.0 || u_ > 1.0 { return -1.0; }
    let q  = cross(s, e1);
    let v_ = f * dot(rd, q);
    if v_ < 0.0 || (u_ + v_) > 1.0 { return -1.0; }
    let t  = f * dot(e2, q);
    if t > 0.001 { return t; } else { return -1.0; }
}

/// Returns true if any hull triangle is hit within max_dist.
fn hull_tris_hit(ro: vec3<f32>, rd: vec3<f32>, max_dist: f32) -> bool {
    for (var i = 0u; i < u.num_hull_tris; i = i + 1u) {
        let t = ray_tri_hit(ro, rd, i);
        if t > 0.0 && t < max_dist { return true; }
    }
    return false;
}

/// March along sun_dir from origin. Returns 1.0 if unoccluded, 0.0 if shadowed.
fn cast_shadow_ray(origin: vec3<f32>, sun_dir: vec3<f32>) -> f32 {
    let max_dist = u.ao_radius * 4.0;
    let step_size = max_dist / f32(u.shadow_steps);
    for (var i: u32 = 1u; i <= u.shadow_steps; i = i + 1u) {
        let t = f32(i) * step_size;
        let p = origin + sun_dir * t;
        let terrain_h = world_height(p.x, p.z);
        if terrain_h > p.y {
            return 0.0;
        }
    }
    // Check convex hull shadow proxies for placed objects.
    if hull_tris_hit(origin, sun_dir, max_dist) {
        return 0.0;
    }
    return 1.0;
}

// ----------------------------------------------------------------------------
// Ambient occlusion
// ----------------------------------------------------------------------------

/// Golden-ratio low-discrepancy sequence value for ray index i.
fn golden_seq(i: u32) -> f32 {
    return fract(f32(i) * GOLDEN);
}

/// Compute hemisphere AO at origin with given normal.
/// Returns the fraction of unoccluded rays ∈ [0, 1].
fn compute_ao(origin: vec3<f32>, normal: vec3<f32>) -> f32 {
    if u.ao_rays == 0u {
        return 1.0;
    }

    let tf        = make_tangent_frame(normal);
    let step_size = u.ao_radius / f32(u.shadow_steps);
    var unoccluded: f32 = 0.0;

    for (var i: u32 = 0u; i < u.ao_rays; i = i + 1u) {
        let world_dir = hemisphere_dir(i, u.ao_rays, normal, tf, 0.0);
        if dot(world_dir, normal) <= 0.0 {
            continue;
        }

        var occluded = false;
        for (var s: u32 = 1u; s <= u.shadow_steps; s = s + 1u) {
            let p = origin + world_dir * (f32(s) * step_size);
            if world_height(p.x, p.z) > p.y {
                occluded = true;
                break;
            }
        }
        if !occluded {
            unoccluded = unoccluded + 1.0;
        }
    }

    return unoccluded / f32(u.ao_rays);
}

// ----------------------------------------------------------------------------
// Tangent frame
// ----------------------------------------------------------------------------

struct TangentFrame {
    tangent:   vec3<f32>,
    bitangent: vec3<f32>,
}

/// Build an orthonormal tangent frame for the given normal.
fn make_tangent_frame(normal: vec3<f32>) -> TangentFrame {
    var up: vec3<f32>;
    if abs(normal.y) < 0.99 {
        up = vec3<f32>(0.0, 1.0, 0.0);
    } else {
        up = vec3<f32>(1.0, 0.0, 0.0);
    }
    let tangent   = normalize(cross(up, normal));
    let bitangent = normalize(cross(normal, tangent));
    return TangentFrame(tangent, bitangent);
}

/// Sample a cosine-weighted hemisphere direction for index `seed` out of
/// `n_total` rays, using the golden-ratio sequence for the azimuth.
/// `azimuth_offset` is added to the azimuth angle to decorrelate texel patterns.
/// Returns the world-space direction (the y-up hemisphere is oriented by `normal`).
fn hemisphere_dir(seed: u32, n_total: u32, normal: vec3<f32>, tf: TangentFrame, azimuth_offset: f32) -> vec3<f32> {
    let uv    = (f32(seed) + 0.5) / f32(n_total);
    let angle = TAU * golden_seq(seed) + azimuth_offset;
    let r     = sqrt(uv);
    let local = vec3<f32>(r * cos(angle), sqrt(1.0 - uv), r * sin(angle));
    return normalize(tf.tangent * local.x + normal * local.y + tf.bitangent * local.z);
}

// ----------------------------------------------------------------------------
// Indirect bounce
// ----------------------------------------------------------------------------

/// Multi-bounce indirect lighting: cast `bounce_rays` hemisphere rays from
/// origin and follow each path through up to `bounce_count` terrain reflections.
///
/// Throughput is multiplied by `terrain_albedo` at each bounce so successive
/// bounces contribute diminishing amounts.
///
/// `texel_seed` is a per-texel hash used to rotate the hemisphere sample azimuth
/// and decorrelate the structured golden-ratio pattern across texels, preventing
/// correlated stripe artifacts when the toon quantizer bands the result.
fn compute_bounce(origin: vec3<f32>, normal: vec3<f32>, texel_seed: u32) -> f32 {
    if u.bounce_rays == 0u || u.bounce_count == 0u {
        return 0.0;
    }
    let sun_dir = vec3<f32>(u.sun_dir_x, u.sun_dir_y, u.sun_dir_z);
    // Use the full shadow reach for bounce marching — back-of-hill surfaces
    // need to see sunlit terrain that can be 4× further than ao_radius.
    let step_size = (u.ao_radius * 4.0) / f32(u.shadow_steps);

    // Rotate the entire hemisphere sample set by a per-texel azimuth so adjacent
    // texels don't share the same ray directions → no correlated stripe patterns.
    let azimuth_offset = float01(hash_u32(texel_seed)) * TAU;

    var accum = 0.0;
    for (var i = 0u; i < u.bounce_rays; i = i + 1u) {
        // Primary ray direction (cosine-weighted sample with per-texel azimuth offset).
        let tf0    = make_tangent_frame(normal);
        let dir0   = hemisphere_dir(i, u.bounce_rays, normal, tf0, azimuth_offset);
        let cos_w0 = max(dot(dir0, normal), 0.0);
        if cos_w0 <= 0.0 {
            continue;
        }

        var cur_pos    = origin;
        var cur_dir    = dir0;
        var cur_cos_w  = cos_w0;
        var throughput = 1.0;

        for (var b = 0u; b < u.bounce_count; b = b + 1u) {
            var hit = false;
            for (var s = 1u; s <= u.shadow_steps; s = s + 1u) {
                let pos = cur_pos + cur_dir * (f32(s) * step_size);
                let h   = world_height(pos.x, pos.z);
                // Bug fix: strict > to avoid self-intersection on flat terrain.
                if h > pos.y {
                    // Bug fix: 0.5 unit offset (was 0.1) to clear the step size
                    // (~1.25 units) so the next bounce doesn't immediately re-hit.
                    let hit_pos = vec3<f32>(pos.x, h + 0.5, pos.z);
                    let hn      = world_normal(pos.x, pos.z);
                    let ndotl_q = max(dot(hn, sun_dir), 0.0);
                    let vis_q   = cast_shadow_ray(hit_pos, sun_dir);
                    // Bug fix: no cur_cos_w — it cancels with the PDF for
                    // cosine-weighted hemisphere sampling.
                    accum = accum + throughput * ndotl_q * vis_q * u.terrain_albedo;
                    throughput = throughput * u.terrain_albedo;

                    // Resample direction at hit point using a proper per-bounce hash
                    // rather than a cyclic seed shift — avoids correlated directions.
                    let bounce_seed = hash_u32(
                        texel_seed + i * 1000003u + b * 2654435761u
                    );
                    let bounce_az  = float01(hash_u32(bounce_seed + 1u)) * TAU;
                    let bounce_idx = u32(float01(bounce_seed) * f32(u.bounce_rays)) % u.bounce_rays;
                    let tf         = make_tangent_frame(hn);
                    let new_dir    = hemisphere_dir(bounce_idx, u.bounce_rays, hn, tf, bounce_az);
                    cur_cos_w      = max(dot(new_dir, hn), 0.0);
                    cur_pos        = hit_pos;
                    cur_dir        = new_dir;
                    hit            = true;
                    break;
                }
            }
            if !hit || cur_cos_w <= 0.0 {
                break;
            }
        }
    }
    return accum / f32(u.bounce_rays);
}

// ----------------------------------------------------------------------------
// Main compute entry point
// ----------------------------------------------------------------------------

@compute @workgroup_size(16, 16, 1)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    let res = u.terrain_resolution;
    // Offset ty by the strip start row so each dispatch covers the correct slice.
    let tx = id.x;
    let ty = id.y + u.row_offset;
    if tx >= res || ty >= res {
        return;
    }
    let pixel_idx = ty * res + tx;

    let uv_u = f32(tx) / f32(max(res - 1u, 1u));
    let uv_v = f32(ty) / f32(max(res - 1u, 1u));

    let wx = u.chunk_origin_x + uv_u * u.chunk_world_size;
    let wz = u.chunk_origin_z + uv_v * u.chunk_world_size;
    // 0.5 unit offset clears the step size (~1.25 units at 64 steps / 80 units)
    // so the ray origin doesn't immediately re-hit the surface it came from.
    let wy = world_height(wx, wz) + 0.5;

    let origin = vec3<f32>(wx, wy, wz);
    let sun_dir = vec3<f32>(u.sun_dir_x, u.sun_dir_y, u.sun_dir_z);
    let normal  = world_normal(wx, wz);

    let shadow  = cast_shadow_ray(origin, sun_dir);
    let ao      = compute_ao(origin, normal);
    let ndotl   = max(dot(normal, sun_dir), 0.0);

    // Per-texel seed for decorrelated hemisphere sampling in compute_bounce.
    let texel_seed: u32 = tx * 1000003u + ty * 2654435761u;

    let direct   = ndotl * shadow * (0.5 + ao * 0.5);
    let bounce   = compute_bounce(origin, normal, texel_seed);
    let combined = clamp(direct + u.bounce_strength * bounce, 0.0, 1.0);
    output[pixel_idx] = u32(combined * 255.0);
}
