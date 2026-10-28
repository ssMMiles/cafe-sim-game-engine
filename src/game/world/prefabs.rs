//! Prefabs: reusable entity templates with **linked instances**.
//!
//! A prefab definition ([`PrefabFile`], `assets/prefabs/{name}.prefab`) is a
//! list of [`PlacedEntityRecord`]s with **local** ids: `entities[0]` is the
//! root (local id 0, identity transform), members use local ids `1..` for
//! `parent` and `PropValue::EntityRef`, and member transforms are relative to
//! the root.
//!
//! An instance in a level is **one record**: the root, carrying
//! [`PrefabInstanceData`] — the prefab name, one stable world id per member
//! (`member_ids[k]` ↔ `entities[k + 1]`, stable so baked lightmaps and
//! `EntityRef`s survive re-expansion), and this instance's deviations from
//! the definition ([`PrefabOverride`], per member and per *section*:
//! transform / name / kind / scripts each override independently). Members
//! are **never written to chunk files**; they are expanded from the library
//! when the root spawns, which is what makes editing a prefab propagate to
//! every instance, loaded or not. The root's own kind/name/scripts also come
//! from `entities[0]` (+ a local-0 override), so single-entity prefabs are
//! fully linked too.
//!
//! An instance whose prefab is missing from the library spawns as a bare
//! root with its data intact (no members); the status bar flags it, and it
//! heals when the prefab reappears. Nested prefabs are refused for now.
//!
//! See `docs/PREFABS_PLAN.md`.

use std::collections::HashMap;

use bevy::prelude::*;
use engine_api::{PropValue, ScriptBinding};
use rkyv::{Archive, Deserialize, Serialize, rancor::Error as RkyvError};

use super::objects::{PlacedEntityRecord, PlacedKind, quat_to_array};
use super::terrain::storage::StorageError;

pub const PREFAB_FORMAT_VERSION: u32 = 1;
const PREFAB_MAGIC: &[u8; 4] = b"PFAB";

/// Where prefab definitions live. Native: a real directory; web: the OPFS
/// key prefix.
pub const PREFAB_DIR: &str = "assets/prefabs";

// ============================================================================
// Data
// ============================================================================

/// A prefab definition, as stored on disk.
#[derive(Archive, Deserialize, Serialize, Clone, Debug, PartialEq)]
pub struct PrefabFile {
    pub version: u32,
    pub name: String,
    /// `entities[0]` is the root: local id 0, identity transform. Members
    /// follow with local ids `1..`; `parent` and `EntityRef` props use local
    /// ids. Member records never carry `prefab` (no nesting).
    pub entities: Vec<PlacedEntityRecord>,
}

/// Lives on an instance's **root** record (and mirrored on its
/// `PlacedObject`): everything needed to expand the instance from the
/// definition.
#[derive(Archive, Deserialize, Serialize, Clone, Debug, PartialEq)]
pub struct PrefabInstanceData {
    pub name: String,
    /// `(definition local id, this instance's world id)` for each member.
    /// Explicit pairs rather than positions, so editing the definition
    /// (adding or removing members) can never shift which world id — and
    /// therefore which baked lightmap and `EntityRef` target — a member
    /// means in other instances. Stable across re-expansion.
    pub member_ids: Vec<(u64, u64)>,
    pub overrides: Vec<PrefabOverride>,
}

impl PrefabInstanceData {
    pub fn world_id_of(&self, local: u64) -> Option<u64> {
        self.member_ids
            .iter()
            .find(|(l, _)| *l == local)
            .map(|(_, id)| *id)
    }

    /// Every member world id of this instance.
    pub fn member_world_ids(&self) -> impl Iterator<Item = u64> + '_ {
        self.member_ids.iter().map(|(_, id)| *id)
    }
}

