//! Placed level entities and their per-chunk persistence.
//!
//! Entities are stored per-chunk alongside terrain data:
//! `worlds/{id}/chunks/chunk_X_Y.objects`
//!
//! A placed entity is a mesh instance, a light, or a bare transform anchor
//! (see [`PlacedKind`]), optionally carrying a list of [`ScriptBinding`]s
//! that associate it with scripts exported by dynamic plugins. Script bindings
//! are parsed, held on the entity, and re-serialized verbatim; the host does
//! not act on them yet (that is the hydration work in a later phase), so a
//! load → save round trip never loses authored bindings.
//!
//! # Format history
//! - v2: `objects: Vec<PlacedObjectRecord>`, mesh instances only.
//! - v3: `entities: Vec<PlacedEntityRecord>` with a `kind` and `scripts`.
//!
//! v2 files are still read (see [`legacy`]) and upgraded in memory to v3; they
//! are rewritten as v3 the next time the chunk is saved.

use avian3d::prelude::*;
use bevy::prelude::*;
use rkyv::{Archive, Deserialize, Serialize, rancor::Error as RkyvError};
use std::collections::{HashMap, HashSet};
#[cfg(not(all(target_arch = "wasm32", feature = "opfs")))]
use std::fs;

use engine_api::{AutoCollider, ScriptBinding, ScriptBindings};

use crate::game::{
    GameEntity,
    gltf_loading::{ConvertToToon, Interactable, spawn_interactable_scene},
    world::terrain::storage::StorageError,
};

use crate::game::world::terrain::storage::TerrainWorld;
use crate::game::world::terrain::{CHUNK_SIZE, TILE_SIZE};

// File format constants
const CHUNK_OBJECTS_MAGIC: &[u8; 4] = b"COBJ";
/// Version 3: records generalised from mesh-only `PlacedObjectRecord` to
/// `PlacedEntityRecord` (kind + scripts). Version 4: light `intensity`
/// reinterpreted from bevy photometric units into lightmap units for the
/// baked lighting pipeline. Versions 2 and 3 are still readable.
pub const CHUNK_OBJECTS_VERSION: u32 = 4;

// Chunk world size in units: (CHUNK_SIZE - 1) * TILE_SIZE.
pub const CHUNK_WORLD_SIZE: f32 = (CHUNK_SIZE - 1) as f32 * TILE_SIZE.0;

// ============================================================================
// Serializable Data Structures (v3)
// ============================================================================

/// One placed thing in a level: a mesh, a light, or a bare anchor.
#[derive(Archive, Deserialize, Serialize, Clone, Debug)]
pub struct PlacedEntityRecord {
    /// Stable id within this world, allocated by [`PlacedObjectRegistry`].
    /// Also keys the object's baked lightmap on disk.
    pub id: u64,
    /// Editor-facing label. May be empty.
    pub name: String,
    pub position: [f32; 3],
    /// Quaternion, xyzw.
    pub rotation: [f32; 4],
    pub scale: [f32; 3],
    pub kind: PlacedKind,
    pub scripts: Vec<ScriptBinding>,
}

impl PlacedEntityRecord {
    /// Build an unnamed record of `kind` at `transform` with no scripts — the
    /// shape the editor's placement click produces.
    pub fn new(id: u64, kind: PlacedKind, transform: &Transform) -> Self {
        Self {
            id,
            name: String::new(),
            position: transform.translation.into(),
            rotation: quat_to_array(transform.rotation),
            scale: transform.scale.into(),
            kind,
            scripts: Vec::new(),
        }
    }

    pub fn with_scripts(mut self, scripts: Vec<ScriptBinding>) -> Self {
        self.scripts = scripts;
        self
    }

    pub fn transform(&self) -> Transform {
        Transform {
            translation: Vec3::from(self.position),
            rotation: quat_from_array(self.rotation),
            scale: Vec3::from(self.scale),
        }
    }
}

/// What a placed entity actually is. New variants must be appended, never
/// reordered — rkyv encodes enum variants positionally.
#[derive(Archive, Deserialize, Serialize, Clone, Debug, PartialEq)]
pub enum PlacedKind {
    Mesh {
        asset_path: String,
        collider: ColliderMode,
    },
    /// A baked point light. Never spawned as a bevy `PointLight` — see
    /// `spawn_placed_entity`.
    PointLight {
        /// Linear RGB.
        color: [f32; 3],
        /// Radiance at the light, in lightmap units (1.0 ≈ full base colour
        /// on a surface facing it from close up). Not lumens.
        intensity: f32,
        /// World units at which the light falls off to nothing. The falloff
        /// is smooth and bounded, not physical inverse-square.
        range: f32,
        /// Unused: baked lights always cast baked shadows. Retained so the
        /// record shape is stable for a future dynamic-light option.
        shadows: bool,
    },
    /// A baked spot light. Never spawned as a bevy `SpotLight`.
    SpotLight {
        /// Linear RGB.
        color: [f32; 3],
        /// Radiance at the light, in lightmap units. Not lumens.
        intensity: f32,
        /// World units at which the light falls off to nothing.
        range: f32,
        /// Half-angles in radians; the cone fades between the two.
        inner_angle: f32,
        outer_angle: f32,
        /// Unused: baked lights always cast baked shadows.
        shadows: bool,
    },
    /// Transform only — a spawn point or script anchor.
    Empty,
}

