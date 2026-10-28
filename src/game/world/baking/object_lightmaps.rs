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

use super::lightmap::{LightmapLocation, LightmapSource, lightmap_file_name};
use crate::game::gfx::toon_material::ToonMaterial;

pub type ToonMat = ExtendedMaterial<StandardMaterial, ToonMaterial>;

/// Path of one sub-mesh's lightmap inside a world directory.
pub fn object_lightmap_path(world_path: &Path, object_id: u64, sub_mesh: usize) -> PathBuf {
    world_path
        .join("objects")
        .join(lightmap_file_name(&format!("obj_{object_id}_{sub_mesh}")))
}

/// OPFS key of one sub-mesh's lightmap. See [`super::lightmap::terrain_lightmap_key`],
/// including why this is `allow(dead_code)` on native.
#[allow(dead_code)]
pub fn object_lightmap_key(world_id: &str, object_id: u64, sub_mesh: usize) -> String {
    format!(
        "worlds/{world_id}/objects/{}",
        lightmap_file_name(&format!("obj_{object_id}_{sub_mesh}"))
    )
}

/// Storage location of one sub-mesh's lightmap, so call sites don't need
/// their own `#[cfg]` to pick between a path and an OPFS key.
pub fn object_lightmap_location(
    world: &crate::game::world::terrain::storage::TerrainWorld,
    object_id: u64,
    sub_mesh: usize,
) -> LightmapLocation {
    #[cfg(not(target_arch = "wasm32"))]
    {
        object_lightmap_path(&world.world_path, object_id, sub_mesh)
    }
    #[cfg(target_arch = "wasm32")]
    {
        object_lightmap_key(&world.metadata.world_id, object_id, sub_mesh)
    }
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

/// Bundle of ECS handles the apply step mutates. Only exists to keep the
/// shared helper's parameter list from doubling as a riddle.
struct ApplyCtx<'a, 'w, 's> {
    commands: &'a mut Commands<'w, 's>,
    meshes: &'a mut Assets<Mesh>,
    materials: &'a mut Assets<ToonMat>,
    images: &'a mut Assets<Image>,
    uv_cache: &'a mut super::uv_unwrap::LightmapUvCache,
}

/// Bind one lightmap per sub-mesh onto a placed object's toon materials.
///
/// `sources[k]` is the k-th sub-mesh's saved lightmap, or `None` if it has
/// none — a path natively, bytes already fetched from OPFS on wasm. Shared by
/// both platforms so the material-claiming rules below can't drift apart.
/// Returns how many were applied.
fn apply_lightmap_sources(
    entries: &[ToonMeshEntry],
    sources: &[Option<LightmapSource>],
    ctx: &mut ApplyCtx,
) -> usize {
    use super::lightmap::try_load_lightmap_image;

    // Objects are baked all-or-nothing, so the first sub-mesh tells us
    // whether this object has a bake at all. Checked before anything is
    // claimed below, so an unbaked object costs no cloned materials.
    if sources.first().is_none_or(Option::is_none) {
        return 0;
    }

    let mut seen = HashSet::new();
    let mut applied = 0usize;
    for (k, entry) in entries.iter().enumerate() {
        // Claim a private material for every entry, lightmap or not, so a
        // later entry can never set its lightmap on a material an earlier,
        // unlit entry is still showing.
        let Some(mat_handle) = unique_material(entry, &mut seen, ctx.materials, ctx.commands)
        else {
            continue;
        };
        let Some(src) = sources.get(k).and_then(Option::as_ref) else {
            continue;
        };
        let Some(image) = try_load_lightmap_image(src, ctx.images) else {
            continue;
        };
        // The same unwrap the bake used — the UVs must match the texels.
        ctx.uv_cache.ensure(ctx.meshes, entry.mesh.id());
        if let Some(mut mat) = ctx.materials.get_mut(mat_handle.id()) {
            mat.extension.lightmap = image;
        }
        applied += 1;
    }
    applied
}

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
    let Some(world) = terrain_world else {
        return;
    };

    for (root, placed) in &pending {
        let entries = collect_toon_meshes(root, &mesh_query, &children_query);
        if entries.is_empty() {
            continue;
        }
        commands.entity(root).remove::<NeedsObjectLightmaps>();

        let sources: Vec<Option<LightmapSource>> = (0..entries.len())
            .map(|k| {
                let path = object_lightmap_path(&world.world_path, placed.id, k);
                path.exists().then_some(path)
            })
            .collect();

        let applied = apply_lightmap_sources(
            &entries,
            &sources,
            &mut ApplyCtx {
                commands: &mut commands,
                meshes: &mut meshes,
                materials: &mut materials,
                images: &mut images,
                uv_cache: &mut uv_cache,
            },
        );
        debug!(
            "Applied {applied}/{} saved lightmaps to placed object id={}",
            entries.len(),
            placed.id
        );
    }
}

