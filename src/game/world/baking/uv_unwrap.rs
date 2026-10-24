//! Lightmap UV2 generation for placed meshes.
//!
//! Every triangle gets its own cell in a square grid atlas. That is wasteful
//! — a triangle fills half its cell, and cells are uniform regardless of
//! triangle size — but it is *guaranteed* non-overlapping, which is the one
//! property a lightmap UV set cannot do without.
//!
//! Cell *size* is budgeted against world-space triangle size
//! ([`texels_per_cell`] × [`max_world_edge`]), not against triangle count
//! alone. Count alone is a trap: the cafe floor is one 20 m × 12 m quad, so
//! two triangles, and a count-driven atlas gave it a 32 px texture — about
//! half a texel per metre, against the terrain's sixteen. Bilinear-filtered
//! up to that size and then quantised by the 5-band toon shader, every
//! shadow boundary came out as a staircase that broke at the quad's diagonal.
//!
//! The previous triplanar projection was not: it bucketed vertices by their
//! dominant normal axis ignoring sign, so a cube's top and bottom faces
//! projected to identical XZ coordinates and shared texels. Whichever
//! triangle rasterized first won, and the other face wore its lighting —
//! an unlit underside stamped onto a sunlit top. Repeated features collided
//! the same way (all four legs of a chair map onto one region). The result
//! was blotches of wrong, often black, lighting.
//!
//! Because each triangle needs three UVs that no other triangle shares, the
//! mesh is de-indexed first ([`Mesh::duplicate_vertices`]); sequential
//! indices are put back so everything downstream that expects an indexed
//! mesh (collider generation, the bake rasteriser) keeps working.

use bevy::{
    asset::AssetId,
    mesh::{Indices, Mesh, PrimitiveTopology},
    prelude::*,
};
use std::collections::HashMap;

/// Remembers which mesh assets already have lightmap UVs, and how many
/// triangles each has (which sets its atlas resolution).
///
/// Unwrapping mutates the shared `Mesh` asset, so it must happen exactly once
/// per asset no matter how many placed objects instance it — and the bake and
/// the load path must agree on the result, since the UVs they use have to be
/// the same ones.
#[derive(Resource, Default)]
pub struct LightmapUvCache(HashMap<AssetId<Mesh>, u32>);

impl LightmapUvCache {
    /// Lightmap UVs for `id`, generating them on first use. Returns the
    /// mesh's triangle count, or `None` if it can't be unwrapped.
    pub fn ensure(&mut self, meshes: &mut Assets<Mesh>, id: AssetId<Mesh>) -> Option<u32> {
        if let Some(&tri_count) = self.0.get(&id) {
            return Some(tri_count);
        }
        let tri_count = generate_lightmap_uvs(meshes.get_mut(id)?)?;
        self.0.insert(id, tri_count);
        Some(tri_count)
    }
}

/// Fraction of a cell's width left empty around each triangle, so bilinear
/// filtering at a triangle's edge cannot reach into a neighbouring cell.
/// Dilation then floods lit values outward into this margin.
const GUTTER_FRAC: f32 = 0.18;

/// Where a triangle's three corners sit inside its cell, as a fraction of the
/// inset cell size. A right triangle in the lower-left half.
const CORNERS: [(f32, f32); 3] = [(0.0, 0.0), (1.0, 0.0), (0.0, 1.0)];

/// Atlas side length, in texels, for a mesh of `tri_count` triangles.
///
/// Sized so each cell is about `texels_per_cell` across, rounded up to a
/// power of two and clamped to `max_res`.
pub fn lightmap_resolution(tri_count: u32, texels_per_cell: u32, max_res: u32) -> u32 {
    let grid = grid_side(tri_count);
    let wanted = grid.saturating_mul(texels_per_cell.max(1));
    wanted.next_power_of_two().clamp(32, max_res.max(32))
}