impl PlacedKind {
    pub fn mesh(asset_path: impl Into<String>) -> Self {
        PlacedKind::Mesh {
            asset_path: asset_path.into(),
            collider: ColliderMode::Trimesh,
        }
    }

    /// A warm interior light with the same reach cafe-sim's hand-placed
    /// lights used — a sensible starting point for an editor-placed light.
    pub fn point_light() -> Self {
        PlacedKind::PointLight {
            color: [1.0, 0.85, 0.6],
            intensity: 1.0,
            range: 14.0,
            shadows: false,
        }
    }

    pub fn spot_light() -> Self {
        PlacedKind::SpotLight {
            color: [1.0, 0.95, 0.85],
            intensity: 1.5,
            range: 20.0,
            inner_angle: 0.4,
            outer_angle: 0.7,
            shadows: false,
        }
    }

    pub fn is_mesh(&self) -> bool {
        matches!(self, PlacedKind::Mesh { .. })
    }

    pub fn is_light(&self) -> bool {
        matches!(
            self,
            PlacedKind::PointLight { .. } | PlacedKind::SpotLight { .. }
        )
    }

    /// Short editor label for this kind.
    pub fn label(&self) -> &'static str {
        match self {
            PlacedKind::Mesh { .. } => "Mesh",
            PlacedKind::PointLight { .. } => "Point Light",
            PlacedKind::SpotLight { .. } => "Spot Light",
            PlacedKind::Empty => "Empty",
        }
    }
}

/// How a placed mesh participates in physics.
#[derive(Archive, Deserialize, Serialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ColliderMode {
    /// Static trimesh built from the mesh hierarchy.
    #[default]
    Trimesh,
    /// No collider at all (decoration).
    None,
}

// Script bindings are defined in `engine-api` (see the `script` module there)
// so plugins and the host share one definition with a stable `TypeId` across
// the dylib boundary. The on-disk format reuses `ScriptBinding` verbatim
// rather than mirroring it, so the serialized and runtime shapes cannot drift.

#[derive(Archive, Deserialize, Serialize, Clone, Debug)]
pub struct ChunkObjectsFile {
    pub version: u32,
    pub chunk_x: i32,
    pub chunk_y: i32,
    pub entities: Vec<PlacedEntityRecord>,
}

// ============================================================================
// Legacy (v2) format
// ============================================================================

/// The v2 on-disk shape, retained so existing worlds keep loading. Parsed by
/// [`parse_chunk_objects_from_bytes`] and upgraded to v3 in memory; never
/// written.
pub mod legacy {
    use super::{ChunkObjectsFile, ColliderMode, PlacedEntityRecord, PlacedKind};
    use rkyv::{Archive, Deserialize, Serialize};

    #[derive(Archive, Deserialize, Serialize, Clone, Debug)]
    pub struct PlacedObjectRecordV2 {
        pub id: u64,
        pub asset_path: String,
        pub position: [f32; 3],
        pub rotation: [f32; 4],
        pub scale: [f32; 3],
    }

    #[derive(Archive, Deserialize, Serialize, Clone, Debug)]
    pub struct ChunkObjectsFileV2 {
        pub version: u32,
        pub chunk_x: i32,
        pub chunk_y: i32,
        pub objects: Vec<PlacedObjectRecordV2>,
    }

    /// v3 → v4: light `intensity` was in bevy photometric units (lumens for
    /// point lights, ~100 000 for a room light); v4 reads it as lightmap
    /// units, where 1.0 is roughly "full base colour up close". There is no
    /// meaningful conversion between the two — the old numbers would read as
    /// blinding white — so lights adopt the current preset intensity and can
    /// be re-tuned by hand.
    pub fn rescale_v3_light_intensities(file: &mut ChunkObjectsFile) {
        for entity in &mut file.entities {
            match &mut entity.kind {
                PlacedKind::PointLight { intensity, .. } => {
                    *intensity = match PlacedKind::point_light() {
                        PlacedKind::PointLight { intensity, .. } => intensity,
                        _ => unreachable!(),
                    };
                }
                PlacedKind::SpotLight { intensity, .. } => {
                    *intensity = match PlacedKind::spot_light() {
                        PlacedKind::SpotLight { intensity, .. } => intensity,
                        _ => unreachable!(),
                    };
                }
                _ => {}
            }
        }
    }

