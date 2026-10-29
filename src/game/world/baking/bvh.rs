//! Triangle BVH for bake-time shadow rays, shared by every bake path.
//!
//! Built once per bake pass from the world-space triangles of all static
//! scenery, then traversed on the CPU (terrain, object, AO and bounce rays)
//! and uploaded *verbatim* to the GPU terrain bake, which traverses the same
//! nodes with the same algorithm. One structure means the two paths cannot
//! disagree about what casts a shadow.
//!
//! This replaces the convex-hull proxies: hulls lost all concavity (an arch
//! was a solid block) and the object being baked had to be excluded to avoid
//! artifacts, so nothing ever shadowed itself. Real triangles fix both;
//! self-shadowing now works and acne is avoided by offsetting ray origins
//! along the face normal instead of excluding geometry.
//!
//! Layout is deliberately simple — median split on the longest centroid axis,
//! small leaves, children stored adjacently — because it has to be rebuilt
//! every bake and traversed from WGSL with a fixed-size stack.

use avian3d::prelude::RigidBody;
use bevy::{
    mesh::{Indices, Mesh, VertexAttributeValues},
    prelude::*,
};
use bytemuck::Zeroable;

use super::object_lightmaps::ToonMat;
use crate::game::world::objects::PlacedObject;

/// Triangles per leaf before splitting stops.
const LEAF_SIZE: usize = 4;
/// Hard depth cap; the traversal stacks (CPU and WGSL) hold `MAX_DEPTH + 1`
/// entries with room to spare.
const MAX_DEPTH: u32 = 32;
/// Hits closer than this are the ray's own surface, not an occluder.
pub const RAY_EPSILON: f32 = 0.001;

/// One BVH node. Layout matches `BvhNode` in `lightmap_bake.wgsl`: 32 bytes,
/// each `vec3` on a 16-byte boundary.
#[repr(C)]
#[derive(Copy, Clone, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct BvhNode {
    pub aabb_min: [f32; 3],
    /// Interior node (`count == 0`): index of the left child; the right child
    /// is `left + 1`. Leaf (`count > 0`): index of its first triangle.
    pub left_or_first: u32,
    pub aabb_max: [f32; 3],
    /// Number of triangles in a leaf; 0 marks an interior node.
    pub count: u32,
}

/// One world-space triangle. Layout matches `BvhTriangle` in the WGSL:
/// 48 bytes, three `vec3`s each padded to 16.
#[repr(C)]
#[derive(Copy, Clone, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct BvhTriangle {
    pub v0: [f32; 3],
    pub _p0: f32,
    pub v1: [f32; 3],
    pub _p1: f32,
    pub v2: [f32; 3],
    pub _p2: f32,
}

impl From<[Vec3; 3]> for BvhTriangle {
    fn from(t: [Vec3; 3]) -> Self {
        Self {
            v0: t[0].into(),
            _p0: 0.0,
            v1: t[1].into(),
            _p1: 0.0,
            v2: t[2].into(),
            _p2: 0.0,
        }
    }
}

/// A flat, GPU-uploadable triangle BVH. Empty when the scene has no
/// occluders; every query then reports "unoccluded".
#[derive(Debug, Default)]
pub struct TriangleBvh {
    pub nodes: Vec<BvhNode>,
    /// Triangles reordered so each leaf's are contiguous.
    pub tris: Vec<BvhTriangle>,
}

impl TriangleBvh {
    pub fn build(tris: Vec<[Vec3; 3]>) -> Self {
        if tris.is_empty() {
            return Self::default();
        }

        let bounds: Vec<(Vec3, Vec3)> = tris
            .iter()
            .map(|t| (t[0].min(t[1]).min(t[2]), t[0].max(t[1]).max(t[2])))
            .collect();
        let centroids: Vec<Vec3> = bounds.iter().map(|(a, b)| (*a + *b) * 0.5).collect();
        let mut order: Vec<u32> = (0..tris.len() as u32).collect();

        let mut builder = Builder {
            tris: &tris,
            bounds: &bounds,
            centroids: &centroids,
            nodes: Vec::with_capacity(tris.len() / LEAF_SIZE * 2 + 1),
            out: Vec::with_capacity(tris.len()),
        };
        builder.nodes.push(BvhNode::zeroed());
        builder.build_node(0, &mut order, 0);

        Self {
            nodes: builder.nodes,
            tris: builder.out,
        }
    }

