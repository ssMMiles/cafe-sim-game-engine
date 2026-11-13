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

use avian3d::debug_render::{PhysicsDebugPlugin, PhysicsGizmos};

mod editor;
mod game;
mod menu;
mod plugin_files;
mod projects;
mod script_host;
mod settings;
mod static_plugins;
mod storage;
mod ui;

#[cfg(target_arch = "wasm32")]
use wasm_bindgen::prelude::*;

#[cfg_attr(target_arch = "wasm32", wasm_bindgen(start))]
pub fn run() {
    #[cfg(feature = "profiling")]
    trace_to_temp_dir();

    // Register a custom "world" asset source pointing to the project root CWD.
    // Chunk files are loaded as "world://worlds/{id}/chunks/chunk_X_Y.terrain".
    // Must be registered before DefaultPlugins (which includes AssetPlugin).
    let mut app = App::new();
    app.register_asset_source("world", AssetSourceBuilder::platform_default(".", None));

    app.add_plugins(PhysicsDebugPlugin).insert_gizmo_config(
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
            // Aggregates bevy's per-system spans for the editor's Performance
            // panel. The spans only exist in builds with bevy's `trace`
            // feature (cargo run-profiling); elsewhere the layer is idle.
            .set(bevy::log::LogPlugin {
                custom_layer: game::gfx::system_timing::timing_layer,
                ..default()
            })
            .set(AssetPlugin {
                meta_check: AssetMetaCheck::Never,
                ..default()
            })
            .set(ImagePlugin::default_nearest())
            .set(WindowPlugin {
                primary_window: Some(Window {
                    // Overwritten from `DisplaySettings` once the user's
                    // settings load; this is only the first frame's mode.
                    present_mode: PresentMode::AutoNoVsync,
                    ..default()
                }),
                ..default()
            })
            // Nothing of ours renders through bevy_pbr: every material has
            // its own pass (`game::gfx::scene_pass`, docs/SCENE_PASS_PLAN.md
            // Phase 5). With it gone go clustered lighting, GPU mesh
            // preprocessing, the binned phases and the `mesh_view` bind
            // groups — ~1.5 ms a frame that was being spent on nothing.
            // bevy's 3D gizmo pipeline is built on bevy_pbr and only
            // installed when it is present, so the gizmo render plugin goes
            // too; `scene_pass::gizmos` draws them instead.
            .disable::<bevy::pbr::PbrPlugin>()
            .disable::<bevy::gizmos_render::GizmoRenderPlugin>(),
    )
    // `StandardMaterial` is still an *asset*: the glTF loader produces one per
    // material and `ToonMaterial::with_surface_of` reads it. `MaterialPlugin`
    // used to register the type and the component; now we do, the way the
    // scene pass does for its own materials.
    .init_asset::<StandardMaterial>()
    .register_asset_reflect::<StandardMaterial>()
    .register_type::<MeshMaterial3d<StandardMaterial>>()
    // Per-pass GPU timings, surfaced in the editor (inspector → nothing
    // selected → GPU timings). Bevy only adds this itself under
    // `tracing-tracy`; it costs two timestamps per recorded span and falls
    // back to CPU encode times where `TIMESTAMP_QUERY` is unavailable.
    .add_plugins(bevy::render::diagnostic::RenderDiagnosticsPlugin)
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

/// Point bevy's Chrome trace at the temp dir instead of the working
/// directory (its default is `./trace-<timestamp>.json`). An explicit
/// `TRACE_CHROME` still wins. Runs first thing in `run`, before any thread
/// exists, which is what makes `set_var` sound.
#[cfg(feature = "profiling")]
fn trace_to_temp_dir() {
    if std::env::var_os("TRACE_CHROME").is_some() {
        return;
    }
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_micros());
    let path = std::env::temp_dir().join(format!("trace-{stamp}.json"));
    eprintln!("Chrome trace: {}", path.display());
    // SAFETY: single-threaded at this point; nothing else reads the
    // environment concurrently.
    unsafe { std::env::set_var("TRACE_CHROME", &path) };
}

pub use engine_api::AppState;

fn initialize(mut next_state: ResMut<NextState<AppState>>) {
    next_state.set(AppState::MainMenu);
}
