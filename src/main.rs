use bevy::{prelude::*, window::{PresentMode, Window, WindowPlugin}};
// use bevy_mod_outline::OutlinePlugin;

mod game;
mod menu;
mod settings;

#[derive(Default, Clone, Copy, PartialEq, Eq, Hash, Debug, States)]
pub enum AppState {
    MainMenu,
    InGame,
    #[default]
    Starting,
}

fn main() {
    App::new()
        .add_plugins(DefaultPlugins.set(ImagePlugin::default_nearest()).set(WindowPlugin {
            primary_window: Some(Window {
                present_mode: PresentMode::Immediate,
                ..default()
            }),
            ..default()
        }))
        // .add_plugins(OutlinePlugin)
        .add_plugins(settings::SettingsPlugin)
        .add_plugins(menu::MenuPlugin)
        .add_plugins(game::SimulationPlugin)
        .add_systems(Startup, initialize)
        .init_state::<AppState>()
        .run();
}

fn initialize(mut next_state: ResMut<NextState<AppState>>) {
    next_state.set(AppState::MainMenu);
}
