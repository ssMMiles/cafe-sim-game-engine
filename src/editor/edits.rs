//! The one path every mutation of a placed entity goes through.
//!
//! Panels edit live components directly (that is what makes a drag field feel
//! live) and then report *what* changed with a [`PlacedEdited`] message.
//! [`commit_placed_edits`] does the bookkeeping that is easy to forget and
//! fatal to get wrong: re-deriving the owning chunk from the transform,
//! dirtying the chunk(s) so Shift+S writes them, syncing the gameplay label,
//! and flagging the baked lighting as out of date.
//!
//! Script bindings are different: the plugin only sees prop values when its
//! hydrate function runs, and the host skips bindings it has already
//! hydrated. So script edits are messages ([`ScriptEdit`]) applied here by
//! detaching and re-arming hydration, which rebuilds the plugin's components
//! from the new props (and resets their runtime state — panels therefore
//! commit on release/blur, never per frame).
//!
//! This is also where the undo stack records: every message carries the value
//! it replaced, and the systems here push a [`history::Change`] for each —
//! unless the message is a replay *from* the history (`PlacedEdited::replay`,
//! `ScriptEdit::Replace`), which is applied but never recorded.

use std::collections::HashMap;

use bevy::prelude::*;
use engine_api::{ScriptBinding, ScriptBindings, ScriptProp, ScriptRegistry};

use super::history::{self, Change, History, MergeKey};
use super::selection::{PlacedTree, Selection, descendants};
use crate::game::gltf_loading::Interactable;
use crate::game::world::baking::BakeMessage;
use crate::game::world::objects::{
    PlacedKind, PlacedObject, PlacedObjectRegistry, spawn_entity_record, world_pos_to_chunk,
};
use crate::script_host::NeedsHydration;

/// What changed on a placed entity. Sent after the component was mutated,
/// carrying the value it had before so the change can be undone.
#[derive(Message, Debug, Clone)]
pub struct PlacedEdited {
    pub entity: Entity,
    pub what: EditKind,
    /// Sent by the undo stack itself: apply the bookkeeping, but neither
    /// record it nor cascade a group move to its members (the history holds
    /// every member's exact transform).
    pub replay: bool,
}

impl PlacedEdited {
    pub fn transform(entity: Entity, before: Transform) -> Self {
        Self::new(entity, EditKind::Transform { before })
    }

    pub fn name(entity: Entity, before: String) -> Self {
        Self::new(entity, EditKind::Name { before })
    }

    pub fn kind(entity: Entity, before: PlacedKind) -> Self {
        Self::new(entity, EditKind::Kind { before })
    }

    fn new(entity: Entity, what: EditKind) -> Self {
        Self {
            entity,
            what,
            replay: false,
        }
    }