    /// Every v2 record was a trimesh-collided mesh with no scripts.
    pub fn upgrade(old: ChunkObjectsFileV2) -> ChunkObjectsFile {
        ChunkObjectsFile {
            version: super::CHUNK_OBJECTS_VERSION,
            chunk_x: old.chunk_x,
            chunk_y: old.chunk_y,
            entities: old
                .objects
                .into_iter()
                .map(|o| PlacedEntityRecord {
                    id: o.id,
                    name: String::new(),
                    position: o.position,
                    rotation: o.rotation,
                    scale: o.scale,
                    kind: PlacedKind::Mesh {
                        asset_path: o.asset_path,
                        collider: ColliderMode::Trimesh,
                    },
                    scripts: Vec::new(),
                })
                .collect(),
        }
    }
}

// ============================================================================
// Runtime Types
// ============================================================================

/// Attached to each placed entity. Holds everything needed to re-serialize the
/// entity without consulting the file it came from, so edits to one field
/// never drop the others.
#[derive(Component, Clone, Debug)]
pub struct PlacedObject {
    pub id: u64,
    pub chunk: (i32, i32),
    pub name: String,
    pub kind: PlacedKind,
}

impl PlacedObject {
    /// Rebuild the on-disk record from live component state.
    ///
    /// Script bindings live in the separate [`ScriptBindings`] component (the
    /// shared `engine-api` type the hydration pass reads), so they are passed
    /// in rather than duplicated here — one source of truth means an edit to
    /// the bindings can't be silently dropped on save.
    pub fn to_record(
        &self,
        transform: &Transform,
        bindings: Option<&ScriptBindings>,
    ) -> PlacedEntityRecord {
        PlacedEntityRecord {
            id: self.id,
            name: self.name.clone(),
            position: transform.translation.into(),
            rotation: quat_to_array(transform.rotation),
            scale: transform.scale.into(),
            kind: self.kind.clone(),
            scripts: bindings.map(|b| b.0.clone()).unwrap_or_default(),
        }
    }
}

/// Global registry of all placed entities.
#[derive(Resource, Default)]
pub struct PlacedObjectRegistry {
    pub by_id: HashMap<u64, Entity>,
    pub next_id: u64,
    pub dirty_chunks: HashSet<(i32, i32)>,
    /// `next_id` is only trustworthy once [`seed_placed_ids`] has applied the
    /// world's saved `next_entity_id` counter. Until then `alloc_id` could
    /// hand out an id already used by an entity in a not-yet-streamed chunk —
    /// colliding baked lightmap files and breaking `PropValue::EntityRef` —
    /// so the editor refuses to place anything while this is false.
    pub ids_seeded: bool,
}

impl PlacedObjectRegistry {
    pub fn alloc_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    /// Advance `next_id` past `id` so future allocations can't collide with
    /// ids loaded from disk.
    pub fn observe_id(&mut self, id: u64) {
        if id >= self.next_id {
            self.next_id = id + 1;
        }
    }
}

/// What the editor places on the next click.
#[derive(Resource)]
pub struct PlacementConfig {
    /// Kind (and kind-specific defaults) of the next placed entity.
    pub kind: PlacedKind,
    pub scale: Vec3,
    /// Stamp mode: a `(plugin, script)` binding attached to every newly placed
    /// entity, seeded with the script's schema defaults. Sticky across mesh
    /// changes so a run of different meshes can share one script; cleared
    /// from the scripts panel.
    pub stamp_script: Option<(String, String)>,
}

impl Default for PlacementConfig {
    fn default() -> Self {
        Self {
            kind: PlacedKind::mesh("meshes/rock/bigrock.gltf#Scene0"),
            scale: Vec3::splat(0.75),
            stamp_script: None,
        }
    }
}

impl PlacementConfig {
    /// Scale is meaningful only for meshes; lights and anchors ignore it, and
    /// forcing it to one keeps a stale mesh scale from leaking into a light's
    /// gizmo or a later record.
    pub fn effective_scale(&self) -> Vec3 {
        if self.kind.is_mesh() {
            self.scale
        } else {
            Vec3::ONE
        }
    }
}

// ============================================================================
// Chunk Math
// ============================================================================

pub fn world_pos_to_chunk(world_x: f32, world_z: f32) -> (i32, i32) {
    let cx = (world_x / CHUNK_WORLD_SIZE).floor() as i32;
    let cy = (world_z / CHUNK_WORLD_SIZE).floor() as i32;
    (cx, cy)
}

#[inline]
fn quat_to_array(q: Quat) -> [f32; 4] {
    [q.x, q.y, q.z, q.w]
}

#[inline]
fn quat_from_array(q: [f32; 4]) -> Quat {
    Quat::from_xyzw(q[0], q[1], q[2], q[3])
}

// ============================================================================
// Spawn / Despawn Helpers
// ============================================================================

