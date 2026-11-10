pub mod bootstrap;
pub mod constants;
pub mod lod;
pub mod material;
pub mod mesh;
pub mod paint;
pub mod palette_sync;
pub mod reconcile;
pub mod save;
pub mod storage;
pub mod types;

use bevy::prelude::*;
pub use types::{HeightmapData, TerrainChunk};

pub use constants::{CHUNK_SIZE, HEIGHT_SCALE, TILE_SIZE};

/// Chunks whose grids include global tile `(tx, ty)`: its own, plus the
/// neighbours that share it when it sits on a chunk edge (chunks overlap by
/// one tile row). Heights and paint both follow this.
pub fn chunks_touching_tile(tx: i32, ty: i32) -> impl Iterator<Item = (i32, i32)> {
    let cs = (CHUNK_SIZE - 1) as i32;
    let (cx, cy) = (tx.div_euclid(cs), ty.div_euclid(cs));
    let on_x = tx.rem_euclid(cs) == 0;
    let on_y = ty.rem_euclid(cs) == 0;
    [
        Some((cx, cy)),
        on_x.then_some((cx - 1, cy)),
        on_y.then_some((cx, cy - 1)),
        (on_x && on_y).then_some((cx - 1, cy - 1)),
    ]
    .into_iter()
    .flatten()
}

use crate::game::GameEntity;
use bootstrap::{TerrainRenderAssets, TerrainSetupMarker, create_terrain};
use lod::{TerrainLodSettings, TerrainLodState, update_terrain_lod};
use storage::{TerrainChunkRegistry, TerrainWorld};

#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub enum TerrainSet {
    /// Core terrain update systems: creation, LOD, save.
    Update,
}

pub struct TerrainPlugin;

impl Plugin for TerrainPlugin {
    fn build(&self, app: &mut App) {
        // No `MaterialPlugin::<TerrainMaterial>`: terrain draws through our
        // own scene pass, whose `SceneMaterialPlugin` registers the asset type
        // (`gfx::scene_pass`, docs/SCENE_PASS_PLAN.md).

        app.init_resource::<TerrainLodState>();
        app.init_resource::<TerrainLodSettings>();
        app.init_resource::<storage::WorldConfig>();
        app.init_resource::<save::PendingTerrainSave>();
        app.add_message::<save::SaveTerrainMessage>();

        app.init_resource::<palette_sync::PaletteTextures>();
        app.add_systems(
            Update,
            (
                create_terrain,
                update_terrain_lod,
                palette_sync::sync_palette,
            )
                .in_set(TerrainSet::Update),
        );
        app.add_systems(
            Update,
            (save::save_terrain, save::poll_terrain_save).in_set(TerrainSet::Update),
        );
    }
}

/// Kick off async terrain bootstrap. Spawning a [`TerrainSetupMarker`] is the
/// hand-off into the bootstrap state machine — it issues the world-meta load,
/// then inserts [`TerrainWorld`], [`TerrainChunkRegistry`], [`HeightmapData`],
/// and the shared render assets when the load completes.
pub fn setup(commands: &mut Commands) {
    commands.spawn((GameEntity, TerrainSetupMarker));
}

/// Drop terrain-owned resources so they don't leak between game sessions /
/// projects. The streaming, save, and bake systems all use `Option<Res<…>>`
/// for these, so they no-op until the next [`setup`] re-inserts them.
pub fn destroy(commands: &mut Commands) {
    commands.remove_resource::<TerrainWorld>();
    commands.remove_resource::<TerrainChunkRegistry>();
    commands.remove_resource::<HeightmapData>();
    commands.remove_resource::<paint::PaintmapData>();
    commands.remove_resource::<TerrainRenderAssets>();
}
