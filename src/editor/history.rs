//! Undo / redo for every change to level data made from the editor.
//!
//! A [`Transaction`] is a label plus a list of [`Change`]s, each a before /
//! after pair of **plain data keyed by the placed entity's stable id** — never
//! an `Entity`, because grab, respawn and chunk streaming all mint a new
//! entity for the same id. Undo applies the changes in reverse with before and
//! after swapped; redo replays them forward. Both go through [`LevelOps`],
//! which owns the raw mutations and the bookkeeping (dirty chunks, stale
//! lighting, re-hydration) by re-using the same messages the panels send,
//! flagged as replays so the recorders ignore them.
//!
//! Recording happens at the existing choke points — `commit_placed_edits`,
//! `apply_script_edits`, `respawn_placed`, `delete_placed`, the place/grab/
//! group actions, the terrain stroke end and the environment panel — so no
//! panel can change level data without leaving a record.
//!
//! Continuous edits (a gizmo or inspector drag) coalesce: the first edit opens
//! a transaction keyed by `(kind, id)`, later edits with the same key fold
//! into it (first `before`, latest `after`), and [`settle_history`] closes it
//! once the pointer is released and egui is idle. A closed transaction whose
//! changes all turned out to be no-ops (a cancelled gizmo drag, a click on a
//! handle without moving) is dropped.
//!
//! History is kept **per world**, in memory: switching worlds swaps stacks
//! rather than clearing them. Chunks the stacks reference are pinned against
//! streaming unload (`History::pinned`); a change whose target still cannot be
//! resolved skips its whole transaction with a status-bar notice.
//!
//! See `docs/UNDO_REDO_PLAN.md`.

use std::collections::{HashMap, HashSet};

use bevy::ecs::system::SystemParam;
use bevy::prelude::*;
use engine_api::{ScriptBinding, ScriptBindings};

use super::actions::EditorAction;
use super::edits::{LightingState, PlacedEdited, RespawnPlaced, ScriptEdit};
use super::gizmo::GizmoInteraction;
use super::grab::GrabbedEntity;
use super::picking::ViewportInput;
use super::selection::{PlacedTree, Selection};
use super::terrain_edit::{TerrainBrushHit, TerrainChunks, chunks_touching_tile};
use super::tool::EditorTool;
use crate::game::world::objects::{
    PlacedEntityRecord, PlacedKind, PlacedObject, PlacedObjectRegistry, spawn_entity_record,
};
use crate::game::world::prefabs::{PrefabFile, PrefabInstanceData, PrefabLibrary, store_prefab};
use crate::game::world::streaming::StreamingState;
use crate::game::world::terrain::mesh::LOD_LEVELS;
use crate::game::world::terrain::storage::{EnvironmentData, TerrainWorld, WorldConfig};

/// Transactions kept per world; the oldest is evicted past this.
pub const MAX_TRANSACTIONS: usize = 200;
/// Terrain tiles held across a world's stacks, in bytes; the oldest
/// terrain-bearing transactions are evicted past this. A 5 m stroke is
/// ~25 KB, a 50 m one ~2.5 MB.
pub const MAX_TERRAIN_BYTES: usize = 64 << 20;
/// How long a status-bar notice ("Can't undo: …") stays up, in seconds.
const NOTICE_SECONDS: f64 = 4.0;

/// One reversible edit. `before` / `after` are both stored so a change can be
/// applied in either direction without consulting the world.
#[derive(Debug, Clone, PartialEq)]
pub enum Change {
    /// An entity came into existence (undo = despawn by id).
    Spawn {
        chunk: (i32, i32),
        record: PlacedEntityRecord,
    },
    /// An entity was removed (undo = spawn the record, same id).
    Despawn {
        chunk: (i32, i32),
        record: PlacedEntityRecord,
    },
    Transform {
        id: u64,
        before: Transform,
        after: Transform,
    },
    Name {
        id: u64,
        before: String,
        after: String,
    },
    /// Respawns the entity when the asset path or collider differs.
    Kind {
        id: u64,
        before: PlacedKind,
        after: PlacedKind,
    },
    Parent {
        id: u64,
        before: Option<u64>,
        after: Option<u64>,
    },
    Scripts {
        id: u64,
        before: Vec<ScriptBinding>,
        after: Vec<ScriptBinding>,
    },
    Environment {
        before: EnvironmentData,
        after: EnvironmentData,
    },
    /// One brush stroke: only the tiles it touched, as `(tx, ty, before, after)`.
    Terrain { tiles: Vec<(i32, i32, f32, f32)> },
    /// A prefab definition was rewritten ("Apply to prefab"). Applying swaps
    /// the library + file and re-expands every loaded instance.
    PrefabDefinition {
        name: String,
        before: PrefabFile,
        after: PrefabFile,
    },
    /// An instance root's link changed: saved-as (None → Some), reverted,
    /// applied (overrides cleared), or unpacked (Some → None).
    PrefabInstance {
        id: u64,
        before: Option<PrefabInstanceData>,
        after: Option<PrefabInstanceData>,
    },
}

impl Change {
    fn is_noop(&self) -> bool {
        match self {
            Change::Spawn { .. } | Change::Despawn { .. } => false,
            Change::Transform { before, after, .. } => before == after,
            Change::Name { before, after, .. } => before == after,
            Change::Kind { before, after, .. } => before == after,
            Change::Parent { before, after, .. } => before == after,
            Change::Scripts { before, after, .. } => before == after,
            Change::Environment { before, after } => before == after,
            Change::Terrain { tiles } => tiles.iter().all(|(_, _, b, a)| b == a),
            Change::PrefabDefinition { before, after, .. } => before == after,
            Change::PrefabInstance { before, after, .. } => before == after,
        }
    }

    fn terrain_bytes(&self) -> usize {
        match self {
            Change::Terrain { tiles } => tiles.len() * std::mem::size_of::<(i32, i32, f32, f32)>(),
            _ => 0,
        }
    }

    /// The placed id an entity-keyed change targets.
    fn id(&self) -> Option<u64> {
        match self {
            Change::Spawn { record, .. } | Change::Despawn { record, .. } => Some(record.id),
            Change::Transform { id, .. }
            | Change::Name { id, .. }
            | Change::Kind { id, .. }
            | Change::Parent { id, .. }
            | Change::Scripts { id, .. }
            | Change::PrefabInstance { id, .. } => Some(*id),
            Change::Environment { .. }
            | Change::Terrain { .. }
            | Change::PrefabDefinition { .. } => None,
        }
    }

