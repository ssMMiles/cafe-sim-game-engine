use bevy::prelude::*;
use engine_api::{AutoCollider, DynamicPlugin, add};

mod balance;
mod cafe;
mod catalog;
mod cursor;
mod customer;
mod grid;
mod pause_menu;
mod placement;
mod shop;
mod shop_overview;
mod spawn;
mod thumbnails;
mod time;
mod ui;

use balance::{Balance, spawn_balance_hud, update_balance_text};
use cafe::{CafeOpen, enforce_closing_time};
use cursor::manage_cursor;
use customer::{
    CustomerSpawnTimer, init_customer_assets, manage_customer_popups, position_customer_popups,
    spawn_customers, tick_spawn_timer, walk_customers,
};
use grid::{FLOOR_Y, draw_floor_grid};
use pause_menu::{GamePaused, handle_pause_buttons, sync_pause_menu_ui, toggle_pause_on_escape};
use placement::{
    GridOccupancy, PlacementSelection, PreviewEntity, apply_paint_when_loaded,
    cancel_placement, handle_place_click, manage_placement_hint, sync_preview,
    update_preview_transform,
};
use shop::{ShopOpen, build_shop_ui, handle_card_click, teardown_shop_ui, toggle_shop};
use shop_overview::{
    ShopNameEditing, ShopOverviewOpen, ShopState, apply_shop_name_keystrokes,
    build_shop_overview, focus_shop_name_input, handle_close_cafe_button, handle_edit_button,
    handle_open_cafe_button, sync_shop_name_display, toggle_shop_overview,
};
use spawn::SpawnOwnedWorld;
use thumbnails::{init_thumbnails, propagate_thumbnail_layers};
use time::{GameTime, advance_game_time, spawn_time_hud, update_time_text};
use ui::{UiTheme, update_interactive_visuals};

#[derive(Default)]
pub struct CafeSimPlugin;

impl DynamicPlugin for CafeSimPlugin {
    fn setup(&self, world: &mut World, schedule: &mut Schedule) {
        let cafe: Handle<Scene> = world
            .resource::<AssetServer>()
            .load("cafe-sim/Cafe.glb#Scene0");

        world.spawn_owned((
            SceneRoot(cafe),
            Transform::from_xyz(0.0, FLOOR_Y, 0.0),
            AutoCollider,
        ));

        world.insert_resource(ShopOpen::default());
        world.insert_resource(ShopOverviewOpen::default());
        world.insert_resource(ShopNameEditing::default());
        world.insert_resource(ShopState::default());
        world.insert_resource(CafeOpen::default());
        world.insert_resource(PlacementSelection::default());
        world.insert_resource(GridOccupancy::default());
        world.insert_resource(PreviewEntity::default());
        world.insert_resource(Balance::default());
        world.insert_resource(GameTime::default());
        world.insert_resource(UiTheme::default());
        world.insert_resource(CustomerSpawnTimer::default());

        // Start every load with the pause flag cleared — stale `true` from a
        // prior session would pop the menu open the instant the plugin runs.
        world.insert_resource(GamePaused(false));

        init_customer_assets(world);
        init_thumbnails(world);

        spawn_balance_hud(world);
        spawn_time_hud(world);

        schedule.add_systems(
            (
                (
                    toggle_pause_on_escape,
                    sync_pause_menu_ui,
                    handle_pause_buttons,
                    toggle_shop,
                    toggle_shop_overview,
                    focus_shop_name_input,
                    apply_shop_name_keystrokes,
                    handle_edit_button,
                    handle_open_cafe_button,
                    handle_close_cafe_button,
                    handle_card_click,
                    cancel_placement,
                    build_shop_ui,
                    teardown_shop_ui,
                    build_shop_overview,
                )
                    .chain(),
                (
                    sync_shop_name_display,
                    sync_preview,
                    apply_paint_when_loaded,
                    propagate_thumbnail_layers,
                    update_preview_transform,
                    handle_place_click,
                    manage_cursor,
                    manage_placement_hint,
                    draw_floor_grid,
                    update_balance_text,
                    advance_game_time,
                    enforce_closing_time,
                    update_time_text,
                    update_interactive_visuals,
                    tick_spawn_timer,
                    spawn_customers,
                    walk_customers,
                    manage_customer_popups,
                    position_customer_popups,
                )
                    .chain(),
            )
                .chain(),
        );

        info!("[{PLUGIN_NAME}] loaded");
    }
}

add!(CafeSimPlugin, "cafe-sim");
