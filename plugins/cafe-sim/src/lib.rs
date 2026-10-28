// Bevy ECS signatures trip two clippy lints by construction: a system's
// parameters are its dependency list, so eight or more `Res`/`Query` arguments
// is normal and splitting them up to satisfy the limit would obscure the
// dependencies rather than reveal them; and a `Query`'s type *is* the filter,
// so it is unavoidably verbose. Both are allowed crate-wide rather than
// sprinkled per-system. Where a query type is genuinely reusable, prefer a
// type alias anyway (see `editor::ui::PlacedQuery`).
#![allow(clippy::too_many_arguments, clippy::type_complexity)]

use bevy::prelude::*;
use engine_api::{DynamicPlugin, Player, ScriptBinding, ScriptRegistry, ScriptSchema, add};

mod balance;
mod cafe;
mod cafe_scene;
mod catalog;
mod coffee_menu;
mod cursor;
mod customer;
mod delivery;
mod drinks;
mod grid;
mod main_menu;
mod menus;
mod pause_menu;
mod placement;
mod saves;
mod shop;
mod shop_overview;
mod slot_picker;
mod spawn;
mod staff;
mod staff_agent;
mod thumbnails;
mod time;
mod ui;

use balance::{
    Balance, DayStartBalance, spawn_balance_hud, tick_balance_popups, update_balance_text,
};
use cafe::{CafeOpen, enforce_closing_time};
use cafe_scene::{CafeShell, sync_cafe_anchor};
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
use grid::{CafeAnchor, draw_floor_grid};
use main_menu::{
    CafePhase, MainMenuRoot, build_main_menu, handle_main_menu_buttons, in_main_menu, in_playing,
    in_slot_picker,
};
use menus::{ActiveMenu, close_menus_when_paused};
use pause_menu::{GamePaused, handle_pause_buttons, sync_pause_menu_ui, toggle_pause_on_escape};
use placement::{
    GridOccupancy, PlacementSelection, PreviewEntity, apply_paint_when_loaded, cancel_placement,
    handle_place_click, manage_placement_hint, sync_preview, update_preview_transform,
};
use saves::SlotMetadataCache;
use shop::{handle_card_click, sync_shop_ui, toggle_shop};
use shop_overview::{
    ShopNameEditing, ShopState, apply_shop_name_keystrokes, build_shop_overview,
    cleanup_shop_overview_when_inactive, focus_shop_name_input, handle_close_cafe_button,
    handle_edit_button, handle_open_cafe_button, sync_shop_name_display, toggle_shop_overview,
};
use slot_picker::{SlotPickerRoot, build_slot_picker, handle_slot_picker_buttons};
use staff::{
    AssignJobOpenFor, HireCandidates, Staff, build_hire_menu, handle_assign_job_button,
    handle_hire_back_button, handle_hire_candidate_click, handle_hire_staff_button,
    handle_job_option_click,
};
use staff_agent::{
    PayrollNotice, init_staff_assets, staff_day_lifecycle, tick_notice_toast, tick_staff,
};
use thumbnails::{init_thumbnails, propagate_thumbnail_layers};
use time::{GameTime, advance_game_time, spawn_time_hud, update_time_text};
use ui::{UiTheme, update_interactive_visuals};

#[derive(Default)]
pub struct CafeSimPlugin;

/// Asset the editor offers when placing the `cafe-shell` script, so the
/// shell is a one-click "mesh + binding" entry in the placement catalog.
const CAFE_SHELL_MESH: &str = "cafe-sim/Cafe.gltf#Scene0";

impl DynamicPlugin for CafeSimPlugin {
    /// Registration only — runs in editor mode too, which is what puts
    /// `cafe-sim/cafe-shell` in the placement catalog while the plugin's
    /// gameplay systems are stopped.
    fn register(&self, _world: &mut World, registry: &mut ScriptRegistry) {
        registry.register_script(
            PLUGIN_NAME,
            ScriptSchema::new("cafe-shell").with_default_mesh(CAFE_SHELL_MESH),
            hydrate_cafe_shell,
        );
    }