    /// Chunks this change needs resident regardless of where its entity is.
    fn chunks(&self) -> HashSet<(i32, i32)> {
        match self {
            Change::Spawn { chunk, .. } | Change::Despawn { chunk, .. } => HashSet::from([*chunk]),
            Change::Terrain { tiles } => tiles
                .iter()
                .flat_map(|(tx, ty, _, _)| chunks_touching_tile(*tx, *ty))
                .collect(),
            _ => HashSet::new(),
        }
    }

    /// Two changes to the same field of the same target coalesce: keep this
    /// change's `before`, take the newer `after`. Returns false (and leaves
    /// `self` alone) when they don't describe the same field.
    fn absorb(&mut self, newer: &Change) -> bool {
        match (self, newer) {
            (
                Change::Transform { id, after, .. },
                Change::Transform {
                    id: nid,
                    after: nafter,
                    ..
                },
            ) if id == nid => {
                *after = *nafter;
                true
            }
            (
                Change::Kind { id, after, .. },
                Change::Kind {
                    id: nid,
                    after: nafter,
                    ..
                },
            ) if id == nid => {
                *after = nafter.clone();
                true
            }
            (Change::Environment { after, .. }, Change::Environment { after: nafter, .. }) => {
                *after = nafter.clone();
                true
            }
            _ => false,
        }
    }
}

/// Which open transaction a continuous edit folds into.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MergeKey {
    Transform(u64),
    Kind(u64),
    Environment,
}

#[derive(Debug)]
pub struct Transaction {
    /// Shown in the Edit menu: "Undo Move 'Rock'".
    pub label: String,
    pub changes: Vec<Change>,
    /// Placed id selected when the transaction began / ended; undo and redo
    /// restore these so what you were working on comes back with the edit.
    pub selection_before: Option<u64>,
    pub selection_after: Option<u64>,
    merge_key: Option<MergeKey>,
    /// Still coalescing: edits with the same `merge_key` fold in.
    open: bool,
    /// Pushed or extended this frame; `settle` captures `selection_after`.
    fresh: bool,
}

impl Transaction {
    fn new(label: String, changes: Vec<Change>, selection: Option<u64>) -> Self {
        Self {
            label,
            changes,
            selection_before: selection,
            selection_after: selection,
            merge_key: None,
            open: false,
            fresh: true,
        }
    }

    /// Fold `change` in: coalesce with an existing change to the same field,
    /// else append.
    pub fn absorb(&mut self, change: Change) {
        if !self.changes.iter_mut().any(|c| c.absorb(&change)) {
            self.changes.push(change);
        }
        self.fresh = true;
    }
}

#[derive(Default)]
struct Stacks {
    undo: Vec<Transaction>,
    redo: Vec<Transaction>,
}

impl Stacks {
    fn close_open(&mut self) {
        let Some(top) = self.undo.last_mut() else {
            return;
        };
        if !top.open {
            return;
        }
        top.open = false;
        top.merge_key = None;
        if top.changes.iter().all(Change::is_noop) {
            self.undo.pop();
        }
    }

    fn push(&mut self, tx: Transaction) {
        self.close_open();
        self.redo.clear();
        self.undo.push(tx);
        if self.undo.len() > MAX_TRANSACTIONS {
            self.undo.remove(0);
        }
        // Terrain is the only change that can be large; shed the oldest
        // strokes first rather than the oldest transactions.
        let bytes = |tx: &Transaction| tx.changes.iter().map(Change::terrain_bytes).sum::<usize>();
        while self.undo.iter().map(bytes).sum::<usize>() > MAX_TERRAIN_BYTES {
            match self.undo.iter().position(|tx| bytes(tx) > 0) {
                Some(i) if i + 1 < self.undo.len() => {
                    self.undo.remove(i);
                }
                _ => break, // only the newest stroke is left; keep it
            }
        }
    }
}

#[derive(Resource, Default)]
pub struct History {
    worlds: HashMap<String, Stacks>,
    active: String,
    /// Selected placed id as of the start of this frame (`snapshot_selection`);
    /// becomes `selection_before` of anything pushed this frame.
    pub selection_now: Option<u64>,
    /// Chunks referenced by any transaction of the active world. Streaming
    /// keeps these resident.
    pub pinned: HashSet<(i32, i32)>,
    /// Status-bar message and the time it expires.
    notice: Option<(String, f64)>,
}

impl History {
    fn stacks(&mut self) -> &mut Stacks {
        self.worlds.entry(self.active.clone()).or_default()
    }

    fn stacks_ref(&self) -> Option<&Stacks> {
        self.worlds.get(&self.active)
    }

    /// Point the stacks at `world_id`. Other worlds' stacks stay in memory.
    pub fn set_world(&mut self, world_id: &str) {
        if self.active != world_id {
            self.active = world_id.to_owned();
            self.pinned.clear();
        }
    }

    /// Record a discrete transaction. Clears the redo stack.
    pub fn push(&mut self, label: impl Into<String>, changes: Vec<Change>) {
        let changes: Vec<Change> = changes.into_iter().filter(|c| !c.is_noop()).collect();
        if changes.is_empty() {
            return;
        }
        let tx = Transaction::new(label.into(), changes, self.selection_now);
        self.stacks().push(tx);
    }

    /// Record a continuous edit: folds into the top transaction while it is
    /// open with the same key, else opens a new one. Returns the transaction
    /// so a caller can absorb dependent changes (group member moves).
    pub fn merge_or_push(
        &mut self,
        key: MergeKey,
        label: impl Into<String>,
        change: Change,
    ) -> &mut Transaction {
        let selection = self.selection_now;
        let stacks = self.stacks();
        let merges = stacks
            .undo
            .last()
            .is_some_and(|t| t.open && t.merge_key == Some(key));
        if !merges {
            let mut tx = Transaction::new(label.into(), Vec::new(), selection);
            tx.merge_key = Some(key);
            tx.open = true;
            stacks.push(tx);
        }
        let tx = stacks.undo.last_mut().expect("just pushed");
        tx.absorb(change);
        tx
    }

    /// End coalescing. Called when the user lets go, and before any undo.
    pub fn close_open(&mut self) {
        self.stacks().close_open();
    }

    pub fn undo_label(&self) -> Option<&str> {
        self.stacks_ref()?.undo.last().map(|t| t.label.as_str())
    }

    pub fn redo_label(&self) -> Option<&str> {
        self.stacks_ref()?.redo.last().map(|t| t.label.as_str())
    }

