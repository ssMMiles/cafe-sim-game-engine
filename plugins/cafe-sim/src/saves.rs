//! Cafe-sim save slots.
//!
//! Four fixed slots persisted to disk at
//! `projects/cafe-sim/saves/slot{1..4}.bin`, relative to the current working
//! directory (matches the convention used by the engine's `NativeStorage`).
//!
//! On wasm this module degrades to no-ops — the game still runs, slots just
//! always read back as empty. Adding wasm persistence later means routing
//! load/save through the engine's OPFS-backed storage.

use bevy::prelude::*;
use rkyv::{Archive, Deserialize, Serialize, rancor::Error as RkyvError};
use std::collections::HashMap;

use crate::balance::{Balance, DayStartBalance};
use crate::cafe::CafeOpen;
use crate::catalog;
use crate::placement::{CafePiece, GridOccupancy, PlacedCell, spawn_piece};
use crate::shop_overview::ShopState;
use crate::staff::{Staff, StaffMember};
use crate::time::{DAY_START_HOUR, GameTime};

// ----------------------------------------------------------------------------
// Constants
// ----------------------------------------------------------------------------

/// Bump on any incompatible change to [`CafeSaveFile`] so older saves are
/// rejected with a clear error rather than silently mis-deserializing.
pub const SAVE_VERSION: u32 = 2;

pub const SLOT_COUNT: u8 = 4;

/// Storage key (path) for a given slot. Slots are 1-indexed in the UI.
pub fn slot_key(slot: u8) -> String {
    format!("projects/cafe-sim/saves/slot{slot}.bin")
}

// ----------------------------------------------------------------------------
// On-disk types
// ----------------------------------------------------------------------------

#[derive(Archive, Deserialize, Serialize, Debug, Clone)]
pub struct CafeSaveFile {
    pub version: u32,
    pub shop_name: String,
    pub balance: i64,
    pub day: u32,
    pub pieces: Vec<PlacedPieceRecord>,
    pub staff: Vec<StaffMember>,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone, Copy)]
pub struct PlacedPieceRecord {
    pub cell_x: i32,
    pub cell_z: i32,
    pub category_idx: u8,
    pub piece_idx: u8,
}

// ----------------------------------------------------------------------------
// Light metadata for the slot picker UI
// ----------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct SlotInfo {
    pub shop_name: String,
    pub day: u32,
    pub balance: i64,
}

/// Result of probing a slot on disk. `Empty` means no file; `Incompatible`
/// means a file exists but isn't readable (wrong version / corrupt). Both
/// are distinct so the UI can communicate them differently.
#[derive(Debug, Clone)]
pub enum SlotStatus {
    Empty,
    Occupied(SlotInfo),
    Incompatible,
}

/// Cached probe of every slot, refreshed on entry to the slot picker.
/// Indexed by 1-based slot number (1..=SLOT_COUNT).
#[derive(Resource, Default, Debug)]
pub struct SlotMetadataCache(pub HashMap<u8, SlotStatus>);

// ----------------------------------------------------------------------------
// Capture / apply
// ----------------------------------------------------------------------------

/// Snapshot the live game state into a serializable `CafeSaveFile`.
///
/// We persist `day_start`, the balance at the start of the current day, rather
/// than the live [`Balance`] — saves resume at opening hour, so mid-day
/// earnings are intentionally rolled back. See [`DayStartBalance`].
pub fn capture_state(
    shop: &ShopState,
    day_start: &DayStartBalance,
    time: &GameTime,
    staff: &Staff,
    pieces: &[(PlacedCell, CafePiece)],
) -> CafeSaveFile {
    let pieces = pieces
        .iter()
        .map(|(cell, piece)| PlacedPieceRecord {
            cell_x: cell.0,
            cell_z: cell.1,
            category_idx: piece.category_idx,
            piece_idx: piece.piece_idx,
        })
        .collect();

    CafeSaveFile {
        version: SAVE_VERSION,
        shop_name: shop.name.clone(),
        balance: day_start.0,
        day: time.day,
        pieces,
        staff: staff.members.clone(),
    }
}

