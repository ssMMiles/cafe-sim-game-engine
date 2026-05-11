use bevy::prelude::*;
use engine_api::{AutoCollider, DynamicPlugin, ScriptOwned, add};

mod balance;
mod catalog;
mod cursor;
mod grid;
mod placement;
mod shop;

use balance::{Balance, spawn_balance_hud, update_balance_text};
use cursor::manage_cursor;
use grid::{FLOOR_Y, draw_floor_grid};
use placement::{
    GridOccupancy, PlacementSelection, PreviewEntity, apply_translucent_when_loaded,
    cancel_placement, handle_place_click, manage_placement_hint, sync_preview,
    update_preview_transform,
};
use shop::{ShopOpen, build_shop_ui, handle_card_click, teardown_shop_ui, toggle_shop};

#[derive(Default)]
pub struct CafeSimPlugin;

impl DynamicPlugin for CafeSimPlugin {
    fn setup(&self, world: &mut World, schedule: &mut Schedule) {
        let cafe: Handle<Scene> = world
            .resource::<AssetServer>()
            .load("cafe-sim/Cafe.glb#Scene0");

        world.spawn((
            SceneRoot(cafe),
            Transform::from_xyz(0.0, FLOOR_Y, 0.0),
            ScriptOwned::new(PLUGIN_NAME),
            AutoCollider,
        ));

        world.insert_resource(ShopOpen::default());
        world.insert_resource(PlacementSelection::default());
        world.insert_resource(GridOccupancy::default());
        world.insert_resource(PreviewEntity::default());
        world.insert_resource(Balance::default());

        spawn_balance_hud(world);

        schedule.add_systems(
            (
                toggle_shop,
                cancel_placement,
                handle_card_click,
                build_shop_ui,
                teardown_shop_ui,
                sync_preview,
                apply_translucent_when_loaded,
                update_preview_transform,
                handle_place_click,
                manage_cursor,
                manage_placement_hint,
                draw_floor_grid,
                update_balance_text,
            )
                .chain(),
        );

        info!("[{PLUGIN_NAME}] loaded");
    }
}

add!(CafeSimPlugin, "cafe-sim");
