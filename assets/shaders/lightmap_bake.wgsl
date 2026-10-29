// GPU compute shader for terrain lightmap baking.
//
// Each invocation corresponds to one texel in the output lightmap. It samples
// the heightmap cache (a flat f32 array extracted on the CPU for the chunk +
// surrounding ray-reach region), computes sun shadow, hemisphere AO and
// indirect bounce, then shades with the same model as `BakeLighting::shade`
// on the CPU and writes one Rgba16Float texel (packed into a vec2<u32>) to
// the output storage buffer.
//
// Bind group layout (group 0):
//   binding 0 – heightmap  : read-only storage buffer, Vec<f32>
//   binding 1 – uniforms   : uniform buffer, BakeUniforms
//   binding 2 – output     : storage buffer, Vec<vec2<u32>> (one entry per texel)
//   binding 3 – bvh_nodes  : read-only storage buffer, array<BvhNode>
//   binding 4 – lights     : read-only storage buffer, array<BakedLight>
//   binding 5 – bvh_tris   : read-only storage buffer, array<BvhTriangle>
//
// Shadow rays take two origins (see raycast.rs): `origin` for the coarse
// terrain march, lifted clear of the ground, and `mesh_origin` right at the
// surface for the exact BVH test.

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
    // Finished lightmap resolution (pixels per side); the grid actually
    // baked is `terrain_resolution + 2 * margin`.
    terrain_resolution: u32,
    // Number of AO hemisphere rays.
    ao_rays: u32,
    // Index of the chunk being baked. Texel positions and seeds derive from
    // the chunk's place in the world texel grid, so two chunks compute
    // bit-identical values for the texels they share (ChunkBakeGrid).
    chunk_index_x: i32,
    chunk_index_y: i32,
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
    // Number of nodes in the scene BVH; 0 means no occluders at all.
    num_bvh_nodes: u32,
    // Number of DDA steps for AO rays (shorter range than shadow/bounce).
    ao_steps: u32,
    // Sun colour × intensity and ambient colour × intensity, lightmap units.
    sun_r: f32,
    sun_g: f32,
    sun_b: f32,
    ambient_r: f32,
    ambient_g: f32,
    ambient_b: f32,
    // Number of placed point/spot lights in the `lights` binding.
    num_lights: u32,
    // Extra texels baked on every side of the chunk so the CPU blur sees
    // real neighbour data at the chunk edge; cropped after readback.
    margin: u32,
    // Clamp on a local light's shadow-ray step count (BakeConfig::max_light_shadow_steps).
    max_light_shadow_steps: u32,
    _pad1: u32,
    _pad2: u32,
}

/// Must match `BvhNode` in `baking/bvh.rs`: 32 bytes.
struct BvhNode {
    aabb_min: vec3<f32>,
    // Interior (count == 0): left child; right child is left + 1.
    // Leaf (count > 0): first triangle index.
    left_or_first: u32,
    aabb_max: vec3<f32>,
    count: u32,
}

/// Must match `BvhTriangle` in `baking/bvh.rs`: 48 bytes.
struct BvhTriangle {
    v0: vec3<f32>,
    _p0: f32,
    v1: vec3<f32>,
    _p1: f32,
    v2: vec3<f32>,
    _p2: f32,
}

/// Must match `BakedLight` in `baking/lightmap.rs` field for field.
struct BakedLight {
    position: vec3<f32>,
    inv_range_sq: f32,
    color: vec3<f32>,
    range: f32,
    direction: vec3<f32>,
    spot_scale: f32,
    spot_offset: f32,
    // Three scalars, NOT a vec3: a trailing vec3 would be aligned to 16 and
    // push the struct to 80 bytes, while the Rust side is 64.
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
}

// ----------------------------------------------------------------------------
// Bindings
// ----------------------------------------------------------------------------

@group(0) @binding(0)
var<storage, read> heightmap: array<f32>;

@group(0) @binding(1)
var<uniform> u: BakeUniforms;

@group(0) @binding(2)
var<storage, read_write> output: array<vec2<u32>>;
// One Rgba16Float texel per entry: pack_f16x2(rg), pack_f16x2(b, 1.0).

@group(0) @binding(3)
var<storage, read> bvh_nodes: array<BvhNode>;

@group(0) @binding(4)
var<storage, read> lights: array<BakedLight>;

