//! Per-sub-mesh baked lightmaps for placed objects: file naming, the shared
//! subtree walk that numbers sub-meshes, and the system that applies saved
//! lightmaps once an object's scene has been converted to toon materials.
//!
//! Why per sub-mesh rather than one atlas per object: lightmap UVs live on
//! the shared `Mesh` asset, and a glTF routinely instances one mesh many
//! times — every wall of the cafe shell is the same unit cube. In a single
//! per-object atlas all those instances would land on the same texels, the
//! first one baked would win, and the rest would wear its shadows. So each
//! mesh *entity* under a placed object gets its own lightmap, keyed by the
//! object id plus the entity's index in [`collect_toon_meshes`]'s walk.
//! Re-bake after reordering nodes in the source asset.

use bevy::{pbr::ExtendedMaterial, prelude::*};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

use super::lightmap::lightmap_file_name;
use crate::game::gfx::toon_material::ToonMaterial;

pub type ToonMat = ExtendedMaterial<StandardMaterial, ToonMaterial>;

/// Path of one sub-mesh's lightmap inside a world directory.
pub fn object_lightmap_path(world_path: &Path, object_id: u64, sub_mesh: usize) -> PathBuf {
    world_path
        .join("objects")
        .join(lightmap_file_name(&format!("obj_{object_id}_{sub_mesh}")))
}

/// One toon-shaded mesh entity under a placed object.
pub struct ToonMeshEntry {
    pub entity: Entity,
    pub mesh: Handle<Mesh>,
    pub material: Handle<ToonMat>,
    /// The sub-mesh's *own* world transform. glTF nodes routinely carry
    /// their own scale/offset (the rock mesh is scaled 2.5× inside its file),
    /// so the placed root's transform alone puts texels in the wrong place —
    /// and once an object shadows itself, "wrong place" means "inside the
    /// mesh", which bakes solid black.
    pub transform: GlobalTransform,
}

/// Query shape shared by the baker and the loader for walking an object's
/// toon-shaded meshes.
pub type ToonMeshQuery<'w, 's> = Query<
    'w,
    's,
    (
        &'static Mesh3d,
        &'static MeshMaterial3d<ToonMat>,
        &'static GlobalTransform,
    ),
>;

/// Pre-order walk of `root`'s subtree collecting every entity that has both a
/// mesh and a toon material. The index into the result is the sub-mesh id in
/// lightmap file names, so the baker and the loader must both go through
/// this function.
pub fn collect_toon_meshes(
    root: Entity,
    mesh_query: &ToonMeshQuery,
    children_query: &Query<&Children>,
) -> Vec<ToonMeshEntry> {
    let mut out = Vec::new();
    collect_recursive(root, mesh_query, children_query, &mut out);
    out
}

fn collect_recursive(
    entity: Entity,
    mesh_query: &ToonMeshQuery,
    children_query: &Query<&Children>,
    out: &mut Vec<ToonMeshEntry>,
) {
    if let Ok((mesh, material, transform)) = mesh_query.get(entity) {
        out.push(ToonMeshEntry {
            entity,
            mesh: mesh.0.clone(),
            material: material.0.clone(),
            transform: *transform,
        });
    }
    if let Ok(children) = children_query.get(entity) {
        for child in children.iter() {
            collect_recursive(child, mesh_query, children_query, out);
        }
    }
}

/// A material handle unique to `entry.entity` among the entries processed
/// with the same `seen` set.
///
/// Scene converters may give several mesh entities one material (the cafe
/// plugin shares one per named node). The lightmap is a material property,
/// so such entities get a private clone before it is set; the clone is
/// attached via `commands` and returned. `None` if the material asset is
/// missing.
pub fn unique_material(
    entry: &ToonMeshEntry,
    seen: &mut HashSet<AssetId<ToonMat>>,
    materials: &mut Assets<ToonMat>,
    commands: &mut Commands,
) -> Option<Handle<ToonMat>> {
    if seen.insert(entry.material.id()) {
        return Some(entry.material.clone());
    }
    let clone = materials.get(entry.material.id())?.clone();
    let handle = materials.add(clone);
    seen.insert(handle.id());
    commands
        .entity(entry.entity)
        .insert(MeshMaterial3d(handle.clone()));
    Some(handle)
}

/// Spawned on every placed mesh object. [`apply_object_lightmaps`] removes it
/// once the object's toon meshes exist and its saved lightmaps have been
/// applied (or found absent).
#[derive(Component)]
pub struct NeedsObjectLightmaps;