    #[cfg(test)]
    pub fn undo_len(&self) -> usize {
        self.stacks_ref().map_or(0, |s| s.undo.len())
    }

    #[cfg(test)]
    pub fn redo_len(&self) -> usize {
        self.stacks_ref().map_or(0, |s| s.redo.len())
    }

    /// Undo stack oldest-first, then redo stack nearest-first — the order the
    /// History panel lists them in.
    pub fn undo_entries(&self) -> impl Iterator<Item = &Transaction> {
        self.stacks_ref().into_iter().flat_map(|s| s.undo.iter())
    }

    pub fn redo_entries(&self) -> impl Iterator<Item = &Transaction> {
        self.stacks_ref()
            .into_iter()
            .flat_map(|s| s.redo.iter().rev())
    }

    pub fn notice(&self) -> Option<&str> {
        self.notice.as_ref().map(|(s, _)| s.as_str())
    }

    fn set_notice(&mut self, text: String, now: f64) {
        warn!("{text}");
        self.notice = Some((text, now + NOTICE_SECONDS));
    }

    /// Per-frame upkeep: capture `selection_after` for fresh transactions,
    /// close an open one once the user has let go, expire the notice, and
    /// recompute the pinned chunk set.
    pub fn settle(
        &mut self,
        selection: Option<u64>,
        released: bool,
        now: f64,
        chunk_of: impl Fn(u64) -> Option<(i32, i32)>,
    ) {
        if self.notice.as_ref().is_some_and(|(_, until)| now > *until) {
            self.notice = None;
        }
        let stacks = self.stacks();
        if let Some(top) = stacks.undo.last_mut()
            && top.fresh
        {
            top.selection_after = selection;
            if !top.open {
                top.fresh = false;
            }
        }
        if released {
            stacks.close_open();
        }

        let mut pinned = HashSet::new();
        for tx in stacks.undo.iter().chain(stacks.redo.iter()) {
            for change in &tx.changes {
                pinned.extend(change.chunks());
                if let Some(chunk) = change.id().and_then(&chunk_of) {
                    pinned.insert(chunk);
                }
            }
        }
        self.pinned = pinned;
    }
}

// ── Applying ─────────────────────────────────────────────────────────────────

/// Raw level mutations, with their bookkeeping. Changes are applied through
/// the same messages the panels send (`PlacedEdited`, `RespawnPlaced`,
/// `ScriptEdit`), marked as replays so nothing records them twice.
#[derive(SystemParam)]
pub struct LevelOps<'w, 's> {
    commands: Commands<'w, 's>,
    asset_server: Res<'w, AssetServer>,
    registry: ResMut<'w, PlacedObjectRegistry>,
    streaming: Res<'w, StreamingState>,
    placed: PlacedTree<'w, 's>,
    bindings: Query<'w, 's, &'static ScriptBindings>,
    selection: ResMut<'w, Selection>,
    lighting: ResMut<'w, LightingState>,
    terrain_world: Option<ResMut<'w, TerrainWorld>>,
    edits: MessageWriter<'w, PlacedEdited>,
    script_edits: MessageWriter<'w, ScriptEdit>,
    respawns: MessageWriter<'w, RespawnPlaced>,
    terrain: TerrainChunks<'w, 's>,
    prefabs: ResMut<'w, PrefabLibrary>,
}

impl LevelOps<'_, '_> {
    fn entity_of(&self, id: u64) -> Option<Entity> {
        self.registry
            .by_id
            .get(&id)
            .copied()
            .filter(|e| self.placed.contains(*e))
    }

    fn chunk_loaded(&self, chunk: (i32, i32)) -> bool {
        self.streaming.loaded.contains(&chunk)
    }

    /// Why `change` cannot be applied right now, if it can't.
    fn check(&self, change: &Change, undo: bool) -> Result<(), String> {
        let spawning = matches!(
            (change, undo),
            (Change::Spawn { .. }, false) | (Change::Despawn { .. }, true)
        );
        match change {
            Change::Spawn { chunk, record } | Change::Despawn { chunk, record } => {
                if spawning {
                    if !self.chunk_loaded(*chunk) {
                        return Err(format!("chunk {chunk:?} is not loaded"));
                    }
                    if self.entity_of(record.id).is_some() {
                        return Err(format!("id {} already exists", record.id));
                    }
                } else if self.entity_of(record.id).is_none() {
                    return Err(format!("id {} is not loaded", record.id));
                }
                Ok(())
            }
            Change::Environment { .. } => self
                .terrain_world
                .as_ref()
                .map(|_| ())
                .ok_or_else(|| "world not loaded".to_owned()),
            Change::PrefabDefinition { .. } => Ok(()),
            Change::Terrain { .. } => {
                if self.terrain.heightmap.is_none() {
                    return Err("terrain not loaded".to_owned());
                }
                match change
                    .chunks()
                    .into_iter()
                    .find(|c| !self.terrain.is_loaded(*c))
                {
                    Some(chunk) => Err(format!("terrain chunk {chunk:?} is not loaded")),
                    None => Ok(()),
                }
            }
            other => {
                let id = other.id().expect("entity-keyed");
                self.entity_of(id)
                    .map(|_| ())
                    .ok_or_else(|| format!("id {id} is not loaded"))
            }
        }
    }

