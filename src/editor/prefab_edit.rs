//! Editor operations on prefabs: save a selection as a new prefab, and the
//! instance-level ops (apply / revert / unpack — see the inspector's
//! instance section).
//!
//! Panels send a [`PrefabAction`]; [`apply_prefab_actions`] runs in the edit
//! chain between undo/redo and the commit systems. The definition write goes
//! through `prefabs::store_prefab` (native file + OPFS mirror + library).

use bevy::ecs::system::SystemParam;
use bevy::prelude::*;
use engine_api::ScriptBindings;

use super::edits::LightingState;
use super::history::{Change, History};
use super::selection::{PlacedTree, Selection, descendants};
use crate::game::world::objects::{PlacedEntityRecord, PlacedObjectRegistry, spawn_entity_record};
use crate::game::world::prefabs::{
    PrefabLibrary, compute_instance_overrides, define_from_instance, define_from_records,
    store_prefab, validate_prefab_name,
};

#[derive(Message, Debug, Clone)]
pub enum PrefabAction {
    /// Turn the selection (a group with its members, or a single entity)
    /// into a new prefab definition and make it the first instance.
    SaveSelectionAs(String),
    /// Rewrite the definition from this instance and re-expand every loaded
    /// instance.
    Apply(Entity),
    /// Drop this instance's deviations and re-expand it from the definition.
    Revert(Entity),
    /// Sever the link: the root becomes a plain entity/group and the members
    /// plain entities, saved normally from now on.
    Unpack(Entity),
}

/// UI state for the Place panel's Prefabs tab.
#[derive(Resource, Default)]
pub struct PrefabEditState {
    /// Draft name for "Save selection as prefab".
    pub new_name: String,
    pub error: Option<String>,
}

#[derive(SystemParam)]
pub struct PrefabOps<'w, 's> {
    commands: Commands<'w, 's>,
    asset_server: Res<'w, AssetServer>,
    registry: ResMut<'w, PlacedObjectRegistry>,
    library: ResMut<'w, PrefabLibrary>,
    placed: PlacedTree<'w, 's>,
    bindings: Query<'w, 's, &'static ScriptBindings>,
    selection: ResMut<'w, Selection>,
    history: ResMut<'w, History>,
    lighting: ResMut<'w, LightingState>,
    state: ResMut<'w, PrefabEditState>,
}

impl PrefabOps<'_, '_> {
    fn record_of(&self, entity: Entity) -> Option<PlacedEntityRecord> {
        let (_, obj, transform) = self.placed.get(entity).ok()?;
        Some(obj.to_record(transform, self.bindings.get(entity).ok()))
    }

    /// The live records of an instance's members (and any extras parented
    /// under it), excluding the root.
    fn member_records(&mut self, root: Entity, root_id: u64) -> Vec<PlacedEntityRecord> {
        descendants(root_id, &self.placed)
            .into_iter()
            .filter(|e| *e != root)
            .filter_map(|e| self.record_of(e))
            .collect()
    }

    fn fail(&mut self, message: impl Into<String>) {
        let message = message.into();
        warn!("{message}");
        self.state.error = Some(message);
    }

    fn save_selection_as(&mut self, name: &str) {
        self.state.error = None;
        if let Err(e) = validate_prefab_name(name) {
            return self.fail(e);
        }
        if self.library.get(name).is_some() {
            return self.fail(format!(
                "Prefab '{name}' already exists — select one of its instances and Apply instead"
            ));
        }
        let Some(root) = self.selection.primary else {
            return self.fail("Nothing selected");
        };
        let Some(root_record) = self.record_of(root) else {
            return self.fail("Selection is not a placed entity");
        };
        if root_record.prefab.is_some() {
            return self.fail("Selection is already a prefab instance");
        }
        let members = self.member_records(root, root_record.id);
        if members.iter().any(|m| m.prefab.is_some()) {
            return self.fail("Nested prefabs aren't supported yet");
        }

        let (def, inst) = define_from_records(name, &root_record, &members);
        if let Err(e) = store_prefab(&mut self.library, def) {
            return self.fail(format!("Failed to save prefab: {e}"));
        }

        // The selection becomes the first instance, linked from here on.
        if let Ok((_, mut obj, _)) = self.placed.get_mut(root) {
            obj.prefab = Some(inst.clone());
            let chunk = obj.chunk;
            self.registry.mark_dirty(chunk);
        }
        self.history.push(
            format!("Save prefab '{name}'"),
            vec![Change::PrefabInstance {
                id: root_record.id,
                before: None,
                after: Some(inst),
            }],
        );
    }