    /// Does any triangle block the ray between `RAY_EPSILON` and `max_t`?
    ///
    /// Mirrors `bvh_occluded` in the WGSL exactly, including the slab test
    /// and the guard against zero direction components.
    pub fn occluded(&self, origin: Vec3, dir: Vec3, max_t: f32) -> bool {
        if self.nodes.is_empty() {
            return false;
        }
        let inv_dir = Vec3::new(safe_inv(dir.x), safe_inv(dir.y), safe_inv(dir.z));

        let mut stack = [0u32; 64];
        let mut sp = 1usize;
        while sp > 0 {
            sp -= 1;
            let node = &self.nodes[stack[sp] as usize];
            if !slab_hit(
                node.aabb_min.into(),
                node.aabb_max.into(),
                origin,
                inv_dir,
                max_t,
            ) {
                continue;
            }
            if node.count > 0 {
                let first = node.left_or_first as usize;
                for tri in &self.tris[first..first + node.count as usize] {
                    if let Some(t) =
                        ray_triangle(origin, dir, tri.v0.into(), tri.v1.into(), tri.v2.into())
                        && t < max_t
                    {
                        return true;
                    }
                }
            } else {
                stack[sp] = node.left_or_first;
                stack[sp + 1] = node.left_or_first + 1;
                sp += 2;
            }
        }
        false
    }

    /// Distance to the *closest* triangle the ray hits between `RAY_EPSILON`
    /// and `max_t`, or `None`.
    ///
    /// Same traversal as [`Self::occluded`], but it cannot stop at the first
    /// hit: a leaf visited earlier may hold a farther triangle than one
    /// visited later. Each hit shrinks the bound instead, so subtrees and
    /// triangles beyond the best so far are still culled. Used by the AO
    /// estimate, which weights an occluder by how close it is. Mirrors
    /// `bvh_nearest` in the WGSL.
    pub fn nearest_hit(&self, origin: Vec3, dir: Vec3, max_t: f32) -> Option<f32> {
        if self.nodes.is_empty() {
            return None;
        }
        let inv_dir = Vec3::new(safe_inv(dir.x), safe_inv(dir.y), safe_inv(dir.z));
        let mut best = max_t;
        let mut found = false;

        let mut stack = [0u32; 64];
        let mut sp = 1usize;
        while sp > 0 {
            sp -= 1;
            let node = &self.nodes[stack[sp] as usize];
            if !slab_hit(
                node.aabb_min.into(),
                node.aabb_max.into(),
                origin,
                inv_dir,
                best,
            ) {
                continue;
            }
            if node.count > 0 {
                let first = node.left_or_first as usize;
                for tri in &self.tris[first..first + node.count as usize] {
                    if let Some(t) =
                        ray_triangle(origin, dir, tri.v0.into(), tri.v1.into(), tri.v2.into())
                        && t < best
                    {
                        best = t;
                        found = true;
                    }
                }
            } else {
                stack[sp] = node.left_or_first;
                stack[sp + 1] = node.left_or_first + 1;
                sp += 2;
            }
        }
        found.then_some(best)
    }
}

struct Builder<'a> {
    tris: &'a [[Vec3; 3]],
    bounds: &'a [(Vec3, Vec3)],
    centroids: &'a [Vec3],
    nodes: Vec<BvhNode>,
    out: Vec<BvhTriangle>,
}