/// One member's deviations from the definition. `local` 0 is the root
/// (whose transform is never an override — an instance is free to sit
/// anywhere). A `None` section tracks the prefab; a `Some` section is
/// pinned to this instance's value.
#[derive(Archive, Deserialize, Serialize, Clone, Debug, PartialEq, Default)]
pub struct PrefabOverride {
    pub local: u32,
    /// The member was deleted in this instance.
    pub removed: bool,
    /// Root-relative (position, rotation xyzw, scale).
    pub transform: Option<([f32; 3], [f32; 4], [f32; 3])>,
    pub name: Option<String>,
    pub kind: Option<PlacedKind>,
    pub scripts: Option<Vec<ScriptBinding>>,
}

impl PrefabOverride {
    fn is_empty(&self) -> bool {
        !self.removed
            && self.transform.is_none()
            && self.name.is_none()
            && self.kind.is_none()
            && self.scripts.is_none()
    }
}

/// Every loaded prefab definition, by name. Global (not per world).
#[derive(Resource, Default)]
pub struct PrefabLibrary {
    pub prefabs: HashMap<String, PrefabFile>,
}

impl PrefabLibrary {
    pub fn get(&self, name: &str) -> Option<&PrefabFile> {
        self.prefabs.get(name)
    }

    /// Names, sorted, for stable UI listings.
    pub fn names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.prefabs.keys().cloned().collect();
        names.sort();
        names
    }
}

// ============================================================================
// Transform helpers (records store arrays)
// ============================================================================

fn record_transform(r: &PlacedEntityRecord) -> Transform {
    r.transform()
}

fn set_record_transform(r: &mut PlacedEntityRecord, t: Transform) {
    r.position = t.translation.into();
    r.rotation = quat_to_array(t.rotation);
    r.scale = t.scale.into();
}

/// `member` expressed relative to `root` (both world).
fn relative_transform(root: Transform, member: Transform) -> Transform {
    Transform::from_matrix(root.to_matrix().inverse() * member.to_matrix())
}

/// Transforms compared through the same matrix round trip used to produce
/// them, so a define → expand cycle is a fixed point rather than a drift.
fn transforms_match(a: Transform, b: Transform) -> bool {
    let (a, b) = (a.to_matrix(), b.to_matrix());
    (0..4).all(|c| (a.col(c) - b.col(c)).abs().max_element() < 1e-4)
}

// ============================================================================
// Expansion (definition + instance → world records)
// ============================================================================

/// Map a definition-local id to this instance's world id. Local 0 is the
/// root. Unknown locals (a ref to a member this instance doesn't have) pass
/// through unchanged.
fn local_to_world(local: u64, root_id: u64, inst: &PrefabInstanceData) -> u64 {
    if local == 0 {
        return root_id;
    }
    inst.world_id_of(local).unwrap_or(local)
}

fn remap_refs(scripts: &mut [ScriptBinding], map: impl Fn(u64) -> u64) {
    for binding in scripts {
        for prop in &mut binding.props {
            if let PropValue::EntityRef(id) = &mut prop.value {
                *id = map(*id);
            }
        }
    }
}

fn override_for(inst: &PrefabInstanceData, local: u32) -> Option<&PrefabOverride> {
    inst.overrides.iter().find(|o| o.local == local)
}

/// Apply a member's override sections onto its definition record (local-id
/// space, root-relative transform).
fn merged_local_record(
    def: &PlacedEntityRecord,
    ov: Option<&PrefabOverride>,
) -> PlacedEntityRecord {
    let mut record = def.clone();
    let Some(ov) = ov else {
        return record;
    };
    if let Some((p, r, s)) = ov.transform {
        record.position = p;
        record.rotation = r;
        record.scale = s;
    }
    if let Some(name) = &ov.name {
        record.name = name.clone();
    }
    if let Some(kind) = &ov.kind {
        record.kind = kind.clone();
    }
    if let Some(scripts) = &ov.scripts {
        record.scripts = scripts.clone();
    }
    record
}