    fn instance_of(
        &self,
        root: Entity,
    ) -> Option<(u64, crate::game::world::prefabs::PrefabInstanceData)> {
        let (_, obj, _) = self.placed.get(root).ok()?;
        Some((obj.id, obj.prefab.clone()?))
    }

    /// This instance's deviations from its definition, diffed live.
    fn fresh_overrides(
        &self,
        root: Entity,
    ) -> Option<crate::game::world::prefabs::PrefabInstanceData> {
        let (_, inst) = self.instance_of(root)?;
        let def = self.library.get(&inst.name)?;
        let root_record = self.record_of(root)?;
        let lookup = |id: u64| {
            self.registry
                .by_id
                .get(&id)
                .copied()
                .and_then(|e| self.record_of(e))
        };
        let overrides = compute_instance_overrides(def, &inst, &root_record, lookup);
        Some(crate::game::world::prefabs::PrefabInstanceData { overrides, ..inst })
    }

    fn apply_to_prefab(&mut self, root: Entity) {
        self.state.error = None;
        let Some((root_id, inst)) = self.instance_of(root) else {
            return self.fail("Not a prefab instance");
        };
        let Some(old_def) = self.library.get(&inst.name).cloned() else {
            return self.fail(format!(
                "Prefab '{}' is missing from the library",
                inst.name
            ));
        };
        let Some(root_record) = self.record_of(root) else {
            return;
        };
        let members = self.member_records(root, root_id);
        let (new_def, new_inst) = define_from_instance(&old_def, &inst, &root_record, &members);

        // Other loaded instances keep their own deviations: diff each one
        // against the *old* definition before it is replaced, then re-expand
        // against the new one with those overrides pinned.
        let others = collect_fresh_instances(
            &inst.name,
            Some(root),
            &old_def,
            &self.registry,
            &self.placed,
            &self.bindings,
        );

        if let Err(e) = store_prefab(&mut self.library, new_def.clone()) {
            return self.fail(format!("Failed to save prefab: {e}"));
        }
        // The applying instance *is* the new definition: fresh pairs (extras
        // folded in), no overrides. Its entities stay in place.
        if let Ok((_, mut obj, _)) = self.placed.get_mut(root) {
            obj.prefab = Some(new_inst.clone());
            let chunk = obj.chunk;
            self.registry.mark_dirty(chunk);
        }
        let other_count = others.len();
        for (entity, fresh) in others {
            set_instance_and_reexpand(
                entity,
                Some(fresh),
                &mut self.commands,
                &self.asset_server,
                &mut self.registry,
                &self.library,
                &mut self.placed,
                &self.bindings,
            );
        }
        self.lighting.stale = true;
        self.history.push(
            format!("Apply to prefab '{}'", inst.name),
            vec![
                Change::PrefabDefinition {
                    name: inst.name.clone(),
                    before: old_def,
                    after: new_def,
                },
                Change::PrefabInstance {
                    id: root_id,
                    before: Some(inst),
                    after: Some(new_inst),
                },
            ],
        );
        info!(
            "Applied instance id={root_id} to prefab; re-expanded {other_count} other instance(s)"
        );
    }

    fn revert_instance(&mut self, root: Entity) {
        self.state.error = None;
        let Some((root_id, inst)) = self.instance_of(root) else {
            return self.fail("Not a prefab instance");
        };
        if self.library.get(&inst.name).is_none() {
            return self.fail(format!(
                "Prefab '{}' is missing from the library",
                inst.name
            ));
        }
        let Some(before) = self.fresh_overrides(root) else {
            return;
        };
        let after = crate::game::world::prefabs::PrefabInstanceData {
            overrides: Vec::new(),
            ..inst.clone()
        };
        if before.overrides.is_empty() {
            info!("Instance id={root_id} already matches the prefab");
            return;
        }
        set_instance_and_reexpand(
            root,
            Some(after.clone()),
            &mut self.commands,
            &self.asset_server,
            &mut self.registry,
            &self.library,
            &mut self.placed,
            &self.bindings,
        );
        self.lighting.stale = true;
        self.history.push(
            format!("Revert instance of '{}'", inst.name),
            vec![Change::PrefabInstance {
                id: root_id,
                before: Some(before),
                after: Some(after),
            }],
        );
    }