impl Builder<'_> {
    fn build_node(&mut self, node_idx: usize, items: &mut [u32], depth: u32) {
        let mut aabb_min = Vec3::splat(f32::MAX);
        let mut aabb_max = Vec3::splat(f32::MIN);
        let mut c_min = Vec3::splat(f32::MAX);
        let mut c_max = Vec3::splat(f32::MIN);
        for &i in items.iter() {
            let (bmin, bmax) = self.bounds[i as usize];
            aabb_min = aabb_min.min(bmin);
            aabb_max = aabb_max.max(bmax);
            c_min = c_min.min(self.centroids[i as usize]);
            c_max = c_max.max(self.centroids[i as usize]);
        }

        let extent = c_max - c_min;
        let axis = if extent.x >= extent.y && extent.x >= extent.z {
            0
        } else if extent.y >= extent.z {
            1
        } else {
            2
        };

        // Leaf: small enough, too deep, or every centroid coincides so a
        // split could not separate anything.
        if items.len() <= LEAF_SIZE || depth >= MAX_DEPTH || extent[axis] <= 0.0 {
            let first = self.out.len() as u32;
            self.out.extend(
                items
                    .iter()
                    .map(|&i| BvhTriangle::from(self.tris[i as usize])),
            );
            self.nodes[node_idx] = BvhNode {
                aabb_min: aabb_min.into(),
                left_or_first: first,
                aabb_max: aabb_max.into(),
                count: items.len() as u32,
            };
            return;
        }

        let mid = items.len() / 2;
        items.select_nth_unstable_by(mid, |&a, &b| {
            self.centroids[a as usize][axis].total_cmp(&self.centroids[b as usize][axis])
        });

        // Children are allocated adjacently so a node only stores `left`.
        let left = self.nodes.len();
        self.nodes.push(BvhNode::zeroed());
        self.nodes.push(BvhNode::zeroed());
        self.nodes[node_idx] = BvhNode {
            aabb_min: aabb_min.into(),
            left_or_first: left as u32,
            aabb_max: aabb_max.into(),
            count: 0,
        };

        let (lo, hi) = items.split_at_mut(mid);
        self.build_node(left, lo, depth + 1);
        self.build_node(left + 1, hi, depth + 1);
    }
}

/// Reciprocal that stays finite for near-zero components, so the slab test
/// never produces `0 * inf = NaN`. Mirrors the WGSL.
#[inline]
fn safe_inv(d: f32) -> f32 {
    if d.abs() < 1e-8 {
        if d < 0.0 { -1e8 } else { 1e8 }
    } else {
        1.0 / d
    }
}

#[inline]
fn slab_hit(aabb_min: Vec3, aabb_max: Vec3, origin: Vec3, inv_dir: Vec3, max_t: f32) -> bool {
    let t0 = (aabb_min - origin) * inv_dir;
    let t1 = (aabb_max - origin) * inv_dir;
    let enter = t0.min(t1).max_element().max(0.0);
    let exit = t0.max(t1).min_element().min(max_t);
    enter <= exit
}

/// Möller–Trumbore. `Some(t)` for a hit at `t > RAY_EPSILON`; both faces hit.
#[inline]
pub fn ray_triangle(origin: Vec3, dir: Vec3, v0: Vec3, v1: Vec3, v2: Vec3) -> Option<f32> {
    let e1 = v1 - v0;
    let e2 = v2 - v0;
    let h = dir.cross(e2);
    let a = e1.dot(h);
    if a.abs() < 1e-6 {
        return None;
    }
    let f = 1.0 / a;
    let s = origin - v0;
    let u = f * s.dot(h);
    if !(0.0..=1.0).contains(&u) {
        return None;
    }
    let q = s.cross(e1);
    let v = f * dir.dot(q);
    if v < 0.0 || u + v > 1.0 {
        return None;
    }
    let t = f * e2.dot(q);
    (t > RAY_EPSILON).then_some(t)
}

// ============================================================================
// Scene gathering
// ============================================================================