/// The world-space records of an instance: the **effective root** first
/// (definition root merged with the stored root record's identity — id,
/// transform, outliner parent — and any local-0 override), then the members.
/// Removed members are absent. The stored `root` record's kind/name/scripts
/// are ignored in favour of the definition, which is what makes instances
/// linked.
pub fn expand_records(def: &PrefabFile, root: &PlacedEntityRecord) -> Vec<PlacedEntityRecord> {
    let inst = root.prefab.as_ref().expect("expanding a non-instance");
    let root_t = record_transform(root);
    let map = |l: u64| local_to_world(l, root.id, inst);

    let mut out = Vec::with_capacity(def.entities.len());

    let mut effective_root = merged_local_record(&def.entities[0], override_for(inst, 0));
    effective_root.id = root.id;
    effective_root.parent = root.parent;
    effective_root.prefab = root.prefab.clone();
    set_record_transform(&mut effective_root, root_t);
    remap_refs(&mut effective_root.scripts, map);
    out.push(effective_root);

    for member_def in def.entities.iter().skip(1) {
        let local = member_def.id as u32;
        let ov = override_for(inst, local);
        if ov.is_some_and(|o| o.removed) {
            continue;
        }
        let Some(id) = inst.world_id_of(member_def.id) else {
            // Definition grew since this instance last re-expanded with
            // fresh ids; skip rather than inventing an id (re-expanding from
            // the editor heals it).
            warn!(
                "prefab '{}': no member id for local {local}; member skipped",
                def.name
            );
            continue;
        };
        let mut record = merged_local_record(member_def, ov);
        record.id = id;
        record.parent = Some(map(record.parent.unwrap_or(0)));
        record.prefab = None;
        let world_t =
            Transform::from_matrix(root_t.to_matrix() * record_transform(&record).to_matrix());
        set_record_transform(&mut record, world_t);
        remap_refs(&mut record.scripts, map);
        out.push(record);
    }
    out
}

// ============================================================================
// Definition (live records → prefab)
// ============================================================================

/// Build a definition from live world records, and the instance data that
/// turns those same entities into its first instance. `root` keeps its
/// world id; members are sorted by id for stable local numbering.
///
/// `EntityRef`s pointing outside the prefab are kept as world ids with a
/// warning — they will dangle in other instances.
pub fn define_from_records(
    name: &str,
    root: &PlacedEntityRecord,
    members: &[PlacedEntityRecord],
) -> (PrefabFile, PrefabInstanceData) {
    let mut members: Vec<PlacedEntityRecord> = members.to_vec();
    members.sort_by_key(|m| m.id);

    let mut world_to_local: HashMap<u64, u64> = HashMap::new();
    world_to_local.insert(root.id, 0);
    for (k, m) in members.iter().enumerate() {
        world_to_local.insert(m.id, (k + 1) as u64);
    }
    let map = |id: u64| {
        world_to_local.get(&id).copied().unwrap_or_else(|| {
            warn!("prefab '{name}': EntityRef {id} points outside the prefab; kept as-is");
            id
        })
    };

    let root_t = record_transform(root);
    let mut entities = Vec::with_capacity(members.len() + 1);

    let mut def_root = root.clone();
    def_root.id = 0;
    def_root.parent = None;
    def_root.prefab = None;
    set_record_transform(&mut def_root, Transform::IDENTITY);
    remap_refs(&mut def_root.scripts, map);
    entities.push(def_root);

    let member_ids: Vec<(u64, u64)> = members
        .iter()
        .enumerate()
        .map(|(k, m)| ((k + 1) as u64, m.id))
        .collect();
    for (k, m) in members.iter().enumerate() {
        let mut def_m = m.clone();
        def_m.id = (k + 1) as u64;
        // A member parented under something outside the prefab re-files
        // under the root.
        def_m.parent = Some(
            m.parent
                .and_then(|p| world_to_local.get(&p).copied())
                .unwrap_or(0),
        );
        def_m.prefab = None;
        set_record_transform(&mut def_m, relative_transform(root_t, record_transform(m)));
        remap_refs(&mut def_m.scripts, map);
        entities.push(def_m);
    }

    (
        PrefabFile {
            version: PREFAB_FORMAT_VERSION,
            name: name.to_owned(),
            entities,
        },
        PrefabInstanceData {
            name: name.to_owned(),
            member_ids,
            overrides: Vec::new(),
        },
    )
}

