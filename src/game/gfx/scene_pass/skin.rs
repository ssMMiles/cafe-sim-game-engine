//! Joint matrices for skinned meshes — the one thing the scene pass borrowed
//! from bevy_pbr until Phase 5 (`docs/SCENE_PASS_PLAN.md`), and now its own.
//!
//! Every frame, for every visible `SkinnedMesh`: one `joint_world ×
//! inverse_bindpose` matrix per joint, appended to a storage buffer that is
//! bind group 0 binding 2; the mesh's `SceneInstance.skin_index` is where its
//! run starts, and `mesh_vertex.wesl`'s `SKINNED` variant blends four of them
//! per vertex. Rebuilt from scratch each frame rather than tracked by change:
//! this project has one skinned mesh (the player, ~60 joints), and bevy's own
//! `extract_skins` — with its allocator, per-joint change detection and
//! double-buffering for motion vectors — was several hundred lines in service
//! of cases that don't arise here.
//!
//! The matrix is the same one bevy computes (`extract_joints_for_skin`):
//! the joint's `GlobalTransform` already includes the skinned mesh's own
//! transform through the hierarchy, so the result is a full local-to-world.

use bevy::{
    mesh::skinning::{SkinnedMesh, SkinnedMeshInverseBindposes},
    prelude::*,
    render::{
        Extract,
        render_resource::{BindingResource, BufferUsages, BufferVec},
        renderer::{RenderDevice, RenderQueue},
        sync_world::{MainEntity, MainEntityHashMap},
    },
};

#[derive(Resource)]
pub struct SceneSkins {
    joints: BufferVec<Mat4>,
    /// First joint of each skinned mesh in `joints`.
    offsets: MainEntityHashMap<u32>,
}

impl Default for SceneSkins {
    fn default() -> Self {
        let mut joints = BufferVec::new(BufferUsages::STORAGE);
        joints.set_label(Some("scene_skin_joints"));
        Self {
            joints,
            offsets: MainEntityHashMap::default(),
        }
    }
}

impl SceneSkins {
    /// Where `skinned_mesh`'s joints start in the buffer, in matrices.
    pub fn skin_index(&self, skinned_mesh: MainEntity) -> Option<u32> {
        self.offsets.get(&skinned_mesh).copied()
    }

    pub fn binding(&self) -> Option<BindingResource<'_>> {
        self.joints.binding()
    }
}

pub fn extract_skins(
    mut skins: ResMut<SceneSkins>,
    skinned: Extract<Query<(Entity, &ViewVisibility, &SkinnedMesh)>>,
    inverse_bindposes: Extract<Res<Assets<SkinnedMeshInverseBindposes>>>,
    joints: Extract<Query<&GlobalTransform>>,
) {
    skins.joints.clear();
    skins.offsets.clear();
    for (entity, visibility, skin) in &skinned {
        if !visibility.get() {
            continue;
        }
        let Some(inverse_bindposes) = inverse_bindposes.get(&skin.inverse_bindposes) else {
            continue; // Still loading; the mesh draws unskinned-garbage-free next frame.
        };
        let offset = skins.joints.len() as u32;
        for (&joint, inverse_bindpose) in skin.joints.iter().zip(inverse_bindposes.iter()) {
            // A joint entity that vanished mid-animation: bind pose rather
            // than a hole that would shift every later joint's index.
            let world = joints
                .get(joint)
                .map_or(Mat4::IDENTITY, |t| Mat4::from(t.affine()));
            skins.joints.push(world * *inverse_bindpose);
        }
        skins.offsets.insert(MainEntity::from(entity), offset);
    }
}

/// Upload. One identity keeps the buffer bindable on frames with nothing
/// skinned in view (the main menu, the editor with the player hidden).
pub fn prepare_skins(
    mut skins: ResMut<SceneSkins>,
    device: Res<RenderDevice>,
    queue: Res<RenderQueue>,
) {
    if skins.joints.is_empty() {
        skins.joints.push(Mat4::IDENTITY);
    }
    skins.joints.write_buffer(&device, &queue);
}
