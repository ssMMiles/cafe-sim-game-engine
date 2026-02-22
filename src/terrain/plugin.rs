use bevy::{pbr::ExtendedMaterial, prelude::*};

use super::{
    bootstrap::create_heightmap_mesh,
    edit,
    lod::{TerrainLodState, update_terrain_lod},
    material::TerrainMaterial,
    save, storage,
};

pub struct HeightmapPlugin;

impl Plugin for HeightmapPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(MaterialPlugin::<
            ExtendedMaterial<StandardMaterial, TerrainMaterial>,
        >::default());

        app.init_resource::<TerrainLodState>();
        app.init_resource::<storage::WorldConfig>();

        app.add_systems(Update, (create_heightmap_mesh, update_terrain_lod));
        app.add_systems(Update, save::save_terrain);

        app.init_resource::<edit::TerrainBrushHit>();
        app.add_systems(
            Update,
            (
                edit::toggle_terrain_edit_mode,
                edit::adjust_brush_size,
                edit::raycast_terrain_hit,
                edit::apply_terrain_brush,
            )
                .chain(),
        );
    }
}
