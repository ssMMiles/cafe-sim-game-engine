use bevy::{log, prelude::*};

use crate::{AppState, ui::clear_focus};

mod create_project;
mod main;

#[derive(Default, Clone, Copy, PartialEq, Eq, Hash, Debug, States)]
pub enum MenuScreen {
    #[default]
    None,
    Main,
    CreateProject,
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

        app.add_systems(OnEnter(MenuScreen::CreateProject), create_project::setup);
        app.add_systems(
            OnExit(MenuScreen::CreateProject),
            (
                despawn_screen::<create_project::CreateProjectScreen>,
                clear_focus,
            ),
        );
        app.add_systems(
            Update,
            create_project::handle_actions.run_if(in_state(MenuScreen::CreateProject)),
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
        commands.entity(entity).despawn();
    }

    menu_state.set(MenuScreen::None);
}

fn despawn_screen<T: Component>(to_despawn: Query<Entity, With<T>>, mut commands: Commands) {
    for entity in &to_despawn {
        commands.entity(entity).despawn();
    }
}
