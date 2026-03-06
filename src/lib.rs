use avian3d::{debug_render::{PhysicsDebugPlugin, PhysicsGizmos}, prelude::PhysicsPlugins};
use bevy::{asset::AssetMetaCheck, prelude::*, window::PresentMode};

mod game;
mod menu;
mod settings;

#[cfg(target_arch = "wasm32")]
use wasm_bindgen::prelude::*;

#[cfg_attr(target_arch = "wasm32", wasm_bindgen(start))]
pub fn run() {
    App::new()
        .add_plugins(
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
        .add_plugins(PhysicsDebugPlugin::default())
        .insert_gizmo_config(
            PhysicsGizmos {
                collider_color: Some(Color::srgb(0.0, 0.9, 0.3)),
                ..PhysicsGizmos::none()
            },
            GizmoConfig { enabled: false, ..default() },
        )
        .add_plugins(settings::SettingsPlugin)
        .add_plugins(menu::MenuPlugin)
        .add_plugins(game::SimulationPlugin)
        .add_systems(Startup, initialize)
        .init_state::<AppState>()
        .run();
}

#[derive(Default, Clone, Copy, PartialEq, Eq, Hash, Debug, States)]
pub enum AppState {
    MainMenu,
    InGame,
    #[default]
    Starting,
}

fn initialize(mut next_state: ResMut<NextState<AppState>>) {
    next_state.set(AppState::MainMenu);
}