@group(0) @binding(5)
var<storage, read> bvh_tris: array<BvhTriangle>;

// Hits closer than this are the ray's own surface. Mirrors bvh::RAY_EPSILON.
const RAY_EPSILON: f32 = 0.001;

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
// f16 packing
// ----------------------------------------------------------------------------

/// IEEE-754 binary16 bits for `value`, round-to-nearest-even.
///
/// Hand-rolled because the builtin `pack2x16float` needs naga's
/// SHADER_FLOAT16_IN_FLOAT32 capability, which this compute pipeline is not
/// granted — the module fails validation if it is used.
fn f32_to_f16_bits(value: f32) -> u32 {
    let bits = bitcast<u32>(value);
    let sign = (bits >> 16u) & 0x8000u;
    let exp32 = i32((bits >> 23u) & 0xFFu);
    let mant = bits & 0x7FFFFFu;

    if (exp32 == 0xFF) {
        // Inf or NaN.
        if (mant != 0u) {
            return sign | 0x7E00u;
        }
        return sign | 0x7C00u;
    }

    let exp16 = exp32 - 127 + 15;
    if (exp16 >= 31) {
        // Overflow → inf.
        return sign | 0x7C00u;
    }
    if (exp16 <= 0) {
        if (exp16 < -10) {
            // Underflow → signed zero.
            return sign;
        }
        // Subnormal half: shift the implicit-1 mantissa down and round.
        let full = mant | 0x800000u;
        let shift = u32(14 - exp16);
        var sub = full >> shift;
        let rem = full & ((1u << shift) - 1u);
        let halfway = 1u << (shift - 1u);
        if (rem > halfway || (rem == halfway && (sub & 1u) == 1u)) {
            sub = sub + 1u;
        }
        return sign | sub;
    }

    var h = sign | (u32(exp16) << 10u) | (mant >> 13u);
    let rem = mant & 0x1FFFu;
    // A carry out of the mantissa correctly bumps the exponent (and rounds
    // the largest normals up to inf).
    if (rem > 0x1000u || (rem == 0x1000u && (h & 1u) == 1u)) {
        h = h + 1u;
    }
    return h;
}