/// World-space triangles of every static, toon-shaded mesh whose footprint
/// overlaps `bounds_xz` (`None` = everywhere).
///
/// "Static" means the mesh, or an ancestor, is a placed object or a
/// `RigidBody::Static`. That takes in editor-placed props and plugin-spawned
/// scenery such as the cafe shell (which gets a static body from
/// `AutoCollider`), while leaving out anything that moves — a plugin's
/// animated toon mesh would otherwise bake a shadow wherever it happened to
/// be standing.
pub fn collect_occluder_triangles(
    toon_meshes: &Query<(Entity, &Mesh3d, &GlobalTransform), With<MeshMaterial3d<ToonMat>>>,
    parents: &Query<&ChildOf>,
    scenery: &Query<(Option<&PlacedObject>, Option<&RigidBody>)>,
    meshes: &Assets<Mesh>,
    bounds_xz: Option<(Vec2, Vec2)>,
) -> Vec<[Vec3; 3]> {
    let mut out = Vec::new();

    for (entity, mesh_handle, gt) in toon_meshes.iter() {
        if !is_static_scenery(entity, parents, scenery) {
            continue;
        }
        let Some(mesh) = meshes.get(mesh_handle.id()) else {
            continue;
        };
        let Some(VertexAttributeValues::Float32x3(positions)) =
            mesh.attribute(Mesh::ATTRIBUTE_POSITION)
        else {
            continue;
        };

        let world = gt.to_matrix();
        let world_pos: Vec<Vec3> = positions
            .iter()
            .map(|p| world.transform_point3(Vec3::from(*p)))
            .collect();

        if let Some((bmin, bmax)) = bounds_xz {
            let (mut mn, mut mx) = (Vec2::splat(f32::MAX), Vec2::splat(f32::MIN));
            for p in &world_pos {
                mn = mn.min(p.xz());
                mx = mx.max(p.xz());
            }
            if mx.x < bmin.x || mn.x > bmax.x || mx.y < bmin.y || mn.y > bmax.y {
                continue;
            }
        }

        let push_tri = |out: &mut Vec<[Vec3; 3]>, a: usize, b: usize, c: usize| {
            if let (Some(&pa), Some(&pb), Some(&pc)) =
                (world_pos.get(a), world_pos.get(b), world_pos.get(c))
            {
                out.push([pa, pb, pc]);
            }
        };
        match mesh.indices() {
            Some(Indices::U32(idx)) => {
                for t in idx.as_chunks::<3>().0 {
                    push_tri(&mut out, t[0] as usize, t[1] as usize, t[2] as usize);
                }
            }
            Some(Indices::U16(idx)) => {
                for t in idx.as_chunks::<3>().0 {
                    push_tri(&mut out, t[0] as usize, t[1] as usize, t[2] as usize);
                }
            }
            None => {
                for t in (0..world_pos.len() / 3).map(|i| i * 3) {
                    push_tri(&mut out, t, t + 1, t + 2);
                }
            }
        }
    }

    out
}

