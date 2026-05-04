pub mod bootstrap;
pub mod constants;
pub mod edit;
pub mod lod;
pub mod material;
pub mod mesh;
pub mod reconcile;
pub mod save;
pub mod storage;
pub mod types;

use bevy::{pbr::ExtendedMaterial, prelude::*};
pub use types::{HeightmapData, TerrainChunk};

pub use constants::{CHUNK_SIZE, HEIGHT_SCALE, TILE_SIZE};

use crate::game::GameEntity;
use bootstrap::{TerrainRenderAssets, TerrainSetupMarker, create_terrain};
use lod::{TerrainLodState, update_terrain_lod};
use material::TerrainMaterial;
use storage::{TerrainChunkRegistry, TerrainWorld};

#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub enum TerrainSet {
    /// Core terrain update systems: creation, LOD, save.
    Update,
    /// Terrain editing systems: mode toggle, brush, raycasting.
    Edit,
}

pub struct TerrainPlugin;

impl Plugin for TerrainPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(MaterialPlugin::<
            ExtendedMaterial<StandardMaterial, TerrainMaterial>,
        >::default());

        app.init_resource::<TerrainLodState>();
        app.init_resource::<storage::WorldConfig>();
        app.init_resource::<save::PendingTerrainSave>();

        app.add_systems(
            Update,
            (create_terrain, update_terrain_lod).in_set(TerrainSet::Update),
        );
        app.add_systems(
            Update,
            (save::save_terrain, save::poll_terrain_save).in_set(TerrainSet::Update),
        );

        app.init_resource::<edit::TerrainBrushHit>();
        app.add_systems(
            Update,
            (
                edit::toggle_terrain_edit_mode,
                edit::adjust_brush_size,
                edit::raycast_terrain_hit,
                edit::apply_terrain_brush,
                edit::finalize_terrain_lod,
                edit::update_brush_preview,
            )
                .chain()
                .in_set(TerrainSet::Edit),
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
    commands.remove_resource::<TerrainRenderAssets>();
}