    pub fn replayed(mut self) -> Self {
        self.replay = true;
        self
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum EditKind {
    /// `Transform` changed: may have crossed a chunk boundary; lighting stale.
    Transform { before: Transform },
    /// `PlacedObject.name` changed: gameplay `Interactable` labels follow.
    Name { before: String },
    /// `PlacedObject.kind` parameters changed in place (light colour, range…).
    Kind { before: PlacedKind },
}

/// Script-binding edits. Applied by [`apply_script_edits`].
#[derive(Message, Debug, Clone)]
pub enum ScriptEdit {
    Attach {
        entity: Entity,
        plugin: String,
        script: String,
    },
    Detach {
        entity: Entity,
        plugin: String,
        script: String,
    },
    /// Replace the props of one binding and re-hydrate it.
    SetProps {
        entity: Entity,
        plugin: String,
        script: String,
        props: Vec<ScriptProp>,
    },
    /// Undo/redo: make the binding list exactly `bindings`, re-hydrating
    /// everything. Never recorded.
    Replace {
        entity: Entity,
        bindings: Vec<ScriptBinding>,
    },
}

/// Re-create a placed entity from its record (same id). Needed when the
/// change can't be applied in place: a different mesh asset or collider mode.
/// Also used by Duplicate (with a fresh id).
#[derive(Message, Debug, Clone, Copy)]
pub enum RespawnPlaced {
    InPlace(Entity),
    Duplicate(Entity),
}

/// Whether the baked lightmaps still match the level. Purely advisory: the
/// bake is manual, this just makes "you moved something" visible.
#[derive(Resource, Default, Debug)]
pub struct LightingState {
    pub stale: bool,
}

/// Bookkeeping for [`PlacedEdited`].
///
/// `group_transforms` remembers every group's transform as of the end of the
/// previous run, so a group's transform edit can be turned into a delta and
/// applied to its loaded members (no ECS parenting: members keep world
/// transforms and are moved explicitly).
pub fn commit_placed_edits(
    mut edits: MessageReader<PlacedEdited>,
    mut registry: ResMut<PlacedObjectRegistry>,
    mut lighting: ResMut<LightingState>,
    mut history: ResMut<History>,
    mut placed: PlacedTree,
    children: Query<&Children>,
    mut interactables: Query<&mut Interactable>,
    mut group_transforms: Local<HashMap<Entity, Transform>>,
) {
    for edit in edits.read() {
        let Ok((_, obj, transform)) = placed.get(edit.entity) else {
            continue;
        };
        let (id, chunk, is_group) = (obj.id, obj.chunk, obj.kind.is_group());
        let display = obj.display_name();
        let new_transform = *transform;
        registry.mark_dirty(chunk);

        match &edit.what {
            EditKind::Transform { before } => {
                rederive_chunk(&mut placed, &mut registry, edit.entity);
                lighting.stale = true;

                let mut tx = (!edit.replay).then(|| {
                    history.merge_or_push(
                        MergeKey::Transform(id),
                        format!("Move '{display}'"),
                        Change::Transform {
                            id,
                            before: *before,
                            after: new_transform,
                        },
                    )
                });

                // Members follow a group by the group's delta. Each member's
                // move is recorded on its own, so undo restores exact
                // transforms rather than re-running the cascade (which would
                // miss members that weren't loaded at the time, or move ones
                // that were).
                if is_group
                    && !edit.replay
                    && let Some(old) = group_transforms.get(&edit.entity).copied()
                {
                    let delta = new_transform.to_matrix() * old.to_matrix().inverse();
                    if delta != Mat4::IDENTITY {
                        for member in descendants(id, &placed) {
                            if let Ok((_, m, mut t)) = placed.get_mut(member) {
                                let before = *t;
                                *t = Transform::from_matrix(delta * t.to_matrix());
                                if let Some(tx) = tx.as_deref_mut() {
                                    tx.absorb(Change::Transform {
                                        id: m.id,
                                        before,
                                        after: *t,
                                    });
                                }
                            }
                            rederive_chunk(&mut placed, &mut registry, member);
                        }
                    }
                }
            }
            EditKind::Name { before } => {
                // The label is cloned onto the root and every mesh child at
                // scene-ready time (`propagate_to_mesh_children`); keep them
                // all in step.
                let label = if obj.name.is_empty() {
                    "Object".to_owned()
                } else {
                    obj.name.clone()
                };
                let after = obj.name.clone();
                for e in std::iter::once(edit.entity).chain(children.iter_descendants(edit.entity))
                {
                    if let Ok(mut i) = interactables.get_mut(e) {
                        i.name = label.clone();
                    }
                }
                if !edit.replay {
                    history.push(
                        format!(
                            "Rename '{}' to '{label}'",
                            history::name_or_kind(before, obj)
                        ),
                        vec![Change::Name {
                            id,
                            before: before.clone(),
                            after,
                        }],
                    );
                }
            }
            EditKind::Kind { before } => {
                // The baker and the light gizmo read `PlacedObject.kind`
                // live, so nothing to propagate — only the bake is stale.
                lighting.stale = true;
                if !edit.replay {
                    history.merge_or_push(
                        MergeKey::Kind(id),
                        format!("Edit '{display}'"),
                        Change::Kind {
                            id,
                            before: before.clone(),
                            after: obj.kind.clone(),
                        },
                    );
                }
            }
        }
    }

    // Snapshot group transforms for next time (after this run's edits, so
    // the next delta is measured from where the group now is).
    group_transforms.clear();
    for (entity, obj, transform) in placed.iter() {
        if obj.kind.is_group() {
            group_transforms.insert(entity, *transform);
        }
    }
}

/// Re-derive which chunk owns an entity from its transform, dirtying both the
/// chunk it leaves and the one it joins. The entity is saved into whichever
/// chunk's file `PlacedObject.chunk` names, so both must be rewritten.
fn rederive_chunk(placed: &mut PlacedTree, registry: &mut PlacedObjectRegistry, entity: Entity) {
    let Ok((_, mut obj, transform)) = placed.get_mut(entity) else {
        return;
    };
    let new_chunk = world_pos_to_chunk(transform.translation.x, transform.translation.z);
    registry.mark_dirty(obj.chunk);
    if new_chunk != obj.chunk {
        registry.mark_dirty(new_chunk);
        obj.chunk = new_chunk;
    }
}

/// Attach / detach / re-prop bindings, re-arming hydration so the plugin
/// sees the result.
pub fn apply_script_edits(
    mut commands: Commands,
    mut edits: MessageReader<ScriptEdit>,
    script_registry: Res<ScriptRegistry>,
    mut registry: ResMut<PlacedObjectRegistry>,
    mut history: ResMut<History>,
    placed: Query<&PlacedObject>,
    mut bindings_query: Query<&mut ScriptBindings>,
) {
    for edit in edits.read() {
        let entity = match edit {
            ScriptEdit::Attach { entity, .. }
            | ScriptEdit::Detach { entity, .. }
            | ScriptEdit::SetProps { entity, .. }
            | ScriptEdit::Replace { entity, .. } => *entity,
        };
        let Ok(obj) = placed.get(entity) else {
            continue;
        };
        let before: Vec<ScriptBinding> = bindings_query
            .get(entity)
            .map(|b| b.0.clone())
            .unwrap_or_default();
        let label = match edit {
            ScriptEdit::Attach { plugin, script, .. } => {
                if before
                    .iter()
                    .any(|x| &x.plugin == plugin && &x.script == script)
                {
                    info!("{plugin}/{script} already attached to id={}", obj.id);
                    continue;
                }
                let props = script_registry
                    .get(plugin, script)
                    .map(|r| r.schema.default_props())
                    .unwrap_or_default();
                let binding = ScriptBinding::new(plugin.clone(), script.clone()).with_props(props);
                match bindings_query.get_mut(entity) {
                    Ok(mut existing) => existing.0.push(binding),
                    Err(_) => {
                        commands
                            .entity(entity)
                            .insert(ScriptBindings(vec![binding]));
                    }
                }
                commands.entity(entity).insert(NeedsHydration);
                info!("Attached {plugin}/{script} to placed id={}", obj.id);
                format!("Attach {plugin}/{script}")
            }
            ScriptEdit::Detach { plugin, script, .. } => {
                let Ok(mut existing) = bindings_query.get_mut(entity) else {
                    continue;
                };
                existing
                    .0
                    .retain(|b| !(&b.plugin == plugin && &b.script == script));
                if existing.0.len() == before.len() {
                    continue;
                }
                if existing.0.is_empty() {
                    commands
                        .entity(entity)
                        .remove::<(ScriptBindings, NeedsHydration)>();
                }
                queue_detach(&mut commands, entity, plugin, script);
                info!("Detached {plugin}/{script} from placed id={}", obj.id);
                format!("Detach {plugin}/{script}")
            }
            ScriptEdit::SetProps {
                plugin,
                script,
                props,
                ..
            } => {
                let Ok(mut existing) = bindings_query.get_mut(entity) else {
                    continue;
                };
                let Some(binding) = existing
                    .0
                    .iter_mut()
                    .find(|b| &b.plugin == plugin && &b.script == script)
                else {
                    continue;
                };
                if binding.props == *props {
                    continue;
                }
                binding.props = props.clone();
                // Hydration skips (plugin, script) pairs it has already done,
                // so strip the old components first and re-arm.
                queue_detach(&mut commands, entity, plugin, script);
                commands.entity(entity).insert(NeedsHydration);
                format!("Edit {plugin}/{script} props")
            }
            ScriptEdit::Replace { bindings, .. } => {
                if *bindings == before {
                    continue;
                }
                // Everything hydrated goes, then the whole target list is
                // re-armed: exact, and re-hydrating an unchanged binding is
                // harmless in the editor.
                for b in &before {
                    queue_detach(&mut commands, entity, &b.plugin, &b.script);
                }
                if bindings.is_empty() {
                    commands
                        .entity(entity)
                        .remove::<(ScriptBindings, NeedsHydration)>();
                } else {
                    commands
                        .entity(entity)
                        .insert((ScriptBindings(bindings.clone()), NeedsHydration));
                }
                registry.mark_dirty(obj.chunk);
                continue; // a replay: never recorded
            }
        };
        registry.mark_dirty(obj.chunk);

        // `after` for Attach/Detach/SetProps is what the component now holds
        // (Attach of a first binding went through `commands`, so build it).
        let after = match edit {
            ScriptEdit::Attach { plugin, script, .. } if before.is_empty() => {
                let props = script_registry
                    .get(plugin, script)
                    .map(|r| r.schema.default_props())
                    .unwrap_or_default();
                vec![ScriptBinding::new(plugin.clone(), script.clone()).with_props(props)]
            }
            _ => bindings_query
                .get(entity)
                .map(|b| b.0.clone())
                .unwrap_or_default(),
        };
        history.push(
            label,
            vec![Change::Scripts {
                id: obj.id,
                before,
                after,
            }],
        );
    }
}

fn queue_detach(commands: &mut Commands, entity: Entity, plugin: &str, script: &str) {
    let (plugin, script) = (plugin.to_owned(), script.to_owned());
    commands.queue(move |world: &mut World| {
        crate::script_host::detach_binding(world, entity, &plugin, &script);
    });
}

/// Re-create entities from their live record. Selection follows the new
/// entity.
pub fn respawn_placed(
    mut commands: Commands,
    mut requests: MessageReader<RespawnPlaced>,
    asset_server: Res<AssetServer>,
    mut registry: ResMut<PlacedObjectRegistry>,
    mut selection: ResMut<Selection>,
    mut lighting: ResMut<LightingState>,
    mut history: ResMut<History>,
    placed: Query<(&PlacedObject, &Transform, Option<&ScriptBindings>)>,
) {
    for request in requests.read() {
        let (entity, duplicate) = match *request {
            RespawnPlaced::InPlace(e) => (e, false),
            RespawnPlaced::Duplicate(e) => (e, true),
        };
        let Ok((obj, transform, bindings)) = placed.get(entity) else {
            continue;
        };
        let mut record = obj.to_record(transform, bindings);
        let chunk = world_pos_to_chunk(transform.translation.x, transform.translation.z);

        if duplicate {
            if !registry.ids_seeded {
                warn!("Duplicate unavailable: world entity-id counter not seeded yet");
                continue;
            }
            record.id = registry.alloc_id();
            // Nudge so the copy is visibly a copy and not a z-fighting twin.
            record.position[0] += 1.0;
        } else {
            registry.by_id.remove(&obj.id);
            registry.mark_dirty(obj.chunk);
            commands.entity(entity).despawn();
        }

        let new_entity =
            spawn_entity_record(&mut commands, &asset_server, &mut registry, chunk, &record);
        registry.mark_dirty(chunk);
        lighting.stale = true;
        selection.primary = Some(new_entity);
        // An in-place respawn was caused by a `Kind` change that is already
        // recorded; only a duplicate is a new fact.
        if duplicate {
            history.push(
                format!("Duplicate '{}'", obj.display_name()),
                vec![Change::Spawn {
                    chunk,
                    record: record.clone(),
                }],
            );
        }
        info!(
            "{} placed {} id={}",
            if duplicate { "Duplicated" } else { "Respawned" },
            record.kind.label(),
            record.id
        );
    }
}

/// A bake request means the lighting is about to match the level again.
pub fn clear_lighting_stale_on_bake(
    mut bakes: MessageReader<BakeMessage>,
    mut lighting: ResMut<LightingState>,
) {
    if bakes.read().count() > 0 {
        lighting.stale = false;
    }
}