/// How many texels across a cell needs to be, for a mesh whose largest
/// triangle spans `max_edge` metres in world space.
///
/// Cells are uniform, so this is driven by the *largest* triangle: anything
/// smaller gets the same budget and some of it goes unused. That waste is the
/// price of the uniform grid, and it is why a cuboid wall (two big faces plus
/// ten slivers along its thickness) resolves more coarsely than a flat quad
/// of the same size.
///
/// `min_texels` is a floor for dense meshes, where per-triangle world extent
/// is tiny and the triangle count already drives the atlas size.
pub fn texels_per_cell(max_edge: f32, texels_per_metre: f32, min_texels: u32) -> u32 {
    if !max_edge.is_finite() || max_edge <= 0.0 || texels_per_metre <= 0.0 {
        return min_texels.max(1);
    }
    let wanted = (max_edge * texels_per_metre).ceil();
    // The atlas is capped anyway; this just keeps the cast well-defined.
    let wanted = wanted.clamp(1.0, u16::MAX as f32) as u32;
    wanted.max(min_texels.max(1))
}

/// Longest triangle edge in `mesh`, in world space under `world_from_local`.
///
/// World space, not local: a glTF node's own scale is part of how big the
/// triangle actually is (the cafe floor is a 2 m quad scaled to 20 m × 12 m),
/// and texel density has to be budgeted against the size you can see.
pub fn max_world_edge(mesh: &Mesh, world_from_local: &Mat4) -> f32 {
    use bevy::mesh::VertexAttributeValues;

    let Some(VertexAttributeValues::Float32x3(positions)) =
        mesh.attribute(Mesh::ATTRIBUTE_POSITION)
    else {
        return 0.0;
    };
    let Some(indices) = mesh.indices() else {
        return 0.0;
    };

    let world = |i: u32| -> Vec3 {
        let p = positions[i as usize];
        world_from_local.transform_point3(Vec3::from(p))
    };

    let mut max_edge = 0.0f32;
    let mut consider = |a: u32, b: u32, c: u32| {
        let (wa, wb, wc) = (world(a), world(b), world(c));
        max_edge = max_edge
            .max(wa.distance(wb))
            .max(wb.distance(wc))
            .max(wc.distance(wa));
    };

    match indices {
        Indices::U32(v) => {
            for t in v.chunks_exact(3) {
                consider(t[0], t[1], t[2]);
            }
        }
        Indices::U16(v) => {
            for t in v.chunks_exact(3) {
                consider(t[0] as u32, t[1] as u32, t[2] as u32);
            }
        }
    }
    max_edge
}

/// Dilation passes needed to flood lit values across the whole gutter of an
/// atlas of side `res` holding `tri_count` triangles.
///
/// Derived rather than configured: the gutter is a *fraction* of the cell, so
/// it grows with resolution. A fixed pass count that covered the gutter at
/// 8 texels/cell leaves a black rim at 256 texels/cell, which bilinear
/// filtering then smears across every triangle edge.
pub fn dilate_passes_for_atlas(res: u32, tri_count: u32) -> u32 {
    // +1 so the flood reaches a texel *past* the gutter, covering the
    // half-texel a bilinear tap can reach beyond the cell boundary.
    (cell_texels(res, tri_count) as f32 * GUTTER_FRAC).ceil() as u32 + 1
}

/// Width in texels of one triangle's cell, in an atlas of side `res` holding
/// `tri_count` triangles.
pub fn cell_texels(res: u32, tri_count: u32) -> u32 {
    (res / grid_side(tri_count).max(1)).max(1)
}

/// Number of cells per atlas side for `tri_count` triangles.
fn grid_side(tri_count: u32) -> u32 {
    (tri_count as f64).sqrt().ceil().max(1.0) as u32
}

