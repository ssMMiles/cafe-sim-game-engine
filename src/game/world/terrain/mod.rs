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

use lod::{TerrainLodState, update_terrain_lod};
use material::TerrainMaterial;

pub use bootstrap::TerrainRenderAssets;
use bootstrap::create_terrain;

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

        app.add_systems(
            Update,
            (create_terrain, update_terrain_lod).in_set(TerrainSet::Update),
        );
        app.add_systems(Update, save::save_terrain.in_set(TerrainSet::Update));

        app.init_resource::<edit::TerrainBrushHit>();
        app.add_systems(
            Update,
            (
                edit::toggle_terrain_edit_mode,
                edit::adjust_brush_size,
                edit::raycast_terrain_hit,
                edit::apply_terrain_brush,
                edit::finalize_terrain_lod,
            )
                .chain()
                .in_set(TerrainSet::Edit),
        );
    }
}