    fn apply(&mut self, change: &Change, undo: bool) {
        match change {
            Change::Spawn { chunk, record } | Change::Despawn { chunk, record } => {
                let spawning = matches!(change, Change::Spawn { .. }) != undo;
                if spawning {
                    spawn_entity_record(
                        &mut self.commands,
                        &self.asset_server,
                        &mut self.registry,
                        &self.prefabs,
                        *chunk,
                        record,
                    );
                    self.registry.mark_dirty(*chunk);
                } else if let Some(entity) = self.entity_of(record.id) {
                    let chunk = self
                        .placed
                        .get(entity)
                        .map(|(_, o, _)| o.chunk)
                        .unwrap_or(*chunk);
                    // A prefab-instance root takes its expanded members with
                    // it — they have no record of their own to despawn by.
                    if let Some(inst) = &record.prefab {
                        for id in inst.member_world_ids() {
                            if let Some(member) = self.entity_of(id) {
                                self.registry.by_id.remove(&id);
                                self.commands.entity(member).despawn();
                            }
                        }
                    }
                    self.registry.by_id.remove(&record.id);
                    self.registry.mark_dirty(chunk);
                    self.commands.entity(entity).despawn();
                }
                self.lighting.stale = true;
            }
            Change::Transform { id, before, after } => {
                let (from, to) = if undo {
                    (after, before)
                } else {
                    (before, after)
                };
                let Some(entity) = self.entity_of(*id) else {
                    return;
                };
                if let Ok((_, _, mut t)) = self.placed.get_mut(entity) {
                    *t = *to;
                }
                self.edits
                    .write(PlacedEdited::transform(entity, *from).replayed());
            }
            Change::Name { id, before, after } => {
                let (from, to) = if undo {
                    (after, before)
                } else {
                    (before, after)
                };
                let Some(entity) = self.entity_of(*id) else {
                    return;
                };
                if let Ok((_, mut obj, _)) = self.placed.get_mut(entity) {
                    obj.name = to.clone();
                }
                self.edits
                    .write(PlacedEdited::name(entity, from.clone()).replayed());
            }
            Change::Kind { id, before, after } => {
                let (from, to) = if undo {
                    (after, before)
                } else {
                    (before, after)
                };
                let Some(entity) = self.entity_of(*id) else {
                    return;
                };
                if let Ok((_, mut obj, _)) = self.placed.get_mut(entity) {
                    obj.kind = to.clone();
                }
                if kind_needs_respawn(from, to) {
                    self.respawns.write(RespawnPlaced::InPlace(entity));
                } else {
                    self.edits
                        .write(PlacedEdited::kind(entity, from.clone()).replayed());
                }
            }
            Change::Parent { id, before, after } => {
                let to = if undo { before } else { after };
                let Some(entity) = self.entity_of(*id) else {
                    return;
                };
                if let Ok((_, mut obj, _)) = self.placed.get_mut(entity) {
                    obj.parent = *to;
                    let chunk = obj.chunk;
                    self.registry.mark_dirty(chunk);
                }
            }
            Change::Scripts { id, before, after } => {
                let to = if undo { before } else { after };
                let Some(entity) = self.entity_of(*id) else {
                    return;
                };
                self.script_edits.write(ScriptEdit::Replace {
                    entity,
                    bindings: to.clone(),
                });
            }
            Change::Environment { before, after } => {
                let to = if undo { before } else { after };
                if let Some(tw) = self.terrain_world.as_deref_mut() {
                    // Through `ResMut`, which is what `apply_environment` listens for.
                    tw.metadata.environment = to.clone();
                    self.lighting.stale = true;
                }
            }
            Change::Terrain { tiles } => {
                self.terrain
                    .set_tiles(tiles.iter().map(|(tx, ty, before, after)| {
                        ((*tx, *ty), if undo { *before } else { *after })
                    }));
                for chunk in change.chunks() {
                    self.terrain.rebuild_meshes(chunk, 0..LOD_LEVELS);
                    self.terrain.rebuild_collider(chunk);
                    self.terrain.mark_dirty(chunk);
                }
                self.lighting.stale = true;
            }
            Change::PrefabDefinition {
                name,
                before,
                after,
            } => {
                let target = if undo { before } else { after };
                // Each loaded instance's deviations are measured against the
                // definition being replaced, then pinned through the rebuild.
                let current = self.prefabs.get(name).cloned();
                let fresh = current
                    .as_ref()
                    .map(|def| {
                        super::prefab_edit::collect_fresh_instances(
                            name,
                            None,
                            def,
                            &self.registry,
                            &self.placed,
                            &self.bindings,
                        )
                    })
                    .unwrap_or_default();
                if let Err(e) = store_prefab(&mut self.prefabs, target.clone()) {
                    error!("Failed to rewrite prefab '{name}': {e}");
                    return;
                }
                for (entity, inst) in fresh {
                    super::prefab_edit::set_instance_and_reexpand(
                        entity,
                        Some(inst),
                        &mut self.commands,
                        &self.asset_server,
                        &mut self.registry,
                        &self.prefabs,
                        &mut self.placed,
                        &self.bindings,
                    );
                }
                self.lighting.stale = true;
            }
            Change::PrefabInstance { id, before, after } => {
                let target = if undo { before } else { after };
                let Some(entity) = self.entity_of(*id) else {
                    return;
                };
                super::prefab_edit::set_instance_and_reexpand(
                    entity,
                    target.clone(),
                    &mut self.commands,
                    &self.asset_server,
                    &mut self.registry,
                    &self.prefabs,
                    &mut self.placed,
                    &self.bindings,
                );
                self.lighting.stale = true;
            }
        }
    }

    /// Registry only, not `entity_of`: an entity this same undo just
    /// respawned exists only as a queued command until the frame's commands
    /// flush. `validate_selection` drops it next frame if it never appears.
    fn select(&mut self, id: Option<u64>) {
        self.selection.primary = id.and_then(|id| self.registry.by_id.get(&id).copied());
    }
}

/// `name`, or the entity's kind label when the name is empty — for labels
/// that quote a *previous* name.
pub fn name_or_kind(name: &str, obj: &PlacedObject) -> String {
    if name.is_empty() {
        obj.kind.label().to_owned()
    } else {
        name.to_owned()
    }
}

/// A kind change that can't be applied in place: a different mesh asset or
/// collider mode (both baked into the spawned scene).
fn kind_needs_respawn(from: &PlacedKind, to: &PlacedKind) -> bool {
    match (from, to) {
        (
            PlacedKind::Mesh {
                asset_path: a,
                collider: ca,
            },
            PlacedKind::Mesh {
                asset_path: b,
                collider: cb,
            },
        ) => a != b || ca != cb,
        (a, b) => std::mem::discriminant(a) != std::mem::discriminant(b),
    }
}

// ── Systems ──────────────────────────────────────────────────────────────────

/// `EditorSet::Prepare`: the stacks follow the loaded world, and the selection
/// at frame start becomes `selection_before` for anything recorded this frame.
pub fn prepare_history(
    world_config: Option<Res<WorldConfig>>,
    selection: Res<Selection>,
    placed: Query<&PlacedObject>,
    mut history: ResMut<History>,
) {
    if let Some(config) = world_config {
        history.set_world(&config.world_id);
    }
    history.selection_now = selection
        .primary
        .and_then(|e| placed.get(e).ok())
        .map(|o| o.id);
}