    fn unpack_instance(&mut self, root: Entity) {
        self.state.error = None;
        let Some((root_id, inst)) = self.instance_of(root) else {
            return self.fail("Not a prefab instance");
        };
        // Snapshot deviations first so undo restores the link *and* the
        // instance's exact state.
        let before = self.fresh_overrides(root).unwrap_or(inst.clone());
        if let Ok((_, mut obj, _)) = self.placed.get_mut(root) {
            obj.prefab = None;
            let chunk = obj.chunk;
            self.registry.mark_dirty(chunk);
        }
        self.history.push(
            format!("Unpack instance of '{}'", inst.name),
            vec![Change::PrefabInstance {
                id: root_id,
                before: Some(before),
                after: None,
            }],
        );
        info!("Unpacked instance id={root_id}; its entities now save normally");
    }
}

pub fn apply_prefab_actions(mut actions: MessageReader<PrefabAction>, mut ops: PrefabOps) {
    for action in actions.read() {
        match action {
            PrefabAction::SaveSelectionAs(name) => ops.save_selection_as(name),
            PrefabAction::Apply(root) => ops.apply_to_prefab(*root),
            PrefabAction::Revert(root) => ops.revert_instance(*root),
            PrefabAction::Unpack(root) => ops.unpack_instance(*root),
        }
    }
}

// ── Re-expansion (shared with the undo stack) ────────────────────────────────

/// Set a root's instance data and rebuild it: despawn the root and its
/// current members, then respawn from the (possibly new) definition +
/// overrides. `None` severs the link, leaving the entities as they are.
/// Returns the new root entity when a rebuild happened.
#[allow(clippy::too_many_arguments)]
pub fn set_instance_and_reexpand(
    root: Entity,
    instance: Option<crate::game::world::prefabs::PrefabInstanceData>,
    commands: &mut Commands,
    asset_server: &AssetServer,
    registry: &mut PlacedObjectRegistry,
    library: &PrefabLibrary,
    placed: &mut PlacedTree,
    bindings: &Query<&ScriptBindings>,
) -> Option<Entity> {
    let Ok((_, obj, transform)) = placed.get(root) else {
        return None;
    };
    let old_inst = obj.prefab.clone();
    let mut record = obj.to_record(transform, bindings.get(root).ok());
    let chunk = obj.chunk;

    record.prefab = instance;
    let Some(inst) = record.prefab.as_mut() else {
        // Unlink only: everything stays live, now saved as plain entities.
        if let Ok((_, mut obj, _)) = placed.get_mut(root) {
            obj.prefab = None;
        }
        registry.mark_dirty(chunk);
        return None;
    };

    // Pair healing: a definition that grew since this instance's pairs were
    // minted needs fresh world ids for the new members.
    if let Some(def) = library.get(&inst.name) {
        for member in &def.entities[1..] {
            if inst.world_id_of(member.id).is_none() {
                inst.member_ids.push((member.id, registry.alloc_id()));
            }
        }
    }

    // Tear down the live instance: old members (by the *old* pairs) and the
    // root itself, then respawn — expansion rebuilds everything, including a
    // root whose kind the definition changed.
    if let Some(old) = &old_inst {
        for id in old.member_world_ids() {
            if let Some(member) = registry.by_id.remove(&id) {
                commands.entity(member).despawn();
            }
        }
    }
    registry.by_id.remove(&record.id);
    commands.entity(root).despawn();
    registry.mark_dirty(chunk);
    Some(spawn_entity_record(
        commands,
        asset_server,
        registry,
        library,
        chunk,
        &record,
    ))
}

/// Every loaded instance of `name` (except `skip`) with its deviations
/// freshly diffed against `def` — callers pass the library's **current**
/// definition before swapping it, so each instance's edits are measured
/// against what it was actually expanded from.
pub fn collect_fresh_instances(
    name: &str,
    skip: Option<Entity>,
    def: &crate::game::world::prefabs::PrefabFile,
    registry: &PlacedObjectRegistry,
    placed: &PlacedTree,
    bindings: &Query<&ScriptBindings>,
) -> Vec<(Entity, crate::game::world::prefabs::PrefabInstanceData)> {
    let record_of = |entity: Entity| {
        placed
            .get(entity)
            .ok()
            .map(|(_, obj, t)| obj.to_record(t, bindings.get(entity).ok()))
    };
    placed
        .iter()
        .filter(|(e, obj, _)| {
            Some(*e) != skip && obj.prefab.as_ref().is_some_and(|i| i.name == name)
        })
        .filter_map(|(e, obj, transform)| {
            let inst = obj.prefab.clone()?;
            let root_record = obj.to_record(transform, bindings.get(e).ok());
            let overrides = compute_instance_overrides(def, &inst, &root_record, |id| {
                registry.by_id.get(&id).copied().and_then(record_of)
            });
            Some((
                e,
                crate::game::world::prefabs::PrefabInstanceData { overrides, ..inst },
            ))
        })
        .collect()
}
