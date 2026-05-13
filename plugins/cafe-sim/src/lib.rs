use bevy::prelude::*;
use engine_api::{AutoCollider, DynamicPlugin, add};

mod balance;
mod cafe;
mod catalog;
mod coffee_menu;
mod cursor;
mod customer;
mod delivery;
mod drinks;
mod grid;
mod menus;
mod pause_menu;
mod placement;
mod shop;
mod shop_overview;
mod spawn;
mod thumbnails;
mod time;
mod ui;

use balance::{Balance, spawn_balance_hud, tick_balance_popups, update_balance_text};
use cafe::{CafeOpen, enforce_closing_time};
use coffee_menu::{
    ActiveCoffeeMachine, handle_drink_click, handle_machine_click, manage_brew_popups,
    position_brew_popups, sync_active_coffee_machine, sync_coffee_menu_ui, tick_brewing,
    update_brew_popups,
};
use cursor::manage_cursor;
use customer::{
    CustomerSpawnTimer, clear_customers_when_closed, init_customer_assets, manage_customer_popups,
    position_customer_popups, spawn_customers, tick_spawn_timer, walk_customers,
};
use delivery::{
    CarriedDrink, cleanup_orphan_drinks, handle_delivery_click, handle_discard_click,
    handle_pickup_click, init_delivery_assets, manage_carried_visual, manage_ready_drinks,
    tick_served_timer,
};
use grid::{FLOOR_Y, draw_floor_grid};
use menus::{ActiveMenu, close_menus_when_paused};
use pause_menu::{GamePaused, handle_pause_buttons, sync_pause_menu_ui, toggle_pause_on_escape};
use placement::{
    GridOccupancy, PlacementSelection, PreviewEntity, apply_paint_when_loaded, cancel_placement,
    handle_place_click, manage_placement_hint, sync_preview, update_preview_transform,
};
use shop::{handle_card_click, sync_shop_ui, toggle_shop};
use shop_overview::{
    ShopNameEditing, ShopState, apply_shop_name_keystrokes, build_shop_overview,
    cleanup_shop_overview_when_inactive, focus_shop_name_input, handle_close_cafe_button,
    handle_edit_button, handle_open_cafe_button, sync_shop_name_display, toggle_shop_overview,
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

        world.insert_resource(ActiveMenu::default());
        world.insert_resource(ActiveCoffeeMachine::default());
        world.insert_resource(CarriedDrink::default());
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
        init_delivery_assets(world);
        init_thumbnails(world);

        spawn_balance_hud(world);
        spawn_time_hud(world);

        schedule.add_systems(
            (
                (
                    // Click consumers — ordered so earlier ones can
                    // `clear_just_pressed` to swallow the click from later ones.
                    // `handle_delivery_click` and `handle_pickup_click` run before
                    // `handle_drink_click`/`handle_machine_click` so that picking
                    // up or delivering a drink doesn't also re-toggle the brew
                    // menu over the same machine.
                    (
                        toggle_pause_on_escape,
                        toggle_shop,
                        toggle_shop_overview,
                        handle_delivery_click,
                        handle_discard_click,
                        handle_pickup_click,
                        handle_drink_click,
                        handle_machine_click,
                        handle_card_click,
                        handle_edit_button,
                        handle_open_cafe_button,
                        handle_close_cafe_button,
                        handle_pause_buttons,
                        cancel_placement,
                    )
                        .chain(),
                    // Menu/state syncs — depend on the click handlers above
                    // having run this frame.
                    (
                        close_menus_when_paused,
                        cleanup_shop_overview_when_inactive,
                        sync_pause_menu_ui,
                        focus_shop_name_input,
                        apply_shop_name_keystrokes,
                        sync_shop_ui,
                        sync_coffee_menu_ui,
                        sync_active_coffee_machine,
                        build_shop_overview,
                    )
                        .chain(),
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
                    clear_customers_when_closed,
                    tick_spawn_timer,
                    spawn_customers,
                    walk_customers,
                    manage_customer_popups,
                    position_customer_popups,
                )
                    .chain(),
                (
                    tick_brewing,
                    tick_served_timer,
                    manage_carried_visual,
                    manage_ready_drinks,
                    cleanup_orphan_drinks,
                    tick_balance_popups,
                    manage_brew_popups,
                    update_brew_popups,
                    position_brew_popups,
                )
                    .chain(),
            )
                .chain(),
        );

        info!("[{PLUGIN_NAME}] loaded");
    }
}

add!(CafeSimPlugin, "cafe-sim");
