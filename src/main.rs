use bevy::prelude::*;

mod freecam;
mod game;
mod menu;

#[derive(Default, Clone, Copy, PartialEq, Eq, Hash, Debug, States)]
pub enum AppState {
    MainMenu,
    InGame,
    #[default]
    Starting,
}

fn main() {
    App::new()
        .add_plugins(DefaultPlugins)
        .add_plugins(menu::MenuPlugin)
        .add_plugins(game::SimulationPlugin)
        .add_systems(Startup, initialize)
        .init_state::<AppState>()
        .run();
}

fn initialize(mut next_state: ResMut<NextState<AppState>>) {
    next_state.set(AppState::MainMenu);
}