/// Spawn one placed entity and register it. Shared by the editor's placement
/// path and the streaming loader, so both produce identical entities.
///
/// Does **not** mark the chunk dirty — the caller decides, since streaming
/// must not dirty a chunk merely by loading it.
pub fn spawn_entity_record(
    commands: &mut Commands,
    asset_server: &AssetServer,
    registry: &mut PlacedObjectRegistry,
    chunk: (i32, i32),
    record: &PlacedEntityRecord,
) -> Entity {
    let transform = record.transform();
    let placed = PlacedObject {
        id: record.id,
        chunk,
        name: record.name.clone(),
        kind: record.kind.clone(),
    };

    let entity = match &record.kind {
        PlacedKind::Mesh {
            asset_path,
            collider,
        } => {
            let handle: Handle<Scene> = asset_server.load(asset_path);
            let entity = spawn_interactable_scene(
                commands,
                (
                    GameEntity,
                    placed,
                    Interactable {
                        name: if record.name.is_empty() {
                            "Object".to_string()
                        } else {
                            record.name.clone()
                        },
                        distance: 5.0,
                    },
                    ConvertToToon,
                    crate::game::world::baking::object_lightmaps::NeedsObjectLightmaps,
                    SceneRoot(handle),
                    transform,
                    GlobalTransform::default(),
                    Visibility::Inherited,
                ),
            );
            if *collider == ColliderMode::Trimesh {
                commands.entity(entity).insert((
                    RigidBody::Static,
                    ColliderConstructorHierarchy::new(ColliderConstructor::TrimeshFromMesh),
                ));
            }
            entity
        }
        // Lights carry no bevy light component: all lighting is baked, so a
        // real `PointLight`/`SpotLight` would light nothing the toon shaders
        // draw while still costing cluster assignment and shadow-map work
        // every frame. The parameters live on `PlacedObject.kind`, which is
        // what the baker reads; `draw_light_gizmos` visualises them in the
        // editor.
        PlacedKind::PointLight { .. } | PlacedKind::SpotLight { .. } => {
            commands.spawn((GameEntity, placed, transform)).id()
        }
        PlacedKind::Empty => commands
            .spawn((
                GameEntity,
                placed,
                transform,
                GlobalTransform::default(),
                Visibility::Inherited,
            ))
            .id(),
    };

    // Bindings ride on their own component so the hydration pass (and, later,
    // the editor) can read and mutate them without touching `PlacedObject`.
    // `NeedsHydration` is inserted even when the naming plugin isn't loaded —
    // the marker is what lets a plugin added mid-session pick the entity up.
    if !record.scripts.is_empty() {
        commands.entity(entity).insert((
            ScriptBindings(record.scripts.clone()),
            crate::script_host::NeedsHydration,
        ));
    }

    registry.by_id.insert(record.id, entity);
    registry.observe_id(record.id);
    entity
}

#[inline]
fn linear_color(rgb: [f32; 3]) -> Color {
    Color::linear_rgb(rgb[0], rgb[1], rgb[2])
}

/// Spawn all entities described in a `ChunkObjectsFile` and register them.
pub fn spawn_chunk_objects(
    commands: &mut Commands,
    asset_server: &AssetServer,
    registry: &mut PlacedObjectRegistry,
    chunk_objects: &ChunkObjectsFile,
) {
    let chunk = (chunk_objects.chunk_x, chunk_objects.chunk_y);
    for record in &chunk_objects.entities {
        spawn_entity_record(commands, asset_server, registry, chunk, record);
    }
}

/// Despawn all placed entities belonging to `chunk` and remove them from the registry.
pub fn despawn_chunk_objects(
    commands: &mut Commands,
    registry: &mut PlacedObjectRegistry,
    placed_query: &Query<(Entity, &PlacedObject)>,
    chunk: (i32, i32),
) {
    let to_despawn: Vec<(u64, Entity)> = placed_query
        .iter()
        .filter(|(_, obj)| obj.chunk == chunk)
        .map(|(entity, obj)| (obj.id, entity))
        .collect();

    for (id, entity) in to_despawn {
        registry.by_id.remove(&id);
        commands.entity(entity).despawn();
    }
}

// ============================================================================
// Id Seeding
// ============================================================================

/// Seed `PlacedObjectRegistry::next_id` from the world's saved counter
/// (`WorldMetadata::next_entity_id`) when a world loads.
///
/// `observe_id` only sees records whose chunks have streamed in, so without
/// the counter the first `alloc_id` in a freshly loaded world could reuse an
/// id that belongs to an entity in a distant, unloaded chunk. Keyed on
/// `is_added` rather than `ids_seeded` so a lingering `TerrainWorld` from a
/// world being switched away can't seed the next world's fresh registry.
pub fn seed_placed_ids(
    terrain_world: Option<Res<TerrainWorld>>,
    mut registry: ResMut<PlacedObjectRegistry>,
) {
    let Some(terrain_world) = terrain_world else {
        return;
    };
    if !terrain_world.is_added() {
        return;
    }
    registry.next_id = registry.next_id.max(terrain_world.metadata.next_entity_id);
    registry.ids_seeded = true;
}

