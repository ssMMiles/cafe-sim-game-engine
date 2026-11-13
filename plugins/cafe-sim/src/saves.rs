//! Cafe-sim save slots.
//!
//! Four fixed slots, `slot{1..4}.bin` in this plugin's
//! [`PluginFiles`] folder — `projects/{project}/saves/cafe-sim/` on disk
//! natively, the same key in OPFS on the web. Reads come from the host's
//! in-memory mirror and writes are persisted in the background, so the main
//! menu waits for [`PluginFiles::is_loaded`] before offering the slots.

use bevy::prelude::*;
use engine_api::{FileRead, PluginFiles};
use rkyv::{Archive, Deserialize, Serialize, rancor::Error as RkyvError};
use std::collections::HashMap;

use crate::PLUGIN_NAME;
use crate::balance::{Balance, DayStartBalance};
use crate::cafe::CafeOpen;
use crate::catalog;
use crate::grid::CafeAnchor;
use crate::placement::{CafePiece, GridOccupancy, PlacedCell, spawn_piece};
use crate::shop_overview::ShopState;
use crate::staff::{Staff, StaffMember};
use crate::time::{DAY_START_HOUR, GameTime};

// ----------------------------------------------------------------------------
// Constants
// ----------------------------------------------------------------------------

/// Bump on any incompatible change to [`CafeSaveFile`] so older saves are
/// rejected with a clear error rather than silently mis-deserializing.
pub const SAVE_VERSION: u32 = 3;

pub const SLOT_COUNT: u8 = 4;

/// [`PluginFiles`] key for a given slot. Slots are 1-indexed in the UI.
fn slot_key(slot: u8) -> String {
    format!("slot{slot}.bin")
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
    anchor: &CafeAnchor,
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
        if !anchor.contains((rec.cell_x, rec.cell_z)) {
            // The grid is measured from whatever shell this level has, so a
            // save made in a roomier cafe can name cells this one doesn't
            // have. Dropping the piece beats placing it outside the walls.
            warn!(
                "save references cell ({}, {}), outside this cafe's {}x{} floor — skipping \
                 piece '{}'",
                rec.cell_x,
                rec.cell_z,
                anchor.width(),
                anchor.depth(),
                piece.name
            );
            continue;
        }
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
            anchor,
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
pub fn probe_slot(files: &PluginFiles, slot: u8) -> SlotStatus {
    match files.read(PLUGIN_NAME, &slot_key(slot)) {
        FileRead::Missing => SlotStatus::Empty,
        FileRead::Present(bytes) => match deserialize(bytes) {
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
        // The main menu doesn't open the picker before the files are read;
        // if it ever does, don't let New Game treat a real save as empty.
        FileRead::Loading => {
            warn!("slot {slot} probed before the save files were read");
            SlotStatus::Incompatible
        }
    }
}

/// Load and deserialize a slot's save file. `Ok(None)` if there is none.
pub fn load_slot(files: &PluginFiles, slot: u8) -> Result<Option<CafeSaveFile>, String> {
    match files.read(PLUGIN_NAME, &slot_key(slot)) {
        FileRead::Present(bytes) => deserialize(bytes).map(Some),
        FileRead::Missing => Ok(None),
        FileRead::Loading => Err("save files are still being read".into()),
    }
}

/// Write a save file to a slot. Persisted by the host in the background.
pub fn save_slot(files: &mut PluginFiles, slot: u8, file: &CafeSaveFile) -> Result<(), String> {
    files.write(PLUGIN_NAME, &slot_key(slot), serialize(file)?);
    Ok(())
}