/// Generate lightmap UV2 for `mesh`, overwriting any existing
/// [`Mesh::ATTRIBUTE_UV_1`].
///
/// Existing UV1 is deliberately *not* trusted: in glTF it is a second texture
/// channel, which is routinely tiled or overlapping and so unusable as a
/// lightmap parameterisation. Nothing else in this engine reads UV1 — base
/// colour sampling uses UV0 — so overwriting it is safe.
///
/// Returns the triangle count, or `None` if the mesh can't be unwrapped.
pub fn generate_lightmap_uvs(mesh: &mut Mesh) -> Option<u32> {
    if mesh.primitive_topology() != PrimitiveTopology::TriangleList {
        warn!("generate_lightmap_uvs: mesh is not a TriangleList, skipping");
        return None;
    }

    if mesh.attribute(Mesh::ATTRIBUTE_POSITION).is_none() {
        warn!("generate_lightmap_uvs: mesh has no ATTRIBUTE_POSITION, skipping");
        return None;
    }

    // One triangle per three vertices, so each gets private UVs. No-op when
    // the mesh is already unindexed.
    mesh.duplicate_vertices();

    let vertex_count = mesh.count_vertices();
    if vertex_count == 0 || !vertex_count.is_multiple_of(3) {
        warn!(
            "generate_lightmap_uvs: {vertex_count} vertices is not a whole number of triangles, skipping"
        );
        return None;
    }
    let tri_count = (vertex_count / 3) as u32;

    let grid = grid_side(tri_count);
    let cell = 1.0 / grid as f32;
    let inset = cell * (1.0 - 2.0 * GUTTER_FRAC);
    let margin = cell * GUTTER_FRAC;

    let mut uv1s = Vec::with_capacity(vertex_count);
    for tri in 0..tri_count {
        let col = (tri % grid) as f32;
        let row = (tri / grid) as f32;
        let origin_u = col * cell + margin;
        let origin_v = row * cell + margin;
        for (cu, cv) in CORNERS {
            uv1s.push([origin_u + cu * inset, origin_v + cv * inset]);
        }
    }

    // bevy's pbr_fragment.wgsl assumes VERTEX_UVS_A exists whenever any UV
    // set does, so a mesh with only UV1 fails to compile. Supply a dummy UV0.
    if mesh.attribute(Mesh::ATTRIBUTE_UV_0).is_none() {
        mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, vec![[0.0f32, 0.0f32]; vertex_count]);
    }

    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_1, uv1s);

    // Restore indices so downstream consumers that require an indexed mesh
    // (avian's trimesh collider, the bake rasteriser) keep working.
    mesh.insert_indices(Indices::U32((0..vertex_count as u32).collect()));

    Some(tri_count)
}