// ============================================================================
// Systems
// ============================================================================

/// Fulfills `AutoCollider` markers from dynamic plugins by attaching a static
/// trimesh collider hierarchy. The marker is removed after insertion so this
/// runs at most once per tagged entity.
pub fn apply_auto_colliders(mut commands: Commands, q: Query<Entity, Added<AutoCollider>>) {
    for entity in q.iter() {
        commands
            .entity(entity)
            .insert((
                RigidBody::Static,
                ColliderConstructorHierarchy::new(ColliderConstructor::TrimeshFromMesh),
            ))
            .remove::<AutoCollider>();
    }
}

/// Draw editor-only wireframe markers for placed lights, which otherwise have
/// no visual representation.
///
/// Deliberately immediate-mode [`Gizmos`] rather than child mesh entities: a
/// gizmo never appears in the mesh queries the lightmap baker walks, so light
/// markers can't accidentally cast baked shadows or get a lightmap atlas.
pub fn draw_light_gizmos(mut gizmos: Gizmos, placed: Query<(&PlacedObject, &GlobalTransform)>) {
    for (obj, gt) in placed.iter() {
        let pos = gt.translation();
        match &obj.kind {
            PlacedKind::PointLight { color, range, .. } => {
                let c = linear_color(*color);
                gizmos.sphere(pos, 0.15, c);
                gizmos.sphere(pos, *range, c.with_alpha(0.15));
            }
            PlacedKind::SpotLight {
                color,
                range,
                outer_angle,
                ..
            } => {
                let c = linear_color(*color);
                let dir = gt.forward().as_vec3();
                let tip = pos + dir * *range;
                gizmos.sphere(pos, 0.15, c);
                gizmos.line(pos, tip, c);
                gizmos.circle(
                    Isometry3d::new(tip, Quat::from_rotation_arc(Vec3::Z, dir)),
                    *range * outer_angle.tan(),
                    c.with_alpha(0.3),
                );
            }
            _ => {}
        }
    }
}

/// Saves all dirty object chunks alongside terrain saving — runs whenever the
/// editor sends a `SaveTerrainMessage`.
pub fn save_placed_objects(
    mut messages: MessageReader<crate::game::world::terrain::save::SaveTerrainMessage>,
    terrain_world: Option<Res<TerrainWorld>>,
    mut registry: ResMut<PlacedObjectRegistry>,
    placed_query: Query<(&PlacedObject, &Transform, Option<&ScriptBindings>)>,
    #[cfg(all(target_arch = "wasm32", feature = "opfs"))] _storage: Res<crate::storage::Storage>,
) {
    if messages.read().count() == 0 {
        return;
    }

    let Some(terrain_world) = terrain_world else {
        warn!("Cannot save objects: world not loaded");
        return;
    };

    if registry.dirty_chunks.is_empty() {
        return;
    }

    let dirty: Vec<(i32, i32)> = registry.dirty_chunks.drain().collect();
    info!("Saving {} dirty object chunk(s)", dirty.len());

    // Build the list of (file, serialized_bytes) pairs once, shared by both paths.
    let world_id = terrain_world.metadata.world_id.clone();
    let mut writes: Vec<(String, Vec<u8>, i32, i32)> = Vec::new();

    for (cx, cy) in &dirty {
        let (cx, cy) = (*cx, *cy);
        let entities: Vec<PlacedEntityRecord> = placed_query
            .iter()
            .filter(|(obj, _, _)| obj.chunk == (cx, cy))
            .map(|(obj, transform, bindings)| obj.to_record(transform, bindings))
            .collect();

        let file = ChunkObjectsFile {
            version: CHUNK_OBJECTS_VERSION,
            chunk_x: cx,
            chunk_y: cy,
            entities,
        };

        match serialize_chunk_objects(&file) {
            Ok(bytes) => {
                let key = format!("worlds/{}/chunks/chunk_{}_{}.objects", world_id, cx, cy);
                writes.push((key, bytes, cx, cy));
            }
            Err(e) => error!("Failed to serialize objects for chunk ({cx}, {cy}): {e}"),
        }
    }

    #[cfg(not(all(target_arch = "wasm32", feature = "opfs")))]
    {
        let world_path = terrain_world.world_path.clone();
        for (_, bytes, cx, cy) in writes {
            let chunks_dir = world_path.join("chunks");
            if let Err(e) = fs::create_dir_all(&chunks_dir) {
                error!("Failed to create chunks dir: {e}");
                continue;
            }
            let path = world_path
                .join("chunks")
                .join(format!("chunk_{}_{}.objects", cx, cy));
            match fs::write(&path, &bytes) {
                Ok(()) => info!("Saved objects for chunk ({cx}, {cy})"),
                Err(e) => error!("Failed to save objects for chunk ({cx}, {cy}): {e}"),
            }
        }
    }

    #[cfg(all(target_arch = "wasm32", feature = "opfs"))]
    {
        use wasm_bindgen_futures::spawn_local;
        let s = crate::storage::wasm::WasmStorage;
        spawn_local(async move {
            for (key, bytes, cx, cy) in writes {
                if let Err(e) = s.save(&key, &bytes).await {
                    log::error!("OPFS objects save failed for chunk ({cx}, {cy}): {e}");
                } else {
                    log::info!("Saved objects for chunk ({cx}, {cy}) to OPFS");
                }
            }
        });
    }
}