/// Apply saved lightmaps to placed objects whose scenes have finished loading
/// and been converted to toon materials.
///
/// Runs every frame but only touches entities still carrying
/// [`NeedsObjectLightmaps`]. An object whose subtree has no toon meshes yet
/// (scene still loading, or converted by a plugin observer next frame) is
/// simply retried. Works for any converter — the host's `ConvertToToon` path
/// and plugin scene hooks alike — because it keys off the materials that end
/// up on the entities, not on who put them there.
#[cfg(not(target_arch = "wasm32"))]
#[allow(clippy::too_many_arguments)]
pub fn apply_object_lightmaps(
    mut commands: Commands,
    terrain_world: Option<Res<crate::game::world::terrain::storage::TerrainWorld>>,
    pending: Query<
        (Entity, &crate::game::world::objects::PlacedObject),
        With<NeedsObjectLightmaps>,
    >,
    mesh_query: ToonMeshQuery,
    children_query: Query<&Children>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<ToonMat>>,
    mut images: ResMut<Assets<Image>>,
    mut uv_cache: ResMut<super::uv_unwrap::LightmapUvCache>,
) {
    use super::lightmap::try_load_lightmap_image;

    let Some(world) = terrain_world else {
        return;
    };

    for (root, placed) in &pending {
        let entries = collect_toon_meshes(root, &mesh_query, &children_query);
        if entries.is_empty() {
            continue;
        }
        commands.entity(root).remove::<NeedsObjectLightmaps>();

        // Objects are baked all-or-nothing, so the first sub-mesh tells us
        // whether this object has a bake at all.
        if !object_lightmap_path(&world.world_path, placed.id, 0).exists() {
            continue;
        }

        let mut seen = HashSet::new();
        let mut applied = 0usize;
        for (k, entry) in entries.iter().enumerate() {
            // Claim a private material for every entry, lightmap or not, so
            // a later entry can never set its lightmap on a material an
            // earlier, unlit entry is still showing.
            let Some(mat_handle) = unique_material(entry, &mut seen, &mut materials, &mut commands)
            else {
                continue;
            };
            let path = object_lightmap_path(&world.world_path, placed.id, k);
            let Some(image) = try_load_lightmap_image(&path, &mut images) else {
                continue;
            };
            // The same unwrap the bake used — the UVs must match the texels.
            uv_cache.ensure(&mut meshes, entry.mesh.id());
            if let Some(mat) = materials.get_mut(mat_handle.id()) {
                mat.extension.lightmap = image;
            }
            applied += 1;
        }
        debug!(
            "Applied {applied}/{} saved lightmaps to placed object id={}",
            entries.len(),
            placed.id
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::world::objects::{PlacedKind, PlacedObject};
    use bevy::ecs::system::SystemState;

    /// The regression behind "rocks bake solid black": sub-meshes must report
    /// their *own* world transform, not the placed root's. A glTF node with
    /// its own scale/offset is the norm, and once an object shadows itself a
    /// mis-placed texel sits inside the real mesh and every ray is occluded.
    #[test]
    fn entries_carry_each_sub_meshs_own_transform() {
        let mut world = World::new();

        let root_tf = GlobalTransform::from(Transform::from_xyz(10.0, 0.0, 5.0));
        let child_tf = GlobalTransform::from(
            Transform::from_xyz(10.0, 0.0, 5.0).with_scale(Vec3::splat(2.529)),
        );
        let grandchild_tf = GlobalTransform::from(Transform::from_xyz(13.0, 2.0, 7.0));

        let root = world
            .spawn((
                PlacedObject {
                    id: 7,
                    chunk: (0, 0),
                    name: String::new(),
                    kind: PlacedKind::mesh("x.gltf"),
                },
                root_tf,
            ))
            .id();
        let child = world
            .spawn((
                Mesh3d(Handle::default()),
                MeshMaterial3d::<ToonMat>(Handle::default()),
                child_tf,
                ChildOf(root),
            ))
            .id();
        let grandchild = world
            .spawn((
                Mesh3d(Handle::default()),
                MeshMaterial3d::<ToonMat>(Handle::default()),
                grandchild_tf,
                ChildOf(child),
            ))
            .id();
        // A non-toon sibling is not a sub-mesh and must not take an index.
        world.spawn((Mesh3d(Handle::default()), root_tf, ChildOf(root)));

        let mut state: SystemState<(ToonMeshQuery, Query<&Children>)> =
            SystemState::new(&mut world);
        let (meshes, children) = state.get(&world);
        let entries = collect_toon_meshes(root, &meshes, &children);

        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].entity, child);
        assert_eq!(entries[0].transform, child_tf);
        assert_ne!(
            entries[0].transform, root_tf,
            "must not be the root's transform"
        );
        assert_eq!(entries[1].entity, grandchild);
        assert_eq!(entries[1].transform, grandchild_tf);
    }
}