/// `Ctrl+Z` undo, `Ctrl+Shift+Z` / `Ctrl+Y` redo. `Cmd` works as `Ctrl` on
/// macOS. Gated on `keyboard_free`, so a focused text field keeps egui's own
/// text undo.
pub fn undo_redo_hotkeys(
    keys: Res<ButtonInput<KeyCode>>,
    mut actions: MessageWriter<EditorAction>,
) {
    let ctrl = keys.any_pressed([
        KeyCode::ControlLeft,
        KeyCode::ControlRight,
        KeyCode::SuperLeft,
        KeyCode::SuperRight,
    ]);
    if !ctrl {
        return;
    }
    let shift = keys.any_pressed([KeyCode::ShiftLeft, KeyCode::ShiftRight]);
    if keys.just_pressed(KeyCode::KeyZ) {
        actions.write(if shift {
            EditorAction::Redo
        } else {
            EditorAction::Undo
        });
    } else if keys.just_pressed(KeyCode::KeyY) {
        actions.write(EditorAction::Redo);
    }
}

/// Apply `EditorAction::Undo` / `Redo`. First in the edit chain so the
/// messages it emits are committed the same frame.
///
/// Refused while a gizmo drag or a terrain stroke is in progress; while an
/// entity is carried the press puts it back (Escape semantics) and does
/// nothing else.
pub fn apply_undo_redo(
    mut actions: MessageReader<EditorAction>,
    mut history: ResMut<History>,
    gizmo: Res<GizmoInteraction>,
    brush: Res<TerrainBrushHit>,
    grabbed: Res<GrabbedEntity>,
    mut next_tool: ResMut<NextState<EditorTool>>,
    time: Res<Time>,
    mut ops: LevelOps,
) {
    for action in actions.read() {
        let undo = match action {
            EditorAction::Undo => true,
            EditorAction::Redo => false,
            _ => continue,
        };
        if gizmo.drag.is_some() || brush.was_painting {
            continue;
        }
        if grabbed.0.is_some() {
            next_tool.set(EditorTool::Select);
            continue;
        }
        history.close_open();
        let stacks = history.stacks();
        let Some(tx) = (if undo {
            stacks.undo.pop()
        } else {
            stacks.redo.pop()
        }) else {
            continue;
        };

        // All-or-nothing: a transaction whose target is gone is dropped,
        // never half-applied.
        if let Some(reason) = tx.changes.iter().find_map(|c| ops.check(c, undo).err()) {
            let verb = if undo { "undo" } else { "redo" };
            let text = format!("Can't {verb} {}: {reason}", tx.label);
            history.set_notice(text, time.elapsed_secs_f64());
            continue;
        }

        if undo {
            for change in tx.changes.iter().rev() {
                ops.apply(change, true);
            }
            ops.select(tx.selection_before);
            info!("Undo {}", tx.label);
            history.stacks().redo.push(tx);
        } else {
            for change in &tx.changes {
                ops.apply(change, false);
            }
            ops.select(tx.selection_after);
            info!("Redo {}", tx.label);
            history.stacks().undo.push(tx);
        }
    }
}