/// Rebuild a definition from a live instance ("Apply to prefab"), keeping
/// every surviving member's **local id** from the old definition so other
/// instances' `(local, world)` pairs stay meaningful; brand-new members
/// (extras the user added under the root) get fresh locals appended. Returns
/// the new definition and this instance's updated pairs (now override-free by
/// construction).
pub fn define_from_instance(
    old_def: &PrefabFile,
    inst: &PrefabInstanceData,
    root: &PlacedEntityRecord,
    members: &[PlacedEntityRecord],
) -> (PrefabFile, PrefabInstanceData) {
    let mut members: Vec<PlacedEntityRecord> = members.to_vec();
    members.sort_by_key(|m| m.id);

    let world_to_old_local: HashMap<u64, u64> = inst
        .member_ids
        .iter()
        .map(|&(local, world)| (world, local))
        .collect();
    let mut next_local = old_def
        .entities
        .iter()
        .map(|e| e.id)
        .max()
        .unwrap_or(0)
        .max(inst.member_ids.iter().map(|&(l, _)| l).max().unwrap_or(0))
        + 1;

    let mut pairs: Vec<(u64, u64)> = Vec::with_capacity(members.len());
    let mut world_to_local: HashMap<u64, u64> = HashMap::new();
    world_to_local.insert(root.id, 0);
    for m in &members {
        let local = world_to_old_local.get(&m.id).copied().unwrap_or_else(|| {
            let l = next_local;
            next_local += 1;
            l
        });
        pairs.push((local, m.id));
        world_to_local.insert(m.id, local);
    }

    let map = |id: u64| {
        world_to_local.get(&id).copied().unwrap_or_else(|| {
            warn!(
                "prefab '{}': EntityRef {id} points outside the prefab; kept as-is",
                old_def.name
            );
            id
        })
    };

    let root_t = record_transform(root);
    let mut entities = Vec::with_capacity(members.len() + 1);
    let mut def_root = root.clone();
    def_root.id = 0;
    def_root.parent = None;
    def_root.prefab = None;
    set_record_transform(&mut def_root, Transform::IDENTITY);
    remap_refs(&mut def_root.scripts, map);
    entities.push(def_root);

    for (m, &(local, _)) in members.iter().zip(&pairs) {
        let mut def_m = m.clone();
        def_m.id = local;
        def_m.parent = Some(
            m.parent
                .and_then(|p| world_to_local.get(&p).copied())
                .unwrap_or(0),
        );
        def_m.prefab = None;
        set_record_transform(&mut def_m, relative_transform(root_t, record_transform(m)));
        remap_refs(&mut def_m.scripts, map);
        entities.push(def_m);
    }
    // Definition order: root, then members by local id, so expansion spawns
    // parents before children deterministically.
    entities[1..].sort_by_key(|e| e.id);

    (
        PrefabFile {
            version: PREFAB_FORMAT_VERSION,
            name: old_def.name.clone(),
            entities,
        },
        PrefabInstanceData {
            name: old_def.name.clone(),
            member_ids: pairs,
            overrides: Vec::new(),
        },
    )
}

// ============================================================================
// Overrides (live instance vs definition)
// ============================================================================