/// Read back the UV1 set as `[f32; 2]`, if present and in the expected format.
#[cfg(test)]
fn uv1_of(mesh: &Mesh) -> Option<Vec<[f32; 2]>> {
    use bevy::mesh::VertexAttributeValues;
    match mesh.attribute(Mesh::ATTRIBUTE_UV_1) {
        Some(VertexAttributeValues::Float32x2(v)) => Some(v.clone()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::asset::RenderAssetUsages;

    /// Two triangles sharing an edge, with an existing (overlapping) UV1 that
    /// must be replaced. Shared vertices force de-indexing.
    fn quad() -> Mesh {
        let mut mesh = Mesh::new(
            PrimitiveTopology::TriangleList,
            RenderAssetUsages::default(),
        );
        mesh.insert_attribute(
            Mesh::ATTRIBUTE_POSITION,
            vec![
                [0.0, 0.0, 0.0],
                [1.0, 0.0, 0.0],
                [0.0, 0.0, 1.0],
                [1.0, 0.0, 1.0],
            ],
        );
        mesh.insert_attribute(Mesh::ATTRIBUTE_UV_1, vec![[0.5f32, 0.5]; 4]);
        mesh.insert_indices(Indices::U32(vec![0, 1, 2, 1, 3, 2]));
        mesh
    }

    /// Area of the UV triangle for triangle `t`.
    fn uv_area(uvs: &[[f32; 2]], t: usize) -> f32 {
        let (a, b, c) = (uvs[t * 3], uvs[t * 3 + 1], uvs[t * 3 + 2]);
        ((b[0] - a[0]) * (c[1] - a[1]) - (c[0] - a[0]) * (b[1] - a[1])).abs() * 0.5
    }

    /// Axis-aligned bounds of triangle `t`'s UVs.
    fn uv_bounds(uvs: &[[f32; 2]], t: usize) -> (f32, f32, f32, f32) {
        let tri = &uvs[t * 3..t * 3 + 3];
        (
            tri.iter().map(|p| p[0]).fold(f32::MAX, f32::min),
            tri.iter().map(|p| p[0]).fold(f32::MIN, f32::max),
            tri.iter().map(|p| p[1]).fold(f32::MAX, f32::min),
            tri.iter().map(|p| p[1]).fold(f32::MIN, f32::max),
        )
    }

    #[test]
    fn deindexes_and_overwrites_existing_uv1() {
        let mut mesh = quad();
        assert_eq!(generate_lightmap_uvs(&mut mesh), Some(2));

        // Shared vertices were split: 2 triangles → 6 vertices.
        assert_eq!(mesh.count_vertices(), 6);
        let uvs = uv1_of(&mesh).unwrap();
        assert_eq!(uvs.len(), 6);
        assert!(
            uvs.iter().all(|uv| *uv != [0.5, 0.5]),
            "the pre-existing overlapping UV1 must be replaced"
        );
        // Indices restored and sequential, so indexed consumers still work.
        match mesh.indices() {
            Some(Indices::U32(i)) => assert_eq!(*i, vec![0, 1, 2, 3, 4, 5]),
            other => panic!("expected sequential U32 indices, got {other:?}"),
        }
        // A dummy UV0 is required or the pbr shader fails to compile.
        assert!(mesh.attribute(Mesh::ATTRIBUTE_UV_0).is_some());
    }

    /// The regression that motivated replacing triplanar projection: no two
    /// triangles may share texels. Opposite faces of a cube are the case that
    /// used to collide, so test a real closed box.
    #[test]
    fn no_two_triangles_overlap_in_uv_space() {
        let mut mesh = Mesh::from(bevy::math::primitives::Cuboid::new(2.0, 2.0, 2.0));
        let tri_count = generate_lightmap_uvs(&mut mesh).unwrap() as usize;
        assert_eq!(tri_count, 12, "a cuboid is 12 triangles");
        let uvs = uv1_of(&mesh).unwrap();

        for t in 0..tri_count {
            let a = uv_bounds(&uvs, t);
            assert!(
                a.0 >= 0.0 && a.1 <= 1.0 && a.2 >= 0.0 && a.3 <= 1.0,
                "triangle {t} escapes the atlas: {a:?}"
            );
            assert!(uv_area(&uvs, t) > 0.0, "triangle {t} is degenerate in UV");

            for u in (t + 1)..tri_count {
                let b = uv_bounds(&uvs, u);
                let disjoint = a.1 <= b.0 || b.1 <= a.0 || a.3 <= b.2 || b.3 <= a.2;
                assert!(disjoint, "triangles {t} and {u} overlap: {a:?} vs {b:?}");
            }
        }
    }

    #[test]
    fn triangles_are_separated_by_a_gutter() {
        let mut mesh = Mesh::from(bevy::math::primitives::Cuboid::default());
        let tri_count = generate_lightmap_uvs(&mut mesh).unwrap();
        let uvs = uv1_of(&mesh).unwrap();
        let cell = 1.0 / grid_side(tri_count) as f32;

        // Every triangle sits at least a gutter's width inside its own cell,
        // so filtering at its edge cannot reach the neighbouring cell.
        for t in 0..tri_count as usize {
            let (min_u, max_u, min_v, max_v) = uv_bounds(&uvs, t);
            let cell_u = (min_u / cell).floor();
            let cell_v = (min_v / cell).floor();
            let gutter = cell * GUTTER_FRAC;
            assert!(min_u >= cell_u * cell + gutter - 1e-5, "triangle {t} left");
            assert!(
                min_v >= cell_v * cell + gutter - 1e-5,
                "triangle {t} bottom"
            );
            assert!(
                max_u <= (cell_u + 1.0) * cell - gutter + 1e-5,
                "triangle {t} right"
            );
            assert!(
                max_v <= (cell_v + 1.0) * cell - gutter + 1e-5,
                "triangle {t} top"
            );
        }
    }

    /// The cafe floor: one 20 m × 12 m quad, so two triangles. Sizing by
    /// triangle count alone gave it a 32 px atlas — ~0.5 texels/m, which is
    /// what turned furniture shadows into a staircase. It must now be
    /// budgeted by world size instead.
    #[test]
    fn a_large_flat_quad_gets_a_usable_texel_density() {
        let mut mesh = quad();
        let tri_count = generate_lightmap_uvs(&mut mesh).unwrap();
        // The unit quad scaled to the cafe floor's footprint.
        let world = Mat4::from_scale(Vec3::new(20.0, 1.0, 12.0));

        let longest = max_world_edge(&mesh, &world);
        assert!(
            (longest - (20.0f32.powi(2) + 12.0f32.powi(2)).sqrt()).abs() < 1e-3,
            "longest edge should be the quad's diagonal, got {longest}"
        );

        let cell = texels_per_cell(longest, 8.0, 8);
        let res = lightmap_resolution(tri_count, cell, 512);

        // Old behaviour, for contrast: count-driven sizing bottomed out at the
        // 32 px floor.
        assert_eq!(lightmap_resolution(tri_count, 8, 512), 32);

        let usable = (res / grid_side(tri_count)) as f32 * (1.0 - 2.0 * GUTTER_FRAC);
        let texels_per_metre = usable / longest;
        assert!(
            texels_per_metre > 4.0,
            "{res}px atlas gives only {texels_per_metre:.2} texels/m across a {longest:.1} m \
             triangle — shadow edges will still band"
        );
    }

    /// A dense mesh is already well served by count-driven sizing; world-space
    /// budgeting must not shrink it below that floor.
    #[test]
    fn dense_small_meshes_fall_back_to_the_per_triangle_floor() {
        // 2000 triangles spanning a 0.5 m prop: ~0.02 m per edge.
        assert_eq!(texels_per_cell(0.02, 8.0, 8), 8);
        // Degenerate inputs must not produce a zero-sized cell.
        assert_eq!(texels_per_cell(0.0, 8.0, 8), 8);
        assert_eq!(texels_per_cell(f32::NAN, 8.0, 8), 8);
        assert_eq!(texels_per_cell(10.0, 0.0, 8), 8);
    }

    /// The gutter is a fraction of the cell, so a fixed pass count that
    /// covered it at 8 texels/cell leaves a black rim once cells get big.
    #[test]
    fn dilation_covers_the_gutter_at_every_resolution() {
        for (res, tri_count) in [(32u32, 2u32), (512, 2), (512, 12), (256, 1000)] {
            let passes = dilate_passes_for_atlas(res, tri_count);
            let cell = res / grid_side(tri_count);
            let gutter = cell as f32 * GUTTER_FRAC;
            assert!(
                passes as f32 >= gutter,
                "{res}px atlas, {tri_count} tris: {passes} passes cannot fill a \
                 {gutter:.1}-texel gutter"
            );
        }
    }

    #[test]
    fn resolution_scales_with_triangle_count_and_clamps() {
        // 12 triangles → 4×4 grid → 4*8 = 32.
        assert_eq!(lightmap_resolution(12, 8, 512), 32);
        // 1000 triangles → 32×32 grid → 256.
        assert_eq!(lightmap_resolution(1000, 8, 512), 256);
        // Dense meshes clamp rather than allocating unbounded textures.
        assert_eq!(lightmap_resolution(100_000, 8, 512), 512);
        // Always a usable floor.
        assert_eq!(lightmap_resolution(1, 8, 512), 32);
    }

    #[test]
    fn rejects_meshes_it_cannot_unwrap() {
        let mut no_positions = Mesh::new(
            PrimitiveTopology::TriangleList,
            RenderAssetUsages::default(),
        );
        assert_eq!(generate_lightmap_uvs(&mut no_positions), None);

        let mut strip = Mesh::new(
            PrimitiveTopology::TriangleStrip,
            RenderAssetUsages::default(),
        );
        strip.insert_attribute(Mesh::ATTRIBUTE_POSITION, vec![[0.0f32, 0.0, 0.0]; 4]);
        assert_eq!(generate_lightmap_uvs(&mut strip), None);

        // Unindexed and not a whole number of triangles.
        let mut partial = Mesh::new(
            PrimitiveTopology::TriangleList,
            RenderAssetUsages::default(),
        );
        partial.insert_attribute(Mesh::ATTRIBUTE_POSITION, vec![[0.0f32, 0.0, 0.0]; 4]);
        assert_eq!(generate_lightmap_uvs(&mut partial), None);
    }
}