// ============================================================================
// I/O Helpers
// ============================================================================

/// Serialize a `ChunkObjectsFile` into the canonical binary frame
/// (magic + version + coords + len + rkyv + crc32).
/// Use this to produce bytes for writing to any backing store (fs, OPFS, etc.).
pub fn serialize_chunk_objects(file: &ChunkObjectsFile) -> Result<Vec<u8>, StorageError> {
    let rkyv_bytes = rkyv::to_bytes::<RkyvError>(file)
        .map_err(|e| StorageError::SerializationError(e.to_string()))?;
    let checksum = crc32fast::hash(&rkyv_bytes);

    let mut out = Vec::with_capacity(4 + 4 + 4 + 4 + 4 + rkyv_bytes.len() + 4);
    out.extend_from_slice(CHUNK_OBJECTS_MAGIC);
    out.extend_from_slice(&CHUNK_OBJECTS_VERSION.to_le_bytes());
    out.extend_from_slice(&file.chunk_x.to_le_bytes());
    out.extend_from_slice(&file.chunk_y.to_le_bytes());
    out.extend_from_slice(&(rkyv_bytes.len() as u32).to_le_bytes());
    out.extend_from_slice(&rkyv_bytes);
    out.extend_from_slice(&checksum.to_le_bytes());
    Ok(out)
}