/// Diff one instance against its definition. `member_record_of(world id)`
/// returns the member's live record, or `None` if it no longer exists
/// (members live and die with the root, so absent means removed). Sections
/// that match the definition stay `None` and keep tracking it.
pub fn compute_instance_overrides(
    def: &PrefabFile,
    inst: &PrefabInstanceData,
    root: &PlacedEntityRecord,
    member_record_of: impl Fn(u64) -> Option<PlacedEntityRecord>,
) -> Vec<PrefabOverride> {
    let root_t = record_transform(root);
    let mut out = Vec::new();

    // Local 0: the root's own sections (transform is the instance's own by
    // nature, never an override). The root record passed in is the *live*
    // one, whose kind/name/scripts mirror the expanded entity.
    let mut ov = diff_sections(&def.entities[0], root, 0, inst, root.id);
    ov.transform = None;
    if !ov.is_empty() {
        out.push(ov);
    }

    for member_def in def.entities.iter().skip(1) {
        let local = member_def.id as u32;
        let Some(id) = inst.world_id_of(member_def.id) else {
            continue;
        };
        let Some(live) = member_record_of(id) else {
            out.push(PrefabOverride {
                local,
                removed: true,
                ..default()
            });
            continue;
        };
        // Compare in local space: the live record is world-space.
        let mut local_live = live.clone();
        set_record_transform(
            &mut local_live,
            relative_transform(root_t, record_transform(&live)),
        );
        let ov = diff_sections(member_def, &local_live, local, inst, root.id);
        if !ov.is_empty() {
            out.push(ov);
        }
    }
    out
}

/// Section-level diff of a live local-space record against its definition.
fn diff_sections(
    def: &PlacedEntityRecord,
    live: &PlacedEntityRecord,
    local: u32,
    inst: &PrefabInstanceData,
    root_id: u64,
) -> PrefabOverride {
    // The definition's refs are local ids; the live entity's are world ids.
    // Map the definition forward for comparison, so an untouched binding
    // isn't misread as an override.
    let mut def_scripts = def.scripts.clone();
    remap_refs(&mut def_scripts, |l| local_to_world(l, root_id, inst));

    PrefabOverride {
        local,
        removed: false,
        transform: (!transforms_match(record_transform(def), record_transform(live)))
            .then(|| (live.position, live.rotation, live.scale)),
        name: (def.name != live.name).then(|| live.name.clone()),
        kind: (def.kind != live.kind).then(|| live.kind.clone()),
        scripts: (def_scripts != live.scripts).then(|| live.scripts.clone()),
    }
}

// ============================================================================
// Serialization (same magic + version + crc frame as chunk files)
// ============================================================================

pub fn serialize_prefab(file: &PrefabFile) -> Result<Vec<u8>, StorageError> {
    let rkyv_bytes = rkyv::to_bytes::<RkyvError>(file)
        .map_err(|e| StorageError::SerializationError(e.to_string()))?;
    let checksum = crc32fast::hash(&rkyv_bytes);
    let mut out = Vec::with_capacity(16 + rkyv_bytes.len());
    out.extend_from_slice(PREFAB_MAGIC);
    out.extend_from_slice(&PREFAB_FORMAT_VERSION.to_le_bytes());
    out.extend_from_slice(&(rkyv_bytes.len() as u32).to_le_bytes());
    out.extend_from_slice(&rkyv_bytes);
    out.extend_from_slice(&checksum.to_le_bytes());
    Ok(out)
}

pub fn parse_prefab(bytes: &[u8]) -> Result<PrefabFile, StorageError> {
    use std::io::Read;
    let mut cursor = std::io::Cursor::new(bytes);
    let mut magic = [0u8; 4];
    cursor.read_exact(&mut magic)?;
    if &magic != PREFAB_MAGIC {
        return Err(StorageError::InvalidMagic);
    }
    let mut word = [0u8; 4];
    cursor.read_exact(&mut word)?;
    let version = u32::from_le_bytes(word);
    if version != PREFAB_FORMAT_VERSION {
        return Err(StorageError::VersionMismatch {
            expected: PREFAB_FORMAT_VERSION,
            found: version,
        });
    }
    cursor.read_exact(&mut word)?;
    let len = u32::from_le_bytes(word) as usize;
    let mut data = vec![0u8; len];
    cursor.read_exact(&mut data)?;
    cursor.read_exact(&mut word)?;
    if u32::from_le_bytes(word) != crc32fast::hash(&data) {
        return Err(StorageError::ChecksumMismatch);
    }
    let archived = rkyv::access::<ArchivedPrefabFile, RkyvError>(&data)
        .map_err(|e| StorageError::SerializationError(e.to_string()))?;
    rkyv::deserialize::<PrefabFile, RkyvError>(archived)
        .map_err(|e| StorageError::SerializationError(e.to_string()))
}

