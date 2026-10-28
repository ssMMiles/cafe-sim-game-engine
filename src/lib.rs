// Bevy 0.19's deeper `SystemParam`/auto-trait chains (notably the render
// pipeline resources) overflow the default 128-deep trait solver. The overflow
// is currently a warning but is scheduled to become a hard error.
#![recursion_limit = "256"]
// Bevy ECS signatures trip two clippy lints by construction: a system's
// parameters are its dependency list, so eight or more `Res`/`Query` arguments
// is normal and splitting them up to satisfy the limit would obscure the
// dependencies rather than reveal them; and a `Query`'s type *is* the filter,
// so it is unavoidably verbose. Both are allowed crate-wide rather than
// sprinkled per-system. Where a query type is genuinely reusable, prefer a
// type alias anyway (see `editor::ui::PlacedQuery`).
#![allow(clippy::too_many_arguments, clippy::type_complexity)]

use avian3d::prelude::PhysicsPlugins;
use bevy::{
    asset::{AssetMetaCheck, io::AssetSourceBuilder},
    prelude::*,
    window::PresentMode,
};

#[cfg(feature = "physics_debug")]
use avian3d::debug_render::{PhysicsDebugPlugin, PhysicsGizmos};

mod editor;
mod game;
mod menu;
mod projects;
mod script_host;
mod settings;
mod storage;
mod ui;

#[cfg(target_arch = "wasm32")]
use wasm_bindgen::prelude::*;

#[cfg_attr(target_arch = "wasm32", wasm_bindgen(start))]
pub fn run() {
    // Register a custom "world" asset source pointing to the project root CWD.
    // Chunk files are loaded as "world://worlds/{id}/chunks/chunk_X_Y.terrain".
    // Must be registered before DefaultPlugins (which includes AssetPlugin).
    let mut app = App::new();
    app.register_asset_source("world", AssetSourceBuilder::platform_default(".", None));

    #[cfg(feature = "physics_debug")]
    app.add_plugins(PhysicsDebugPlugin::default())
        .insert_gizmo_config(
            PhysicsGizmos {
                collider_color: Some(Color::srgb(0.0, 0.9, 0.3)),
                ..PhysicsGizmos::none()
            },
            GizmoConfig {
                enabled: false,
                ..default()
            },
        );

    app.add_plugins(
        DefaultPlugins
            .set(AssetPlugin {
                meta_check: AssetMetaCheck::Never,
                ..default()
            })
            .set(ImagePlugin::default_nearest())
            .set(WindowPlugin {
                primary_window: Some(Window {
                    present_mode: PresentMode::Immediate,
                    ..default()
                }),
                ..default()
            }),
    )
    // .add_plugins(OutlinePlugin)
    .add_plugins(PhysicsPlugins::default())
    .add_plugins(storage::StoragePlugin)
    .add_plugins(settings::SettingsPlugin)
    .add_plugins(ui::UiPlugin)
    .add_plugins(projects::ProjectsPlugin)
    .add_plugins(menu::MenuPlugin)
    .add_plugins(game::SimulationPlugin)
    .add_plugins(editor::EditorPlugin)
    .add_plugins(script_host::ScriptHostPlugin)
    .add_systems(Startup, initialize)
    .init_state::<AppState>()
    .run();
}

pub use engine_api::AppState;

fn initialize(mut next_state: ResMut<NextState<AppState>>) {
    next_state.set(AppState::MainMenu);
}