/// Apply a loaded save to the live world. Resets the clock to opening hour
/// (the user opted for the "close out the day" resume model) and respawns
/// every recorded piece. Caller is responsible for ensuring `occupancy` is
/// empty before calling (we don't despawn existing pieces here).
pub fn apply_save(
    file: &CafeSaveFile,
    commands: &mut Commands,
    asset_server: &AssetServer,
    occupancy: &mut GridOccupancy,
    shop: &mut ShopState,
    balance: &mut Balance,
    day_start: &mut DayStartBalance,
    time: &mut GameTime,
    cafe_open: &mut CafeOpen,
    staff: &mut Staff,
) {
    shop.name = file.shop_name.clone();
    balance.0 = file.balance;
    day_start.0 = file.balance;
    time.day = file.day;
    time.seconds = DAY_START_HOUR as f32 * 3600.0;
    cafe_open.0 = false;
    staff.members = file.staff.clone();

    for rec in &file.pieces {
        let Some(piece) = catalog::lookup(rec.category_idx, rec.piece_idx) else {
            warn!(
                "save references unknown piece (cat={}, piece={}) — skipping",
                rec.category_idx, rec.piece_idx
            );
            continue;
        };
        if occupancy.0.contains_key(&(rec.cell_x, rec.cell_z)) {
            warn!(
                "save references already-occupied cell ({}, {}) — skipping piece '{}'",
                rec.cell_x, rec.cell_z, piece.name
            );
            continue;
        }
        spawn_piece(
            commands,
            asset_server,
            piece,
            (rec.category_idx, rec.piece_idx),
            (rec.cell_x, rec.cell_z),
            occupancy,
        );
    }
}

/// Restore defaults for a "new game" path — same fields apply_save touches,
/// without a save file.
pub fn apply_new_game(
    shop: &mut ShopState,
    balance: &mut Balance,
    day_start: &mut DayStartBalance,
    time: &mut GameTime,
    cafe_open: &mut CafeOpen,
    staff: &mut Staff,
) {
    *shop = ShopState::default();
    *balance = Balance::default();
    *day_start = DayStartBalance::default();
    *time = GameTime::default();
    *cafe_open = CafeOpen::default();
    *staff = Staff::default();
}

// ----------------------------------------------------------------------------
// Disk I/O
// ----------------------------------------------------------------------------

/// Serialize a save file to bytes. Pure function — no I/O.
pub fn serialize(file: &CafeSaveFile) -> Result<Vec<u8>, String> {
    rkyv::to_bytes::<RkyvError>(file)
        .map(|b| b.to_vec())
        .map_err(|e| format!("rkyv serialize: {e}"))
}

/// Parse the canonical byte representation of a save file. Returns `Err`
/// for unreadable / wrong-version data, which the slot picker shows as
/// "Incompatible".
pub fn deserialize(bytes: &[u8]) -> Result<CafeSaveFile, String> {
    let archived = rkyv::access::<ArchivedCafeSaveFile, RkyvError>(bytes)
        .map_err(|e| format!("rkyv access: {e}"))?;
    let file: CafeSaveFile = rkyv::deserialize::<CafeSaveFile, RkyvError>(archived)
        .map_err(|e| format!("rkyv deserialize: {e}"))?;
    if file.version != SAVE_VERSION {
        return Err(format!(
            "incompatible save version {} (expected {})",
            file.version, SAVE_VERSION
        ));
    }
    Ok(file)
}

/// Probe a slot and return its status. Used to populate
/// [`SlotMetadataCache`] when the slot picker opens.
pub fn probe_slot(slot: u8) -> SlotStatus {
    match load_raw(slot) {
        Ok(None) => SlotStatus::Empty,
        Ok(Some(bytes)) => match deserialize(&bytes) {
            Ok(file) => SlotStatus::Occupied(SlotInfo {
                shop_name: file.shop_name,
                day: file.day,
                balance: file.balance,
            }),
            Err(e) => {
                warn!("slot {slot} present but unreadable: {e}");
                SlotStatus::Incompatible
            }
        },
        Err(e) => {
            warn!("slot {slot} read error: {e}");
            SlotStatus::Incompatible
        }
    }
}

/// Load and deserialize a slot's save file. `Ok(None)` if the file
/// doesn't exist.
pub fn load_slot(slot: u8) -> Result<Option<CafeSaveFile>, String> {
    match load_raw(slot)? {
        None => Ok(None),
        Some(bytes) => deserialize(&bytes).map(Some),
    }
}

/// Write a save file to a slot. Creates parent dirs if needed.
pub fn save_slot(slot: u8, file: &CafeSaveFile) -> Result<(), String> {
    let bytes = serialize(file)?;
    save_raw(slot, &bytes)
}

#[cfg(not(target_arch = "wasm32"))]
fn load_raw(slot: u8) -> Result<Option<Vec<u8>>, String> {
    use std::fs;
    use std::io::ErrorKind;
    let path = slot_key(slot);
    match fs::read(&path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("read {path}: {e}")),
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn save_raw(slot: u8, bytes: &[u8]) -> Result<(), String> {
    use std::fs;
    use std::path::Path;
    let path = slot_key(slot);
    if let Some(parent) = Path::new(&path).parent() {
        fs::create_dir_all(parent).map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
    }
    fs::write(&path, bytes).map_err(|e| format!("write {path}: {e}"))
}

#[cfg(target_arch = "wasm32")]
fn load_raw(_slot: u8) -> Result<Option<Vec<u8>>, String> {
    Ok(None)
}

#[cfg(target_arch = "wasm32")]
fn save_raw(_slot: u8, _bytes: &[u8]) -> Result<(), String> {
    warn!("save not implemented on wasm — skipping");
    Ok(())
}