/// Two f16s in one u32, `.x` in the low half — the layout of `pack2x16float`
/// and of little-endian `Rgba16Float` texel memory.
fn pack_f16x2(v: vec2<f32>) -> u32 {
    return f32_to_f16_bits(v.x) | (f32_to_f16_bits(v.y) << 16u);
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

/// Möller–Trumbore ray-triangle intersection, both faces.
/// Returns t > RAY_EPSILON if hit, else -1.0. Mirrors bvh::ray_triangle.
fn ray_tri_hit(ro: vec3<f32>, rd: vec3<f32>, tri: BvhTriangle) -> f32 {
    let e1 = tri.v1 - tri.v0;
    let e2 = tri.v2 - tri.v0;
    let h  = cross(rd, e2);
    let a  = dot(e1, h);
    if abs(a) < 0.000001 { return -1.0; }
    let f  = 1.0 / a;
    let s  = ro - tri.v0;
    let u_  = f * dot(s, h);
    if u_ < 0.0 || u_ > 1.0 { return -1.0; }
    let q  = cross(s, e1);
    let v_ = f * dot(rd, q);
    if v_ < 0.0 || (u_ + v_) > 1.0 { return -1.0; }
    let t  = f * dot(e2, q);
    if t > RAY_EPSILON { return t; } else { return -1.0; }
}

/// Reciprocal that stays finite for near-zero components so the slab test
/// never produces `0 * inf = NaN`. Mirrors bvh::safe_inv.
fn safe_inv(d: f32) -> f32 {
    if abs(d) < 1e-8 {
        return select(1e8, -1e8, d < 0.0);
    }
    return 1.0 / d;
}

fn slab_hit(aabb_min: vec3<f32>, aabb_max: vec3<f32>, ro: vec3<f32>, inv_d: vec3<f32>, max_t: f32) -> bool {
    let t0 = (aabb_min - ro) * inv_d;
    let t1 = (aabb_max - ro) * inv_d;
    let tmin = min(t0, t1);
    let tmax = max(t0, t1);
    let enter = max(max(tmin.x, tmin.y), max(tmin.z, 0.0));
    let exit  = min(min(tmax.x, tmax.y), min(tmax.z, max_t));
    return enter <= exit;
}

/// Does any scenery triangle block the ray between RAY_EPSILON and max_t?
/// Stack traversal of the flat BVH; mirrors TriangleBvh::occluded exactly.
/// The builder caps depth at 32, so 64 entries can't overflow.
fn bvh_occluded(ro: vec3<f32>, rd: vec3<f32>, max_t: f32) -> bool {
    if u.num_bvh_nodes == 0u {
        return false;
    }
    let inv_d = vec3<f32>(safe_inv(rd.x), safe_inv(rd.y), safe_inv(rd.z));

    var stack: array<u32, 64>;
    var sp: u32 = 1u;
    stack[0] = 0u;

    loop {
        if sp == 0u {
            break;
        }
        sp = sp - 1u;
        let node = bvh_nodes[stack[sp]];
        if !slab_hit(node.aabb_min, node.aabb_max, ro, inv_d, max_t) {
            continue;
        }
        if node.count > 0u {
            let end = node.left_or_first + node.count;
            for (var i = node.left_or_first; i < end; i = i + 1u) {
                let t = ray_tri_hit(ro, rd, bvh_tris[i]);
                if t > 0.0 && t < max_t {
                    return true;
                }
            }
        } else {
            stack[sp] = node.left_or_first;
            stack[sp + 1u] = node.left_or_first + 1u;
            sp = sp + 2u;
        }
    }
    return false;
}

/// March along sun_dir from `origin` over the terrain, then test scenery from
/// `mesh_origin`. Returns 1.0 if unoccluded, 0.0 if shadowed.
fn cast_shadow_ray(origin: vec3<f32>, mesh_origin: vec3<f32>, sun_dir: vec3<f32>) -> f32 {
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
    if bvh_occluded(mesh_origin, sun_dir, max_dist) {
        return 0.0;
    }
    return 1.0;
}

/// March from `origin` toward a light `dist` away, stopping at the light
/// rather than at a fixed reach. Mirrors `raycast::cast_shadow_ray_segment`.
/// Returns 1.0 if the light is visible, 0.0 if occluded.
fn cast_shadow_ray_segment(origin: vec3<f32>, mesh_origin: vec3<f32>, dir: vec3<f32>, dist: f32, steps: u32) -> f32 {
    let n = max(steps, 1u);
    let step_size = dist / f32(n);
    // Skip the final sample: it sits on the light itself, where terrain
    // directly beneath it would read as a false occluder.
    for (var i: u32 = 1u; i < n; i = i + 1u) {
        let p = origin + dir * (f32(i) * step_size);
        if world_height(p.x, p.z) > p.y {
            return 0.0;
        }
    }
    if bvh_occluded(mesh_origin, dir, dist) {
        return 0.0;
    }
    return 1.0;
}

/// Shadow-ray steps for a light `dist` away, matched to the sun ray's
/// sampling density. Mirrors `BakeConfig::light_shadow_steps`.
fn light_shadow_steps(dist: f32) -> u32 {
    let step_len = max((u.ao_radius * 4.0) / f32(max(u.shadow_steps, 1u)), 0.0001);
    return clamp(u32(ceil(dist / step_len)), 4u, max(u.max_light_shadow_steps, 4u));
}

/// Summed contribution of every placed light at a surface point.
/// Mirrors `BakeLighting::local_lights` + `BakedLight::sample`.
fn local_lights(origin: vec3<f32>, world_pos: vec3<f32>, normal: vec3<f32>) -> vec3<f32> {
    var total = vec3<f32>(0.0);

    for (var i: u32 = 0u; i < u.num_lights; i = i + 1u) {
        let light = lights[i];

        let to_light = light.position - world_pos;
        let dist_sq = dot(to_light, to_light);
        if dist_sq >= light.range * light.range || dist_sq < 1e-8 {
            continue;
        }
        let dist = sqrt(dist_sq);
        let dir = to_light / dist;

        let ndotl = dot(normal, dir);
        if ndotl <= 0.0 {
            continue;
        }

        // Smooth bounded falloff, not physical inverse-square.
        let falloff = max(1.0 - dist_sq * light.inv_range_sq, 0.0);
        let attenuation = falloff * falloff;

        let cos_angle = dot(-dir, light.direction);
        let spot = clamp(cos_angle * light.spot_scale + light.spot_offset, 0.0, 1.0);

        let radiance = light.color * (ndotl * attenuation * spot);
        if all(radiance <= vec3<f32>(0.0)) {
            continue;
        }

        total = total + radiance * cast_shadow_ray_segment(
            origin, world_pos, dir, dist, light_shadow_steps(dist)
        );
    }

    return total;
}

/// Distance to the closest scenery triangle between RAY_EPSILON and max_t,
/// or max_t if there is none. Same traversal as bvh_occluded but it cannot
/// stop at the first hit; each hit shrinks the bound instead. Mirrors
/// TriangleBvh::nearest_hit.
fn bvh_nearest(ro: vec3<f32>, rd: vec3<f32>, max_t: f32) -> f32 {
    if u.num_bvh_nodes == 0u {
        return max_t;
    }
    let inv_d = vec3<f32>(safe_inv(rd.x), safe_inv(rd.y), safe_inv(rd.z));
    var best = max_t;

    var stack: array<u32, 64>;
    var sp: u32 = 1u;
    stack[0] = 0u;

    loop {
        if sp == 0u {
            break;
        }
        sp = sp - 1u;
        let node = bvh_nodes[stack[sp]];
        if !slab_hit(node.aabb_min, node.aabb_max, ro, inv_d, best) {
            continue;
        }
        if node.count > 0u {
            let end = node.left_or_first + node.count;
            for (var i = node.left_or_first; i < end; i = i + 1u) {
                let t = ray_tri_hit(ro, rd, bvh_tris[i]);
                if t > 0.0 && t < best {
                    best = t;
                }
            }
        } else {
            stack[sp] = node.left_or_first;
            stack[sp + 1u] = node.left_or_first + 1u;
            sp = sp + 2u;
        }
    }
    return best;
}

// ----------------------------------------------------------------------------
// Ambient occlusion
// ----------------------------------------------------------------------------

/// Golden-ratio low-discrepancy sequence value for ray index i.
fn golden_seq(i: u32) -> f32 {
    return fract(f32(i) * GOLDEN);
}

/// Distance-weighted hemisphere visibility against terrain (from `origin`)
/// and scenery (from `mesh_origin`), out to ao_radius. A ray hitting at
/// distance t contributes t / ao_radius, so near occluders dominate — the
/// contact shadow under a chair — and a canopy far overhead only dims.
/// Terrain is marched first (ordered along the ray, so first hit = nearest),
/// then scenery is searched inside that distance. Mirrors raycast::compute_ao.
/// Returns mean visibility ∈ [0, 1].
fn compute_ao(origin: vec3<f32>, mesh_origin: vec3<f32>, normal: vec3<f32>) -> f32 {
    if u.ao_rays == 0u {
        return 1.0;
    }

    let tf        = make_tangent_frame(normal);
    let step_size = u.ao_radius / f32(u.ao_steps);
    var visibility: f32 = 0.0;

    for (var i: u32 = 0u; i < u.ao_rays; i = i + 1u) {
        let world_dir = hemisphere_dir(i, u.ao_rays, normal, tf, 0.0);

        var hit_t = u.ao_radius;
        for (var s: u32 = 1u; s <= u.ao_steps; s = s + 1u) {
            let t = f32(s) * step_size;
            let p = origin + world_dir * t;
            if world_height(p.x, p.z) > p.y {
                hit_t = t;
                break;
            }
        }
        hit_t = bvh_nearest(mesh_origin, world_dir, hit_t);

        visibility = visibility + hit_t / u.ao_radius;
    }

    return visibility / f32(u.ao_rays);
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
fn compute_bounce(origin: vec3<f32>, mesh_origin: vec3<f32>, normal: vec3<f32>, texel_seed: u32) -> f32 {
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
    let tf0    = make_tangent_frame(normal);
    for (var i = 0u; i < u.bounce_rays; i = i + 1u) {
        // Primary ray direction (cosine-weighted sample with per-texel azimuth offset).
        let dir0   = hemisphere_dir(i, u.bounce_rays, normal, tf0, azimuth_offset);
        let cos_w0 = max(dot(dir0, normal), 0.0);
        if cos_w0 <= 0.0 {
            continue;
        }

        var cur_pos      = origin;
        var cur_mesh_pos = mesh_origin;
        var cur_dir      = dir0;
        var cur_cos_w    = cos_w0;
        var throughput   = 1.0;

        for (var b = 0u; b < u.bounce_count; b = b + 1u) {
            var hit = false;
            for (var s = 1u; s <= u.shadow_steps; s = s + 1u) {
                let pos = cur_pos + cur_dir * (f32(s) * step_size);
                let h   = world_height(pos.x, pos.z);
                // Bug fix: strict > to avoid self-intersection on flat terrain.
                if h > pos.y {
                    // A wall between here and the terrain hit absorbs the
                    // path: scenery is opaque to bounce light, not a mirror.
                    if bvh_occluded(cur_mesh_pos, cur_dir, f32(s) * step_size) {
                        break;
                    }
                    // Bug fix: 0.5 unit offset (was 0.1) to clear the step size
                    // (~1.25 units) so the next bounce doesn't immediately re-hit.
                    let hit_pos      = vec3<f32>(pos.x, h + 0.5, pos.z);
                    let hit_mesh_pos = vec3<f32>(pos.x, h + 0.02, pos.z);
                    let hn      = world_normal(pos.x, pos.z);
                    let ndotl_q = max(dot(hn, sun_dir), 0.0);
                    let vis_q   = cast_shadow_ray(hit_pos, hit_mesh_pos, sun_dir);
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
                    cur_mesh_pos   = hit_mesh_pos;
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
    let padded = res + 2u * u.margin;
    // Offset ty by the strip start row so each dispatch covers the correct slice.
    let tx = id.x;
    let ty = id.y + u.row_offset;
    if tx >= padded || ty >= padded {
        return;
    }
    let pixel_idx = ty * padded + tx;

    // Index of this texel in the world grid: `res - 1` texels per chunk, the
    // margin reaching into the neighbours. Mirrors ChunkBakeGrid::world_xz
    // operation for operation.
    let span = i32(max(res - 1u, 1u));
    let gx = u.chunk_index_x * span + i32(tx) - i32(u.margin);
    let gy = u.chunk_index_y * span + i32(ty) - i32(u.margin);
    let wx = f32(gx) / f32(span) * u.chunk_world_size;
    let wz = f32(gy) / f32(span) * u.chunk_world_size;
    let surface = vec3<f32>(wx, world_height(wx, wz), wz);
    let sun_dir = vec3<f32>(u.sun_dir_x, u.sun_dir_y, u.sun_dir_z);
    let normal  = world_normal(wx, wz);

    // Two ray origins (see raycast.rs): the terrain march starts 0.5 up so
    // the coarse DDA (~1.25-unit steps) can't re-hit the ground it left; the
    // exact BVH test starts at the surface so a crate on the ground still
    // shadows it.
    let origin      = surface + vec3<f32>(0.0, 0.5, 0.0);
    let mesh_origin = surface + normal * 0.02;

    let shadow  = cast_shadow_ray(origin, mesh_origin, sun_dir);
    let ao      = compute_ao(origin, mesh_origin, normal);
    let ndotl   = max(dot(normal, sun_dir), 0.0);

    // Per-texel seed for decorrelated hemisphere sampling in compute_bounce,
    // keyed by world texel (ChunkBakeGrid::texel_seed) so seams agree.
    let texel_seed: u32 = bitcast<u32>(gx) * 1000003u + bitcast<u32>(gy) * 2654435761u;

    let bounce = compute_bounce(origin, mesh_origin, normal, texel_seed);

    // Same model as BakeLighting::shade on the CPU: AO shapes only the
    // ambient term; direct sun is attenuated by the shadow ray alone.
    let sun_rgb     = vec3<f32>(u.sun_r, u.sun_g, u.sun_b);
    let ambient_rgb = vec3<f32>(u.ambient_r, u.ambient_g, u.ambient_b);
    var radiance = sun_rgb * (ndotl * shadow + u.bounce_strength * bounce) + ambient_rgb * ao;

    // Placed point/spot lights, each occluded only by geometry between this
    // texel and the light itself.
    radiance = radiance + local_lights(origin, mesh_origin, normal);

    output[pixel_idx] = vec2<u32>(
        pack_f16x2(radiance.rg),
        pack_f16x2(vec2<f32>(radiance.b, 1.0)),
    );
}