// ============================================================================
// Library load/save
// ============================================================================

/// `prefabs/{name}.prefab` — the storage key (OPFS) and, under the project
/// root, the native path tail.
fn prefab_key(name: &str) -> String {
    format!("{PREFAB_DIR}/{name}.prefab")
}

/// A prefab name is also a file name; keep it to the same alphabet world ids
/// use.
pub fn validate_prefab_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("Prefab name is required.".into());
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err("Prefab names may only contain letters, digits, '-', and '_'.".into());
    }
    Ok(())
}

/// Write a definition to disk (fire-and-forget on wasm, like project
/// manifests) and into the library.
pub fn store_prefab(library: &mut PrefabLibrary, file: PrefabFile) -> Result<(), String> {
    let bytes = serialize_prefab(&file).map_err(|e| e.to_string())?;
    #[cfg(not(target_arch = "wasm32"))]
    {
        let path = std::path::Path::new(&prefab_key(&file.name)).to_owned();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        std::fs::write(&path, &bytes).map_err(|e| e.to_string())?;
    }
    #[cfg(target_arch = "wasm32")]
    {
        let key = prefab_key(&file.name);
        wasm_bindgen_futures::spawn_local(async move {
            let s = crate::storage::wasm::WasmStorage;
            if let Err(e) = s.save(&key, &bytes).await {
                log::error!("OPFS prefab save failed for '{key}': {e}");
            }
        });
    }
    info!(
        "Saved prefab '{}' ({} entities)",
        file.name,
        file.entities.len()
    );
    library.prefabs.insert(file.name.clone(), file);
    Ok(())
}

/// Native: scan `assets/prefabs/` at startup.
#[cfg(not(target_arch = "wasm32"))]
fn load_prefab_library(mut library: ResMut<PrefabLibrary>) {
    let Ok(entries) = std::fs::read_dir(PREFAB_DIR) else {
        return; // no prefabs yet
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("prefab") {
            continue;
        }
        match std::fs::read(&path)
            .map_err(StorageError::from)
            .and_then(|b| parse_prefab(&b))
        {
            Ok(file) => {
                library.prefabs.insert(file.name.clone(), file);
            }
            Err(e) => error!("Failed to load prefab {path:?}: {e}"),
        }
    }
    info!("Loaded {} prefab(s)", library.prefabs.len());
}

/// Web: list and read the OPFS `prefabs/` prefix into a slot, merged by
/// [`poll_prefab_library`].
#[cfg(target_arch = "wasm32")]
type PrefabSlot = std::sync::Arc<std::sync::Mutex<Option<Vec<PrefabFile>>>>;

#[cfg(target_arch = "wasm32")]
#[derive(Resource, Default)]
struct PendingPrefabLoad(Option<PrefabSlot>);

#[cfg(target_arch = "wasm32")]
fn load_prefab_library(mut pending: ResMut<PendingPrefabLoad>) {
    use std::sync::{Arc, Mutex};
    let slot: PrefabSlot = Arc::new(Mutex::new(None));
    pending.0 = Some(slot.clone());
    wasm_bindgen_futures::spawn_local(async move {
        let s = crate::storage::wasm::WasmStorage;
        let mut files = Vec::new();
        match s.list_keys(PREFAB_DIR).await {
            Ok(keys) => {
                for key in keys.iter().filter(|k| k.ends_with(".prefab")) {
                    match s.load(key).await {
                        Ok(Some(bytes)) => match parse_prefab(&bytes) {
                            Ok(file) => files.push(file),
                            Err(e) => log::error!("Failed to parse prefab '{key}': {e}"),
                        },
                        Ok(None) => {}
                        Err(e) => log::error!("OPFS prefab load failed for '{key}': {e}"),
                    }
                }
            }
            Err(e) => log::warn!("OPFS prefab listing failed: {e}"),
        }
        *slot.lock().unwrap() = Some(files);
    });
}