// ----------------------------------------------------------------------------
// wasm: the same thing, split in two because OPFS reads are async
// ----------------------------------------------------------------------------

/// Object lightmap fetches in flight. Native reads the EXRs inline; OPFS
/// can't, so the fetch is kicked off in one system and the result applied in
/// another, mirroring how `streaming.rs` loads chunk data.
#[cfg(all(target_arch = "wasm32", feature = "opfs"))]
#[derive(Resource, Default)]
pub struct PendingObjectLightmaps(Vec<InFlightObjectLightmaps>);

#[cfg(all(target_arch = "wasm32", feature = "opfs"))]
struct InFlightObjectLightmaps {
    root: Entity,
    object_id: u64,
    /// `None` until the future has fetched every sub-mesh.
    slot: std::sync::Arc<std::sync::Mutex<Option<Vec<Option<Vec<u8>>>>>>,
}

/// Kick off an OPFS fetch for every placed object whose toon meshes now exist.
#[cfg(all(target_arch = "wasm32", feature = "opfs"))]
pub fn fetch_object_lightmaps(
    mut commands: Commands,
    terrain_world: Option<Res<crate::game::world::terrain::storage::TerrainWorld>>,
    pending: Query<
        (Entity, &crate::game::world::objects::PlacedObject),
        With<NeedsObjectLightmaps>,
    >,
    mesh_query: ToonMeshQuery,
    children_query: Query<&Children>,
    mut in_flight: ResMut<PendingObjectLightmaps>,
) {
    use std::sync::{Arc, Mutex};

    let Some(world) = terrain_world else {
        return;
    };

    for (root, placed) in &pending {
        let entries = collect_toon_meshes(root, &mesh_query, &children_query);
        if entries.is_empty() {
            continue;
        }
        commands.entity(root).remove::<NeedsObjectLightmaps>();

        let slot: Arc<Mutex<Option<Vec<Option<Vec<u8>>>>>> = Arc::new(Mutex::new(None));
        in_flight.0.push(InFlightObjectLightmaps {
            root,
            object_id: placed.id,
            slot: slot.clone(),
        });

        let world_id = world.metadata.world_id.clone();
        let object_id = placed.id;
        let count = entries.len();
        wasm_bindgen_futures::spawn_local(async move {
            let storage = crate::storage::wasm::WasmStorage;
            let mut out = Vec::with_capacity(count);
            for k in 0..count {
                let key = object_lightmap_key(&world_id, object_id, k);
                out.push(storage.load(&key).await.ok().flatten());
            }
            *slot.lock().unwrap() = Some(out);
        });
    }
}

/// Apply fetches that have landed.
#[cfg(all(target_arch = "wasm32", feature = "opfs"))]
pub fn apply_object_lightmaps(
    mut commands: Commands,
    mesh_query: ToonMeshQuery,
    children_query: Query<&Children>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<ToonMat>>,
    mut images: ResMut<Assets<Image>>,
    mut uv_cache: ResMut<super::uv_unwrap::LightmapUvCache>,
    mut in_flight: ResMut<PendingObjectLightmaps>,
) {
    let mut ready: Vec<(Entity, u64, Vec<Option<Vec<u8>>>)> = Vec::new();
    in_flight.0.retain(|job| {
        let fetched = job.slot.lock().unwrap().take();
        match fetched {
            Some(sources) => {
                ready.push((job.root, job.object_id, sources));
                false
            }
            None => true,
        }
    });

    for (root, object_id, sources) in ready {
        // The object may have been despawned while the fetch was in flight.
        let entries = collect_toon_meshes(root, &mesh_query, &children_query);
        if entries.is_empty() {
            continue;
        }
        let applied = apply_lightmap_sources(
            &entries,
            &sources,
            &mut ApplyCtx {
                commands: &mut commands,
                meshes: &mut meshes,
                materials: &mut materials,
                images: &mut images,
                uv_cache: &mut uv_cache,
            },
        );
        debug!(
            "Applied {applied}/{} saved lightmaps to placed object id={object_id}",
            entries.len(),
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
                    parent: None,
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
        let (meshes, children) = state.get(&world).unwrap();
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