fn is_static_scenery(
    entity: Entity,
    parents: &Query<&ChildOf>,
    scenery: &Query<(Option<&PlacedObject>, Option<&RigidBody>)>,
) -> bool {
    let mut current = entity;
    // Bounded walk: hierarchies are shallow and this guards against cycles.
    for _ in 0..64 {
        if let Ok((placed, body)) = scenery.get(current)
            && (placed.is_some() || matches!(body, Some(RigidBody::Static)))
        {
            return true;
        }
        match parents.get(current) {
            Ok(child_of) => current = child_of.parent(),
            Err(_) => return false,
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Tiny deterministic PRNG so the brute-force comparison is repeatable
    /// without pulling test-only features from `rand`.
    struct XorShift(u64);
    impl XorShift {
        fn next_f32(&mut self) -> f32 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            (self.0 >> 40) as f32 / (1u64 << 24) as f32
        }
        fn range(&mut self, lo: f32, hi: f32) -> f32 {
            lo + (hi - lo) * self.next_f32()
        }
        fn vec3(&mut self, lo: f32, hi: f32) -> Vec3 {
            Vec3::new(self.range(lo, hi), self.range(lo, hi), self.range(lo, hi))
        }
    }

    fn random_scene(rng: &mut XorShift, n: usize) -> Vec<[Vec3; 3]> {
        (0..n)
            .map(|_| {
                let c = rng.vec3(-10.0, 10.0);
                [
                    c + rng.vec3(-1.0, 1.0),
                    c + rng.vec3(-1.0, 1.0),
                    c + rng.vec3(-1.0, 1.0),
                ]
            })
            .collect()
    }

    fn brute_force(tris: &[[Vec3; 3]], o: Vec3, d: Vec3, max_t: f32) -> bool {
        tris.iter()
            .any(|t| matches!(ray_triangle(o, d, t[0], t[1], t[2]), Some(h) if h < max_t))
    }

    #[test]
    fn layouts_match_the_wgsl_structs() {
        assert_eq!(std::mem::size_of::<BvhNode>(), 32);
        assert_eq!(std::mem::size_of::<BvhTriangle>(), 48);
        assert_eq!(std::mem::align_of::<BvhNode>(), 4);
        assert_eq!(std::mem::align_of::<BvhTriangle>(), 4);
    }

    #[test]
    fn empty_bvh_never_occludes() {
        let bvh = TriangleBvh::build(Vec::new());
        assert!(bvh.nodes.is_empty() && bvh.tris.is_empty());
        assert!(!bvh.occluded(Vec3::ZERO, Vec3::Y, 100.0));
        assert_eq!(bvh.nearest_hit(Vec3::ZERO, Vec3::Y, 100.0), None);
    }

    /// `nearest_hit` has to return the *closest* triangle, whatever order the
    /// builder laid the leaves out in, and honour `max_t` like `occluded`.
    #[test]
    fn nearest_hit_finds_the_closest_triangle_regardless_of_leaf_order() {
        let slab = |y: f32| {
            [
                Vec3::new(-1.0, y, -1.0),
                Vec3::new(1.0, y, -1.0),
                Vec3::new(0.0, y, 1.0),
            ]
        };
        // Far triangle first in the input, near one last; plus clutter so
        // the tree has more than one leaf.
        let mut tris = vec![slab(8.0), slab(5.0)];
        for i in 0..20 {
            tris.push(slab(20.0 + i as f32));
        }
        tris.push(slab(2.0));
        let bvh = TriangleBvh::build(tris);
        assert!(bvh.nodes.len() > 1);

        let t = bvh.nearest_hit(Vec3::ZERO, Vec3::Y, 100.0).unwrap();
        assert!((t - 2.0).abs() < 1e-4, "nearest is at 2, got {t}");
        assert_eq!(bvh.nearest_hit(Vec3::ZERO, Vec3::Y, 1.5), None);
        assert_eq!(bvh.nearest_hit(Vec3::ZERO, Vec3::NEG_Y, 100.0), None);

        // Agrees with brute force from random origins and directions.
        let mut rng = XorShift(0x1234_5678);
        let scene = random_scene(&mut rng, 300);
        let bvh = TriangleBvh::build(scene.clone());
        for _ in 0..500 {
            let o = rng.vec3(-12.0, 12.0);
            let d = rng.vec3(-1.0, 1.0).normalize_or(Vec3::Y);
            let expect = scene
                .iter()
                .filter_map(|t| ray_triangle(o, d, t[0], t[1], t[2]))
                .filter(|t| *t < 30.0)
                .fold(None, |m: Option<f32>, t| Some(m.map_or(t, |m| m.min(t))));
            let got = bvh.nearest_hit(o, d, 30.0);
            match (expect, got) {
                (None, None) => {}
                (Some(a), Some(b)) => assert!((a - b).abs() < 1e-4, "{a} vs {b}"),
                other => panic!("brute force vs bvh disagree: {other:?}"),
            }
        }
    }

    #[test]
    fn single_triangle_hit_miss_and_range() {
        // Horizontal triangle at y = 2 covering the origin's column.
        let tri = [
            Vec3::new(-1.0, 2.0, -1.0),
            Vec3::new(1.0, 2.0, -1.0),
            Vec3::new(0.0, 2.0, 1.0),
        ];
        let bvh = TriangleBvh::build(vec![tri]);
        assert_eq!(bvh.nodes.len(), 1);

        assert!(bvh.occluded(Vec3::ZERO, Vec3::Y, 10.0), "straight up hits");
        assert!(!bvh.occluded(Vec3::ZERO, Vec3::NEG_Y, 10.0), "away misses");
        assert!(
            !bvh.occluded(Vec3::ZERO, Vec3::Y, 1.5),
            "beyond max_t ignored"
        );
        assert!(
            bvh.occluded(Vec3::ZERO, Vec3::Y, 2.5),
            "just past the hit counts"
        );
        // Both faces occlude: hitting the underside from above counts too.
        assert!(bvh.occluded(Vec3::new(0.0, 5.0, 0.0), Vec3::NEG_Y, 10.0));
    }

    /// A ray starting *on* a surface must not report that surface as an
    /// occluder — that is what RAY_EPSILON is for, and what lets the object
    /// being baked stay in the BVH.
    #[test]
    fn a_surface_does_not_shadow_itself() {
        let tri = [
            Vec3::new(-1.0, 0.0, -1.0),
            Vec3::new(1.0, 0.0, -1.0),
            Vec3::new(0.0, 0.0, 1.0),
        ];
        let bvh = TriangleBvh::build(vec![tri]);
        assert!(!bvh.occluded(Vec3::ZERO, Vec3::Y, 10.0));
        // Slightly above (the face-normal offset bakers use) is still clear.
        assert!(!bvh.occluded(Vec3::new(0.0, 0.02, 0.0), Vec3::Y, 10.0));
    }

    /// Axis-aligned rays have zero direction components; the reciprocal guard
    /// must keep the slab test correct instead of producing NaN.
    #[test]
    fn axis_aligned_rays_traverse_correctly() {
        let mut rng = XorShift(0x9E37_79B9_7F4A_7C15);
        let tris = random_scene(&mut rng, 200);
        let bvh = TriangleBvh::build(tris.clone());
        for _ in 0..300 {
            let o = rng.vec3(-12.0, 12.0);
            for d in [
                Vec3::X,
                Vec3::Y,
                Vec3::Z,
                Vec3::NEG_X,
                Vec3::NEG_Y,
                Vec3::NEG_Z,
            ] {
                let max_t = rng.range(0.5, 30.0);
                assert_eq!(bvh.occluded(o, d, max_t), brute_force(&tris, o, d, max_t));
            }
        }
    }

    #[test]
    fn matches_brute_force_on_random_rays() {
        let mut rng = XorShift(0xDEAD_BEEF_CAFE_F00D);
        let tris = random_scene(&mut rng, 400);
        let bvh = TriangleBvh::build(tris.clone());

        // Every input triangle is in exactly one leaf.
        assert_eq!(bvh.tris.len(), tris.len());
        assert!(bvh.nodes.len() > 1, "400 triangles must split");
        let leaf_total: u32 = bvh.nodes.iter().map(|n| n.count).sum();
        assert_eq!(leaf_total as usize, tris.len());

        let mut hits = 0;
        for _ in 0..3000 {
            let o = rng.vec3(-12.0, 12.0);
            let d = rng.vec3(-1.0, 1.0).normalize_or(Vec3::Y);
            let max_t = rng.range(0.1, 40.0);
            let expected = brute_force(&tris, o, d, max_t);
            hits += expected as u32;
            assert_eq!(
                bvh.occluded(o, d, max_t),
                expected,
                "o={o} d={d} max_t={max_t}"
            );
        }
        assert!(
            hits > 100,
            "test scene too sparse to be meaningful ({hits} hits)"
        );
    }

    /// Many coincident triangles can't be separated by a median split; the
    /// builder must fall back to a leaf rather than recurse forever.
    #[test]
    fn coincident_triangles_terminate_in_one_leaf() {
        let tri = [Vec3::ZERO, Vec3::X, Vec3::Y];
        let bvh = TriangleBvh::build(vec![tri; 100]);
        assert!(bvh.nodes.iter().any(|n| n.count == 100));
        assert!(bvh.occluded(Vec3::new(0.2, 0.2, -1.0), Vec3::Z, 5.0));
    }
}