#[cfg(target_arch = "wasm32")]
fn poll_prefab_library(mut pending: ResMut<PendingPrefabLoad>, mut library: ResMut<PrefabLibrary>) {
    let Some(slot) = pending.0.as_ref() else {
        return;
    };
    let Some(files) = slot.lock().unwrap().take() else {
        return;
    };
    pending.0 = None;
    for file in files {
        library.prefabs.insert(file.name.clone(), file);
    }
    log::info!("Loaded {} prefab(s) from OPFS", library.prefabs.len());
}

pub struct PrefabsPlugin;

impl Plugin for PrefabsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<PrefabLibrary>();
        #[cfg(target_arch = "wasm32")]
        {
            app.init_resource::<PendingPrefabLoad>();
            app.add_systems(Update, poll_prefab_library);
        }
        app.add_systems(Startup, load_prefab_library);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine_api::ScriptProp;

    fn record(id: u64, at: Vec3) -> PlacedEntityRecord {
        PlacedEntityRecord::new(id, PlacedKind::Empty, &Transform::from_translation(at))
    }

    fn with_ref(mut r: PlacedEntityRecord, target: u64) -> PlacedEntityRecord {
        r.scripts = vec![
            ScriptBinding::new("p".to_string(), "s".to_string()).with_props(vec![ScriptProp {
                key: "link".into(),
                value: PropValue::EntityRef(target),
            }]),
        ];
        r
    }

    /// Define from live entities, then expand: a fixed point.
    #[test]
    fn define_then_expand_round_trips() {
        let mut root = record(10, Vec3::new(4.0, 0.0, 2.0));
        root.kind = PlacedKind::Group;
        root.name = "lamp post".into();
        let members = vec![with_ref(record(11, Vec3::new(5.0, 1.0, 2.0)), 12), {
            let mut m = record(12, Vec3::new(4.0, 3.0, 2.0));
            m.parent = Some(10);
            m
        }];

        let (def, inst) = define_from_records("lamp", &root, &members);
        assert_eq!(def.entities.len(), 3);
        assert_eq!(inst.member_ids, vec![(1, 11), (2, 12)]);
        // The intra-prefab ref became a local id.
        let def_ref = &def.entities[1].scripts[0].props[0].value;
        assert_eq!(*def_ref, PropValue::EntityRef(2));

        let mut instance_root = root.clone();
        instance_root.prefab = Some(inst);
        let expanded = expand_records(&def, &instance_root);
        assert_eq!(expanded.len(), 3);
        assert_eq!(expanded[0].id, 10);
        assert_eq!(expanded[0].name, "lamp post");
        for (orig, exp) in members.iter().zip(&expanded[1..]) {
            assert_eq!(exp.id, orig.id);
            assert!(transforms_match(orig.transform(), exp.transform()));
            assert_eq!(exp.parent, Some(10));
        }
        // ...and the ref is a world id again.
        assert_eq!(
            expanded[1].scripts[0].props[0].value,
            PropValue::EntityRef(12)
        );
    }

    #[test]
    fn expansion_applies_overrides_and_removals() {
        let root = record(10, Vec3::ZERO);
        let members = vec![record(11, Vec3::X), record(12, Vec3::Y)];
        let (def, mut inst) = define_from_records("x", &root, &members);
        inst.overrides = vec![
            PrefabOverride {
                local: 1,
                name: Some("tweaked".into()),
                ..default()
            },
            PrefabOverride {
                local: 2,
                removed: true,
                ..default()
            },
        ];
        let mut instance_root = root.clone();
        instance_root.prefab = Some(inst);
        let expanded = expand_records(&def, &instance_root);
        assert_eq!(expanded.len(), 2, "removed member absent");
        assert_eq!(expanded[1].name, "tweaked");
    }

    #[test]
    fn the_instance_root_takes_the_definitions_identity() {
        let mut root = record(10, Vec3::ZERO);
        root.kind = PlacedKind::mesh("meshes/a.gltf#Scene0");
        let (def, inst) = define_from_records("rock", &root, &[]);

        // The stored record diverges (e.g. the prefab was edited since).
        let mut stale = record(10, Vec3::X * 7.0);
        stale.kind = PlacedKind::Empty;
        stale.prefab = Some(inst);
        let expanded = expand_records(&def, &stale);
        assert_eq!(expanded.len(), 1);
        assert_eq!(
            expanded[0].kind, def.entities[0].kind,
            "linked to the definition"
        );
        assert_eq!(
            expanded[0].position,
            [7.0, 0.0, 0.0],
            "transform stays the instance's"
        );
    }

    #[test]
    fn override_diff_is_per_section_and_empty_when_in_sync() {
        let root = record(10, Vec3::new(1.0, 0.0, 0.0));
        let members = vec![record(11, Vec3::new(2.0, 0.0, 0.0))];
        let (def, inst) = define_from_records("x", &root, &members);
        let mut instance_root = root.clone();
        instance_root.prefab = Some(inst.clone());

        // In sync: no overrides.
        let live = expand_records(&def, &instance_root);
        let ovs = compute_instance_overrides(&def, &inst, &live[0], |id| {
            live[1..].iter().find(|m| m.id == id).cloned()
        });
        assert!(ovs.is_empty(), "{ovs:?}");

        // Rename only: one override with only the name section.
        let mut renamed = live.clone();
        renamed[1].name = "custom".into();
        let ovs = compute_instance_overrides(&def, &inst, &renamed[0], |id| {
            renamed[1..].iter().find(|m| m.id == id).cloned()
        });
        assert_eq!(ovs.len(), 1);
        assert_eq!(ovs[0].name.as_deref(), Some("custom"));
        assert!(ovs[0].transform.is_none() && ovs[0].kind.is_none() && ovs[0].scripts.is_none());

        // Missing member: removed.
        let ovs = compute_instance_overrides(&def, &inst, &live[0], |_| None);
        assert_eq!(ovs.len(), 1);
        assert!(ovs[0].removed);
    }

    #[test]
    fn overrides_survive_an_expand_diff_cycle() {
        let root = record(10, Vec3::ZERO);
        let members = vec![record(11, Vec3::X)];
        let (def, mut inst) = define_from_records("x", &root, &members);
        inst.overrides = vec![PrefabOverride {
            local: 1,
            transform: Some(([0.0, 9.0, 0.0], [0.0, 0.0, 0.0, 1.0], [1.0, 1.0, 1.0])),
            ..default()
        }];
        let mut instance_root = root.clone();
        instance_root.prefab = Some(inst.clone());
        let live = expand_records(&def, &instance_root);
        let ovs = compute_instance_overrides(&def, &inst, &live[0], |id| {
            live[1..].iter().find(|m| m.id == id).cloned()
        });
        assert_eq!(ovs, inst.overrides);
    }

    #[test]
    fn prefab_files_round_trip_and_reject_corruption() {
        let (def, _) = define_from_records("x", &record(1, Vec3::ZERO), &[record(2, Vec3::X)]);
        let mut bytes = serialize_prefab(&def).unwrap();
        assert_eq!(parse_prefab(&bytes).unwrap(), def);
        let last = bytes.len() - 6;
        bytes[last] ^= 0xFF;
        assert!(parse_prefab(&bytes).is_err());
    }

    #[test]
    fn prefab_names_are_validated() {
        assert!(validate_prefab_name("lamp-post_2").is_ok());
        assert!(validate_prefab_name("").is_err());
        assert!(validate_prefab_name("has space").is_err());
        assert!(validate_prefab_name("../evil").is_err());
    }
}