/// Parse a `ChunkObjectsFile` from raw bytes. Accepts v2 (upgraded in memory)
/// and v3. Used by the async `ChunkObjectsLoader` and the OPFS load path.
pub fn parse_chunk_objects_from_bytes(bytes: &[u8]) -> Result<ChunkObjectsFile, StorageError> {
    use std::io::Read as StdRead;
    let mut cursor = std::io::Cursor::new(bytes);

    let mut magic = [0u8; 4];
    cursor.read_exact(&mut magic)?;
    if &magic != CHUNK_OBJECTS_MAGIC {
        return Err(StorageError::InvalidMagic);
    }

    let mut version_bytes = [0u8; 4];
    cursor.read_exact(&mut version_bytes)?;
    let version = u32::from_le_bytes(version_bytes);
    if version != 2 && version != 3 && version != CHUNK_OBJECTS_VERSION {
        return Err(StorageError::VersionMismatch {
            expected: CHUNK_OBJECTS_VERSION,
            found: version,
        });
    }

    let mut coord = [0u8; 4];
    cursor.read_exact(&mut coord)?; // chunk_x (i32)
    cursor.read_exact(&mut coord)?; // chunk_y (i32)

    let mut len_bytes = [0u8; 4];
    cursor.read_exact(&mut len_bytes)?;
    let data_len = u32::from_le_bytes(len_bytes) as usize;

    let mut data = vec![0u8; data_len];
    cursor.read_exact(&mut data)?;

    let mut checksum_bytes = [0u8; 4];
    cursor.read_exact(&mut checksum_bytes)?;
    let stored = u32::from_le_bytes(checksum_bytes);
    if stored != crc32fast::hash(&data) {
        return Err(StorageError::ChecksumMismatch);
    }

    if version == 2 {
        let archived = rkyv::access::<legacy::ArchivedChunkObjectsFileV2, RkyvError>(&data)
            .map_err(|e| StorageError::SerializationError(e.to_string()))?;
        let old: legacy::ChunkObjectsFileV2 =
            rkyv::deserialize::<legacy::ChunkObjectsFileV2, RkyvError>(archived)
                .map_err(|e| StorageError::SerializationError(e.to_string()))?;
        return Ok(legacy::upgrade(old));
    }

    let archived = rkyv::access::<ArchivedChunkObjectsFile, RkyvError>(&data)
        .map_err(|e| StorageError::SerializationError(e.to_string()))?;
    let mut file: ChunkObjectsFile = rkyv::deserialize::<ChunkObjectsFile, RkyvError>(archived)
        .map_err(|e| StorageError::SerializationError(e.to_string()))?;

    // v3 shares v4's layout exactly — only the meaning of light `intensity`
    // changed — so it parses with the current types and is fixed up here.
    if version == 3 {
        legacy::rescale_v3_light_intensities(&mut file);
    }
    file.version = CHUNK_OBJECTS_VERSION;

    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine_api::{PropValue, ScriptProp};

    fn sample_current() -> ChunkObjectsFile {
        ChunkObjectsFile {
            version: CHUNK_OBJECTS_VERSION,
            chunk_x: -3,
            chunk_y: 7,
            entities: vec![
                PlacedEntityRecord {
                    id: 1,
                    name: "rock".into(),
                    position: [1.0, 2.0, 3.0],
                    rotation: [0.0, 0.0, 0.0, 1.0],
                    scale: [0.5, 0.5, 0.5],
                    kind: PlacedKind::Mesh {
                        asset_path: "meshes/rock/bigrock.gltf#Scene0".into(),
                        collider: ColliderMode::None,
                    },
                    scripts: vec![ScriptBinding {
                        plugin: "cafe-sim".into(),
                        script: "coffee_machine".into(),
                        props: vec![
                            ScriptProp {
                                key: "capacity".into(),
                                value: PropValue::Int(4),
                            },
                            ScriptProp {
                                key: "linked".into(),
                                value: PropValue::EntityRef(2),
                            },
                        ],
                    }],
                },
                PlacedEntityRecord {
                    id: 2,
                    name: String::new(),
                    position: [0.0, 4.0, 0.0],
                    rotation: [0.0, 0.0, 0.0, 1.0],
                    scale: [1.0, 1.0, 1.0],
                    kind: PlacedKind::PointLight {
                        color: [1.0, 0.85, 0.6],
                        intensity: 100_000.0,
                        range: 14.0,
                        shadows: false,
                    },
                    scripts: Vec::new(),
                },
            ],
        }
    }

    #[test]
    fn current_roundtrip_preserves_kinds_and_scripts() {
        let file = sample_current();
        let bytes = serialize_chunk_objects(&file).unwrap();
        let loaded = parse_chunk_objects_from_bytes(&bytes).unwrap();

        assert_eq!(loaded.version, CHUNK_OBJECTS_VERSION);
        assert_eq!(loaded.chunk_x, -3);
        assert_eq!(loaded.chunk_y, 7);
        assert_eq!(loaded.entities.len(), 2);

        let rock = &loaded.entities[0];
        assert_eq!(rock.name, "rock");
        match &rock.kind {
            PlacedKind::Mesh {
                asset_path,
                collider,
            } => {
                assert_eq!(asset_path, "meshes/rock/bigrock.gltf#Scene0");
                assert_eq!(*collider, ColliderMode::None);
            }
            other => panic!("expected Mesh, got {other:?}"),
        }
        assert_eq!(rock.scripts.len(), 1);
        assert_eq!(rock.scripts[0].plugin, "cafe-sim");
        assert_eq!(rock.scripts[0].script, "coffee_machine");
        assert_eq!(rock.scripts[0].props.len(), 2);
        assert!(matches!(rock.scripts[0].props[0].value, PropValue::Int(4)));
        assert!(matches!(
            rock.scripts[0].props[1].value,
            PropValue::EntityRef(2)
        ));

        match &loaded.entities[1].kind {
            PlacedKind::PointLight {
                intensity, range, ..
            } => {
                assert_eq!(*intensity, 100_000.0);
                assert_eq!(*range, 14.0);
            }
            other => panic!("expected PointLight, got {other:?}"),
        }
    }

    /// Writes a frame in the exact v2 layout and checks it upgrades cleanly —
    /// this is the guard against breaking the `cafe` / `default` worlds.
    #[test]
    fn v2_file_upgrades_to_v3_mesh_records() {
        let old = legacy::ChunkObjectsFileV2 {
            version: 2,
            chunk_x: 1,
            chunk_y: -2,
            objects: vec![legacy::PlacedObjectRecordV2 {
                id: 42,
                asset_path: "meshes/rock/bigrock.gltf#Scene0".into(),
                position: [5.0, 6.0, 7.0],
                rotation: [0.0, 0.0, 0.0, 1.0],
                scale: [2.0, 2.0, 2.0],
            }],
        };

        let rkyv_bytes = rkyv::to_bytes::<RkyvError>(&old).unwrap();
        let checksum = crc32fast::hash(&rkyv_bytes);
        let mut bytes = Vec::new();
        bytes.extend_from_slice(CHUNK_OBJECTS_MAGIC);
        bytes.extend_from_slice(&2u32.to_le_bytes());
        bytes.extend_from_slice(&old.chunk_x.to_le_bytes());
        bytes.extend_from_slice(&old.chunk_y.to_le_bytes());
        bytes.extend_from_slice(&(rkyv_bytes.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&rkyv_bytes);
        bytes.extend_from_slice(&checksum.to_le_bytes());

        let loaded = parse_chunk_objects_from_bytes(&bytes).unwrap();
        assert_eq!(loaded.version, CHUNK_OBJECTS_VERSION);
        assert_eq!(loaded.chunk_x, 1);
        assert_eq!(loaded.chunk_y, -2);
        assert_eq!(loaded.entities.len(), 1);

        let e = &loaded.entities[0];
        assert_eq!(e.id, 42);
        assert_eq!(e.position, [5.0, 6.0, 7.0]);
        assert_eq!(e.scale, [2.0, 2.0, 2.0]);
        assert!(e.scripts.is_empty());
        match &e.kind {
            PlacedKind::Mesh {
                asset_path,
                collider,
            } => {
                assert_eq!(asset_path, "meshes/rock/bigrock.gltf#Scene0");
                assert_eq!(*collider, ColliderMode::Trimesh);
            }
            other => panic!("expected Mesh, got {other:?}"),
        }
    }

    /// A v3 file has v4's exact byte layout — only the meaning of light
    /// `intensity` changed — so it parses with the current types and gets its
    /// photometric intensities swapped for lightmap-unit ones. Everything
    /// else, including the lights' colour, range and cone, is preserved.
    #[test]
    fn v3_light_intensities_are_rescaled_on_load() {
        let mut file = sample_current();
        file.entities.push(PlacedEntityRecord {
            id: 50,
            name: "lamp".into(),
            position: [0.0, 3.0, 0.0],
            rotation: [0.0, 0.0, 0.0, 1.0],
            scale: [1.0, 1.0, 1.0],
            kind: PlacedKind::PointLight {
                color: [1.0, 0.5, 0.25],
                intensity: 100_000.0,
                range: 9.0,
                shadows: false,
            },
            scripts: Vec::new(),
        });
        file.entities.push(PlacedEntityRecord {
            id: 51,
            name: "spot".into(),
            position: [1.0, 4.0, 0.0],
            rotation: [0.0, 0.0, 0.0, 1.0],
            scale: [1.0, 1.0, 1.0],
            kind: PlacedKind::SpotLight {
                color: [0.2, 0.4, 0.6],
                intensity: 250_000.0,
                range: 11.0,
                inner_angle: 0.3,
                outer_angle: 0.6,
                shadows: true,
            },
            scripts: Vec::new(),
        });

        // Same payload, stamped as v3.
        let mut bytes = serialize_chunk_objects(&file).unwrap();
        bytes[4..8].copy_from_slice(&3u32.to_le_bytes());

        let loaded = parse_chunk_objects_from_bytes(&bytes).unwrap();
        assert_eq!(loaded.version, CHUNK_OBJECTS_VERSION);

        let point = &loaded.entities.iter().find(|e| e.id == 50).unwrap().kind;
        match point {
            PlacedKind::PointLight {
                color,
                intensity,
                range,
                ..
            } => {
                assert_eq!(*color, [1.0, 0.5, 0.25]);
                assert_eq!(*range, 9.0);
                assert!(*intensity < 10.0, "lumens must not survive: {intensity}");
            }
            other => panic!("expected a point light, got {other:?}"),
        }

        let spot = &loaded.entities.iter().find(|e| e.id == 51).unwrap().kind;
        match spot {
            PlacedKind::SpotLight {
                color,
                intensity,
                range,
                inner_angle,
                outer_angle,
                ..
            } => {
                assert_eq!(*color, [0.2, 0.4, 0.6]);
                assert_eq!(*range, 11.0);
                assert_eq!(*inner_angle, 0.3);
                assert_eq!(*outer_angle, 0.6);
                assert!(*intensity < 10.0, "lumens must not survive: {intensity}");
            }
            other => panic!("expected a spot light, got {other:?}"),
        }

        // Meshes are untouched by the rescale.
        let rock = loaded.entities.iter().find(|e| e.id == 1).unwrap();
        assert!(matches!(rock.kind, PlacedKind::Mesh { .. }));
        assert_eq!(rock.scripts.len(), 1);
    }

    #[test]
    fn unknown_version_is_rejected() {
        let file = sample_current();
        let mut bytes = serialize_chunk_objects(&file).unwrap();
        bytes[4..8].copy_from_slice(&99u32.to_le_bytes());
        assert!(matches!(
            parse_chunk_objects_from_bytes(&bytes),
            Err(StorageError::VersionMismatch { found: 99, .. })
        ));
    }

    #[test]
    fn corrupt_payload_fails_checksum() {
        let file = sample_current();
        let mut bytes = serialize_chunk_objects(&file).unwrap();
        let n = bytes.len();
        bytes[n - 8] ^= 0xFF;
        assert!(matches!(
            parse_chunk_objects_from_bytes(&bytes),
            Err(StorageError::ChecksumMismatch)
        ));
    }

    #[test]
    fn record_transform_roundtrips() {
        let t = Transform {
            translation: Vec3::new(1.0, 2.0, 3.0),
            rotation: Quat::from_rotation_y(0.75),
            scale: Vec3::splat(2.0),
        };
        let rec = PlacedEntityRecord::new(9, PlacedKind::mesh("a.gltf#Scene0"), &t);
        let back = rec.transform();
        assert!(back.translation.abs_diff_eq(t.translation, 1e-6));
        assert!(back.rotation.abs_diff_eq(t.rotation, 1e-6));
        assert!(back.scale.abs_diff_eq(t.scale, 1e-6));
    }
}