    fn setup(&self, world: &mut World, schedule: &mut Schedule) {
        // Resources are inserted unconditionally so they exist while the
        // menu phases run; the slot picker overwrites them when a save is
        // loaded, and the "new game" path resets them via `apply_new_game`.
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
        world.insert_resource(DayStartBalance::default());
        world.insert_resource(GameTime::default());
        world.insert_resource(UiTheme::default());
        world.insert_resource(CustomerSpawnTimer::default());
        world.insert_resource(Staff::default());
        world.insert_resource(HireCandidates::default());
        world.insert_resource(AssignJobOpenFor::default());
        world.insert_resource(PayrollNotice::default());
        world.init_resource::<CafeAnchor>();

        // Start every load with the pause flag cleared — stale `true` from a
        // prior session would pop the menu open the instant the plugin runs.
        world.insert_resource(GamePaused(false));

        // New: phase machine + slot metadata cache. The transition system
        // below picks `MainMenu` up as a fresh change on the first frame
        // and builds the main-menu UI in response.
        world.insert_resource(CafePhase::default());
        world.insert_resource(SlotMetadataCache::default());

        init_customer_assets(world);
        init_delivery_assets(world);
        init_staff_assets(world);
        init_thumbnails(world);

        // Cafe scene + HUDs are no longer spawned here; the transition
        // system spawns them when entering `CafePhase::Playing`.

        // Phase enter/exit hooks. Must run before any phase-gated system
        // so newly-spawned UI is queryable this frame and outgoing UI is
        // gone before its handlers fire.
        schedule.add_systems(apply_phase_transitions);

        // Always-on UI visuals (hover/press styling for any button in any
        // phase), cursor management (which needs to react to phase changes
        // so the main menu/slot picker get a free, visible cursor), and
        // menu-phase button handlers.
        schedule.add_systems((
            update_interactive_visuals,
            manage_cursor,
            // The shell is a level entity, so it is there in every phase —
            // keep the anchor tracking it regardless. Its materials are the
            // level's business, not ours: they ship inside `Cafe.gltf`.
            sync_cafe_anchor,
            handle_main_menu_buttons.run_if(in_main_menu),
            handle_slot_picker_buttons.run_if(in_slot_picker),
        ));

        // Gameplay — the entire prior schedule, gated to Playing.
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
                        handle_hire_staff_button,
                        handle_hire_candidate_click,
                        handle_hire_back_button,
                        handle_assign_job_button,
                        handle_job_option_click,
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
                        build_hire_menu,
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
                    manage_placement_hint,
                    draw_floor_grid,
                    update_balance_text,
                    advance_game_time,
                    enforce_closing_time,
                    update_time_text,
                    clear_customers_when_closed,
                    staff_day_lifecycle,
                    tick_staff,
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
                    tick_notice_toast,
                    manage_brew_popups,
                    update_brew_popups,
                    position_brew_popups,
                )
                    .chain(),
            )
                .chain()
                .run_if(in_playing),
        );

        info!("[{PLUGIN_NAME}] loaded");
    }
}

/// Single transition system that despawns the outgoing screen UI and sets
/// up the incoming one whenever [`CafePhase`] changes. Skips work when
/// the phase is unchanged so it's cheap to run every frame.
fn apply_phase_transitions(
    mut last: Local<Option<CafePhase>>,
    phase: Res<CafePhase>,
    mut commands: Commands,
    theme: Res<UiTheme>,
    mut cache: ResMut<SlotMetadataCache>,
    balance: Res<Balance>,
    time: Res<GameTime>,
    anchor: Res<CafeAnchor>,
    main_menu_q: Query<Entity, With<MainMenuRoot>>,
    slot_picker_q: Query<Entity, With<SlotPickerRoot>>,
    mut player_q: Query<&mut Transform, With<Player>>,
) {
    let curr = *phase;
    if *last == Some(curr) {
        return;
    }
    let prev = *last;
    *last = Some(curr);

    match prev {
        Some(CafePhase::MainMenu) => {
            for e in &main_menu_q {
                commands.entity(e).despawn();
            }
        }
        Some(CafePhase::SlotPicker(_)) => {
            for e in &slot_picker_q {
                commands.entity(e).despawn();
            }
        }
        // Exits from `Playing` happen via plugin teardown (AppState::MainMenu),
        // not via phase change — nothing to do here.
        _ => {}
    }

    match curr {
        CafePhase::MainMenu => {
            build_main_menu(&mut commands, &theme);
        }
        CafePhase::SlotPicker(mode) => {
            build_slot_picker(&mut commands, &theme, mode, &mut cache);
        }
        CafePhase::Playing { .. } => {
            // The slot-picker click handler has already applied save data
            // (or new-game defaults) by the time we get here; we just
            // bring up the gameplay HUDs. The cafe shell itself is a placed
            // level entity now, not something this plugin spawns.
            spawn_balance_hud(&mut commands, &balance);
            spawn_time_hud(&mut commands, &time);

            // Move the engine-spawned player to the cafe entrance so they
            // don't end up clipping into furniture that was loaded from a
            // save (or placed by `apply_save` in the slot picker handler).
            if let Ok(mut player_tf) = player_q.single_mut() {
                player_tf.translation = anchor.player_spawn();
            }
        }
    }
}

/// Turn a `cafe-shell` binding into the marker the anchor and dressing
/// systems look for.
fn hydrate_cafe_shell(entity: &mut bevy::ecs::world::EntityWorldMut, _binding: &ScriptBinding) {
    entity.insert(CafeShell);
}

add!(CafeSimPlugin, "cafe-sim");