/// End of the edit chain: coalescing ends when the pointer is up and egui is
/// idle (no drag, popup or focused field); pinned chunks are recomputed.
pub fn settle_history(
    mut history: ResMut<History>,
    mouse: Res<ButtonInput<MouseButton>>,
    viewport: Res<ViewportInput>,
    selection: Res<Selection>,
    registry: Res<PlacedObjectRegistry>,
    placed: Query<&PlacedObject>,
    time: Res<Time>,
) {
    let selected = selection
        .primary
        .and_then(|e| placed.get(e).ok())
        .map(|o| o.id);
    let released = !mouse.pressed(MouseButton::Left) && !viewport.egui_busy;
    history.settle(selected, released, time.elapsed_secs_f64(), |id| {
        registry
            .by_id
            .get(&id)
            .and_then(|e| placed.get(*e).ok())
            .map(|o| o.chunk)
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tx(label: &str) -> Transaction {
        Transaction::new(label.into(), Vec::new(), None)
    }

    fn t(x: f32) -> Transform {
        Transform::from_xyz(x, 0.0, 0.0)
    }

    #[test]
    fn continuous_edits_coalesce_and_keep_the_first_before() {
        let mut h = History::default();
        for i in 1..=10 {
            h.merge_or_push(
                MergeKey::Transform(7),
                "Move",
                Change::Transform {
                    id: 7,
                    before: t(i as f32 - 1.0),
                    after: t(i as f32),
                },
            );
        }
        assert_eq!(h.undo_len(), 1);
        let top = h.stacks().undo.last().unwrap();
        assert_eq!(
            top.changes,
            vec![Change::Transform {
                id: 7,
                before: t(0.0),
                after: t(10.0)
            }]
        );
    }

    #[test]
    fn a_different_key_closes_the_open_transaction() {
        let mut h = History::default();
        h.merge_or_push(
            MergeKey::Transform(1),
            "Move",
            Change::Transform {
                id: 1,
                before: t(0.0),
                after: t(1.0),
            },
        );
        h.merge_or_push(
            MergeKey::Transform(2),
            "Move",
            Change::Transform {
                id: 2,
                before: t(0.0),
                after: t(1.0),
            },
        );
        assert_eq!(h.undo_len(), 2);
        assert!(!h.stacks().undo[0].open);
    }

    #[test]
    fn a_cancelled_drag_leaves_no_entry() {
        let mut h = History::default();
        h.merge_or_push(
            MergeKey::Transform(1),
            "Move",
            Change::Transform {
                id: 1,
                before: t(0.0),
                after: t(5.0),
            },
        );
        // Cancel restores the start: the coalesced change becomes a no-op.
        h.merge_or_push(
            MergeKey::Transform(1),
            "Move",
            Change::Transform {
                id: 1,
                before: t(5.0),
                after: t(0.0),
            },
        );
        h.close_open();
        assert_eq!(h.undo_len(), 0);
    }

    #[test]
    fn group_member_moves_fold_into_the_group_transaction() {
        let mut h = History::default();
        let tx = h.merge_or_push(
            MergeKey::Transform(1),
            "Move",
            Change::Transform {
                id: 1,
                before: t(0.0),
                after: t(1.0),
            },
        );
        tx.absorb(Change::Transform {
            id: 2,
            before: t(10.0),
            after: t(11.0),
        });
        let tx = h.merge_or_push(
            MergeKey::Transform(1),
            "Move",
            Change::Transform {
                id: 1,
                before: t(1.0),
                after: t(2.0),
            },
        );
        tx.absorb(Change::Transform {
            id: 2,
            before: t(11.0),
            after: t(12.0),
        });
        assert_eq!(h.undo_len(), 1);
        let changes = &h.stacks().undo[0].changes;
        assert_eq!(changes.len(), 2);
        assert!(
            matches!(&changes[1], Change::Transform { id: 2, before, after } if *before == t(10.0) && *after == t(12.0))
        );
    }

    #[test]
    fn push_clears_redo_and_caps_the_stack() {
        let mut h = History::default();
        h.stacks().redo.push(tx("stale"));
        for i in 0..(MAX_TRANSACTIONS + 5) {
            h.push(
                format!("n{i}"),
                vec![Change::Name {
                    id: 1,
                    before: "a".into(),
                    after: "b".into(),
                }],
            );
        }
        assert_eq!(h.redo_len(), 0);
        assert_eq!(h.undo_len(), MAX_TRANSACTIONS);
        assert_eq!(h.stacks().undo[0].label, "n5");
    }

    #[test]
    fn noop_pushes_are_ignored() {
        let mut h = History::default();
        h.push(
            "same",
            vec![Change::Parent {
                id: 1,
                before: None,
                after: None,
            }],
        );
        assert_eq!(h.undo_len(), 0);
    }

    #[test]
    fn stacks_are_per_world() {
        let mut h = History::default();
        h.set_world("a");
        h.push(
            "in a",
            vec![Change::Name {
                id: 1,
                before: "x".into(),
                after: "y".into(),
            }],
        );
        h.set_world("b");
        assert_eq!(h.undo_len(), 0);
        h.set_world("a");
        assert_eq!(h.undo_label(), Some("in a"));
    }

    #[test]
    fn settle_pins_referenced_chunks_and_captures_selection() {
        let mut h = History::default();
        h.push(
            "del",
            vec![Change::Despawn {
                chunk: (3, 4),
                record: PlacedEntityRecord::new(9, PlacedKind::Empty, &Transform::default()),
            }],
        );
        h.push(
            "move",
            vec![Change::Transform {
                id: 5,
                before: t(0.0),
                after: t(1.0),
            }],
        );
        h.settle(Some(5), true, 0.0, |id| (id == 5).then_some((1, 1)));
        assert!(h.pinned.contains(&(3, 4)));
        assert!(h.pinned.contains(&(1, 1)));
        assert_eq!(h.stacks().undo[1].selection_after, Some(5));
    }

    #[test]
    fn mesh_asset_or_collider_changes_need_a_respawn() {
        use crate::game::world::objects::ColliderMode;
        let a = PlacedKind::mesh("a");
        let b = PlacedKind::mesh("b");
        let mut a_no = a.clone();
        if let PlacedKind::Mesh { collider, .. } = &mut a_no {
            *collider = ColliderMode::None;
        }
        assert!(kind_needs_respawn(&a, &b));
        assert!(kind_needs_respawn(&a, &a_no));
        assert!(!kind_needs_respawn(&a, &a));
        assert!(!kind_needs_respawn(
            &PlacedKind::point_light(),
            &PlacedKind::point_light()
        ));
    }

    // ── Round trips through the real systems ─────────────────────────────────

    use super::super::actions::{EditorAction, apply_selection_actions};
    use super::super::edits::{
        PlacedEdited, ScriptEdit, apply_script_edits, commit_placed_edits, respawn_placed,
    };
    use crate::game::world::objects::PlacedObjectRegistry;
    use crate::game::world::terrain::{HeightmapData, TILE_SIZE};

    fn test_app() -> App {
        let mut app = App::new();
        app.add_plugins((
            MinimalPlugins,
            bevy::asset::AssetPlugin::default(),
            bevy::state::app::StatesPlugin,
        ));
        app.init_state::<EditorTool>();
        app.init_resource::<History>()
            .init_resource::<crate::game::world::prefabs::PrefabLibrary>()
            .init_resource::<super::super::prefab_edit::PrefabEditState>()
            .init_resource::<super::super::thumbnails::ThumbnailCache>()
            .init_resource::<Selection>()
            .init_resource::<LightingState>()
            .init_resource::<PlacedObjectRegistry>()
            .init_resource::<StreamingState>()
            .init_resource::<GizmoInteraction>()
            .init_resource::<GrabbedEntity>()
            .init_resource::<TerrainBrushHit>()
            .init_resource::<Assets<Mesh>>()
            .init_resource::<ViewportInput>()
            .init_resource::<ButtonInput<MouseButton>>()
            .init_resource::<engine_api::ScriptRegistry>();
        app.add_message::<PlacedEdited>()
            .add_message::<ScriptEdit>()
            .add_message::<RespawnPlaced>()
            .add_message::<EditorAction>()
            .add_message::<super::super::prefab_edit::PrefabAction>();
        app.add_systems(
            Update,
            (
                prepare_history,
                apply_undo_redo,
                apply_selection_actions,
                super::super::prefab_edit::apply_prefab_actions,
                commit_placed_edits,
                apply_script_edits,
                respawn_placed,
                settle_history,
            )
                .chain(),
        );
        app.world_mut()
            .resource_mut::<StreamingState>()
            .loaded
            .insert((0, 0));
        app
    }

    /// Spawn a placed entity the way the world would, and register it.
    fn spawn(
        app: &mut App,
        id: u64,
        kind: PlacedKind,
        parent: Option<u64>,
        at: Transform,
    ) -> Entity {
        let record = PlacedEntityRecord::new(id, kind, &at).with_parent(parent);
        let entity = app
            .world_mut()
            .spawn((
                PlacedObject {
                    id,
                    chunk: (0, 0),
                    name: String::new(),
                    kind: record.kind.clone(),
                    parent,
                    prefab: None,
                },
                at,
            ))
            .id();
        let mut registry = app.world_mut().resource_mut::<PlacedObjectRegistry>();
        registry.by_id.insert(id, entity);
        registry.observe_id(id);
        registry.ids_seeded = true;
        entity
    }

    fn entity_of(app: &mut App, id: u64) -> Option<Entity> {
        let e = *app
            .world()
            .resource::<PlacedObjectRegistry>()
            .by_id
            .get(&id)?;
        app.world().get_entity(e).ok().map(|e| e.id())
    }

    fn action(app: &mut App, a: EditorAction) {
        app.world_mut().write_message(a);
        app.update();
    }

    fn transform_of(app: &mut App, id: u64) -> Transform {
        let e = entity_of(app, id).expect("loaded");
        *app.world().get::<Transform>(e).unwrap()
    }

    #[test]
    fn transform_edit_round_trips() {
        let mut app = test_app();
        let e = spawn(&mut app, 1, PlacedKind::Empty, None, t(0.0));
        app.update();

        // A panel edit: write, report with the before value.
        *app.world_mut().get_mut::<Transform>(e).unwrap() = t(3.0);
        app.world_mut()
            .write_message(PlacedEdited::transform(e, t(0.0)));
        app.update(); // records (open), then settles closed (mouse up, egui idle)

        action(&mut app, EditorAction::Undo);
        assert_eq!(transform_of(&mut app, 1), t(0.0));
        action(&mut app, EditorAction::Redo);
        assert_eq!(transform_of(&mut app, 1), t(3.0));
        assert!(
            app.world()
                .resource::<PlacedObjectRegistry>()
                .dirty_chunks
                .contains(&(0, 0))
        );
    }

    #[test]
    fn delete_round_trips_with_the_same_id_and_selection() {
        let mut app = test_app();
        let e = spawn(&mut app, 1, PlacedKind::Empty, None, t(2.0));
        app.world_mut().resource_mut::<Selection>().primary = Some(e);
        app.update();

        action(&mut app, EditorAction::DeleteSelection);
        assert!(entity_of(&mut app, 1).is_none());

        action(&mut app, EditorAction::Undo);
        let back = entity_of(&mut app, 1).expect("respawned");
        assert_ne!(back, e, "a fresh entity, same id");
        assert_eq!(transform_of(&mut app, 1), t(2.0));
        assert_eq!(app.world().resource::<Selection>().primary, Some(back));

        action(&mut app, EditorAction::Redo);
        assert!(entity_of(&mut app, 1).is_none());
    }

    #[test]
    fn deleting_a_group_and_undoing_restores_member_parents() {
        let mut app = test_app();
        let group = spawn(&mut app, 1, PlacedKind::Group, None, t(0.0));
        let member = spawn(&mut app, 2, PlacedKind::Empty, Some(1), t(5.0));
        app.world_mut().resource_mut::<Selection>().primary = Some(group);
        app.update();

        action(&mut app, EditorAction::DeleteSelection);
        assert_eq!(
            app.world().get::<PlacedObject>(member).unwrap().parent,
            None
        );

        action(&mut app, EditorAction::Undo);
        assert!(entity_of(&mut app, 1).is_some());
        assert_eq!(
            app.world().get::<PlacedObject>(member).unwrap().parent,
            Some(1)
        );
    }

    #[test]
    fn group_move_undo_restores_members_exactly_without_recascading() {
        let mut app = test_app();
        let group = spawn(&mut app, 1, PlacedKind::Group, None, t(0.0));
        let _member = spawn(&mut app, 2, PlacedKind::Empty, Some(1), t(5.0));
        app.update(); // commit snapshots the group transform

        *app.world_mut().get_mut::<Transform>(group).unwrap() = t(1.0);
        app.world_mut()
            .write_message(PlacedEdited::transform(group, t(0.0)));
        app.update();
        assert_eq!(transform_of(&mut app, 2), t(6.0), "member followed");

        action(&mut app, EditorAction::Undo);
        assert_eq!(transform_of(&mut app, 1), t(0.0));
        assert_eq!(transform_of(&mut app, 2), t(5.0));
        action(&mut app, EditorAction::Redo);
        assert_eq!(transform_of(&mut app, 1), t(1.0));
        assert_eq!(transform_of(&mut app, 2), t(6.0));
    }

    #[test]
    fn script_attach_round_trips() {
        use engine_api::ScriptBindings;
        let mut app = test_app();
        let e = spawn(&mut app, 1, PlacedKind::Empty, None, t(0.0));
        app.update();

        app.world_mut().write_message(ScriptEdit::Attach {
            entity: e,
            plugin: "p".into(),
            script: "s".into(),
        });
        app.update();
        assert_eq!(app.world().get::<ScriptBindings>(e).unwrap().0.len(), 1);

        action(&mut app, EditorAction::Undo);
        assert!(app.world().get::<ScriptBindings>(e).is_none());
        action(&mut app, EditorAction::Redo);
        assert_eq!(
            app.world().get::<ScriptBindings>(e).unwrap().0[0].script,
            "s"
        );
    }

    #[test]
    fn an_unresolvable_target_skips_and_drops_the_transaction() {
        let mut app = test_app();
        app.update();
        app.world_mut().resource_mut::<History>().push(
            "ghost",
            vec![Change::Transform {
                id: 99,
                before: t(0.0),
                after: t(1.0),
            }],
        );
        action(&mut app, EditorAction::Undo);
        let history = app.world().resource::<History>();
        assert_eq!(history.undo_len(), 0);
        assert_eq!(history.redo_len(), 0);
        assert!(history.notice().is_some_and(|n| n.contains("ghost")));
    }

    #[test]
    fn selection_before_is_captured_from_frame_start() {
        let mut app = test_app();
        let a = spawn(&mut app, 1, PlacedKind::Empty, None, t(0.0));
        let b = spawn(&mut app, 2, PlacedKind::Empty, None, t(0.0));
        app.world_mut().resource_mut::<Selection>().primary = Some(a);
        app.update();
        // Rename b while a is selected; undo should bring a back as selection.
        app.world_mut().resource_mut::<Selection>().primary = Some(b);
        app.world_mut().get_mut::<PlacedObject>(b).unwrap().name = "bee".into();
        app.world_mut()
            .write_message(PlacedEdited::name(b, String::new()));
        app.update();
        action(&mut app, EditorAction::Undo);
        assert_eq!(app.world().get::<PlacedObject>(b).unwrap().name, "");
        assert_eq!(app.world().resource::<Selection>().primary, Some(b));
    }

    #[test]
    fn terrain_strokes_round_trip_through_the_heightmap() {
        use crate::game::world::terrain::storage::TerrainChunkRegistry;
        let mut app = test_app();
        app.insert_resource(HeightmapData::new(TILE_SIZE));
        // The chunk counts as loaded for the undo check even with no mesh
        // entity behind it; rebuilds just find nothing to regenerate.
        let mut registry = TerrainChunkRegistry::default();
        registry.loaded_chunks.insert((0, 0), Entity::PLACEHOLDER);
        app.insert_resource(registry);
        app.update();

        app.world_mut()
            .resource_mut::<HeightmapData>()
            .set_tile(3, 4, 2.0);
        app.world_mut().resource_mut::<History>().push(
            "Sculpt terrain",
            vec![Change::Terrain {
                tiles: vec![(3, 4, 0.0, 2.0)],
            }],
        );
        app.update();

        action(&mut app, EditorAction::Undo);
        assert_eq!(app.world().resource::<HeightmapData>().get_tile(3, 4), 0.0);
        assert!(
            app.world()
                .resource::<TerrainChunkRegistry>()
                .dirty_chunks
                .contains(&(0, 0))
        );
        action(&mut app, EditorAction::Redo);
        assert_eq!(app.world().resource::<HeightmapData>().get_tile(3, 4), 2.0);
    }

    #[test]
    fn terrain_memory_cap_sheds_the_oldest_strokes_first() {
        let mut h = History::default();
        let big = || Change::Terrain {
            tiles: vec![(0, 0, 0.0, 1.0); MAX_TERRAIN_BYTES / 16 / 2 + 1],
        };
        h.push("first stroke", vec![big()]);
        h.push(
            "rename",
            vec![Change::Name {
                id: 1,
                before: "a".into(),
                after: "b".into(),
            }],
        );
        h.push("second stroke", vec![big()]);
        h.push("third stroke", vec![big()]);
        let labels: Vec<&str> = h.stacks().undo.iter().map(|t| t.label.as_str()).collect();
        assert_eq!(labels, vec!["rename", "third stroke"]);
    }

    // ── Prefab instances ─────────────────────────────────────────────────────

    use bevy::ecs::system::IntoSystem;

    use crate::game::world::prefabs::{PrefabInstanceData, PrefabLibrary, define_from_records};

    /// A two-entity prefab in the library, plus a live instance of it spawned
    /// through the real spawn path.
    fn spawn_instance(app: &mut App, root_id: u64, member_id: u64) -> (Entity, u64) {
        let mut def_root = PlacedEntityRecord::new(0, PlacedKind::Group, &t(0.0));
        def_root.name = "combo".into();
        let member = PlacedEntityRecord::new(1, PlacedKind::Empty, &t(1.0));
        let (def, _) = define_from_records("combo", &def_root, &[member]);
        app.world_mut()
            .resource_mut::<PrefabLibrary>()
            .prefabs
            .insert("combo".into(), def);

        let mut record = PlacedEntityRecord::new(root_id, PlacedKind::Group, &t(5.0));
        record.prefab = Some(PrefabInstanceData {
            name: "combo".into(),
            member_ids: vec![(1, member_id)],
            overrides: Vec::new(),
        });
        let mut system = IntoSystem::into_system(
            move |mut commands: Commands,
                  asset_server: Res<AssetServer>,
                  prefabs: Res<PrefabLibrary>,
                  mut registry: ResMut<PlacedObjectRegistry>|
                  -> Entity {
                registry.ids_seeded = true;
                registry.observe_id(root_id.max(member_id));
                spawn_entity_record(
                    &mut commands,
                    &asset_server,
                    &mut registry,
                    &prefabs,
                    (0, 0),
                    &record,
                )
            },
        );
        system.initialize(app.world_mut());
        let root = system.run((), app.world_mut()).unwrap();
        system.apply_deferred(app.world_mut());
        app.update();
        (root, member_id)
    }

    #[test]
    fn an_instance_expands_its_members_in_the_roots_chunk() {
        let mut app = test_app();
        let (root, member_id) = spawn_instance(&mut app, 10, 11);
        let member = entity_of(&mut app, member_id).expect("member expanded");
        let obj = app.world().get::<PlacedObject>(member).unwrap();
        assert_eq!(obj.parent, Some(10), "member filed under the root");
        assert_eq!(obj.chunk, (0, 0), "member lives in the root's chunk");
        assert_eq!(
            transform_of(&mut app, member_id),
            t(6.0),
            "root at 5 + local 1"
        );
        let root_obj = app.world().get::<PlacedObject>(root).unwrap();
        assert_eq!(
            root_obj.name, "combo",
            "root identity comes from the definition"
        );
    }

    #[test]
    fn deleting_an_instance_root_takes_members_and_undo_restores_both_ids() {
        let mut app = test_app();
        let (root, member_id) = spawn_instance(&mut app, 10, 11);
        app.world_mut().resource_mut::<Selection>().primary = Some(root);
        app.update();

        action(&mut app, EditorAction::DeleteSelection);
        assert!(entity_of(&mut app, 10).is_none());
        assert!(
            entity_of(&mut app, member_id).is_none(),
            "member went with the root"
        );
        assert_eq!(
            app.world().resource::<History>().undo_len(),
            1,
            "one transaction; the expanded member has no change of its own"
        );

        action(&mut app, EditorAction::Undo);
        assert!(entity_of(&mut app, 10).is_some());
        assert!(
            entity_of(&mut app, member_id).is_some(),
            "undo re-expanded the member with its stable id"
        );
    }

    #[test]
    fn duplicating_an_instance_allocates_fresh_member_ids() {
        let mut app = test_app();
        let (root, member_id) = spawn_instance(&mut app, 10, 11);
        app.world_mut().resource_mut::<Selection>().primary = Some(root);
        app.update();
        action(&mut app, EditorAction::DuplicateSelection);
        app.update(); // respawn_placed ran; commands applied

        let copy = app
            .world()
            .resource::<Selection>()
            .primary
            .expect("copy selected");
        let copy_obj = app.world().get::<PlacedObject>(copy).unwrap();
        let inst = copy_obj.prefab.as_ref().expect("copy is an instance");
        assert_ne!(copy_obj.id, 10);
        assert_eq!(inst.member_ids.len(), 1);
        let copy_member = inst.world_id_of(1).unwrap();
        assert_ne!(copy_member, member_id, "no shared member ids");
        assert!(entity_of(&mut app, copy_member).is_some());
    }

    #[test]
    fn unpack_and_its_undo_round_trip_the_link() {
        use super::super::prefab_edit::PrefabAction;
        let mut app = test_app();
        let (root, member_id) = spawn_instance(&mut app, 10, 11);
        app.world_mut().write_message(PrefabAction::Unpack(root));
        app.update();
        assert!(
            app.world()
                .get::<PlacedObject>(root)
                .unwrap()
                .prefab
                .is_none(),
            "link severed"
        );
        assert!(entity_of(&mut app, member_id).is_some(), "entities stay");

        action(&mut app, EditorAction::Undo);
        let root = entity_of(&mut app, 10).expect("root survives (possibly respawned)");
        assert!(
            app.world()
                .get::<PlacedObject>(root)
                .unwrap()
                .prefab
                .is_some(),
            "undo restores the link"
        );
        assert!(entity_of(&mut app, member_id).is_some());
    }
}
