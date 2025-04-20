use bevy::{log, prelude::*};

use crate::AppState;

mod main;

#[derive(Default, Clone, Copy, PartialEq, Eq, Hash, Debug, States)]
pub enum MenuScreen {
    #[default]
    None,
    Main,
    Settings,
}

pub struct MenuPlugin;
impl Plugin for MenuPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(OnEnter(AppState::MainMenu), setup_menu);
        app.add_systems(OnExit(AppState::MainMenu), destroy_menu);

        app.add_systems(OnEnter(MenuScreen::Main), main::setup_main_menu);
        app.add_systems(
            OnExit(MenuScreen::Main),
            despawn_screen::<main::MainMenuScreen>,
        );

        app.add_systems(
            Update,
            main::handle_main_menu_button_press.run_if(in_state(MenuScreen::Main)),
        );

        app.init_state::<MenuScreen>();
    }
}

#[derive(Component, Debug)]
pub struct MenuCamera;

fn setup_menu(mut commands: Commands, mut next_state: ResMut<NextState<MenuScreen>>) {
    log::info!("Setting up menu");
    commands.spawn((Camera2d::default(), MenuCamera));
    next_state.set(MenuScreen::Main);
}

fn destroy_menu(
    mut commands: Commands,
    camera_query: Query<Entity, With<MenuCamera>>,
    mut menu_state: ResMut<NextState<MenuScreen>>,
) {
    log::info!("Destroying menu");

    for entity in camera_query.iter() {
        commands.entity(entity).despawn_recursive();
    }

    menu_state.set(MenuScreen::None);
}

fn despawn_screen<T: Component>(to_despawn: Query<Entity, With<T>>, mut commands: Commands) {
    for entity in &to_despawn {
        commands.entity(entity).despawn_recursive();
    }
}
