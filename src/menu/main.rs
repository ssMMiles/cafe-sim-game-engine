use bevy::{log, prelude::*};

use crate::{
    AppState,
    projects::{ActiveProject, ProjectRegistry},
    ui::{ButtonVariant, UiExt, UiTheme},
};

use super::MenuScreen;

#[derive(Component, Debug)]
pub struct MainMenuScreen;

#[derive(Component, Debug)]
pub enum MainMenuButton {
    SelectProject(String),
    NewProject,
}

pub fn setup_main_menu(
    mut commands: Commands,
    theme: Res<UiTheme>,
    registry: Res<ProjectRegistry>,
) {
    log::info!("Setting up the main menu screen");

    commands
        .spawn((
            MainMenuScreen,
            Node {
                width: Val::Percent(100.0),
                height: Val::Percent(100.0),
                align_items: AlignItems::Center,
                justify_content: JustifyContent::Center,
                flex_direction: FlexDirection::Column,
                ..default()
            },
            BackgroundColor(theme.bg_dark),
        ))
        .with_children(|parent| {
            parent.ui_title("Projects", &theme);

            if registry.projects.is_empty() {
                parent.spawn((
                    Text::new("No projects found in `projects/`. Create a project.toml to get started."),
                    TextFont {
                        font_size: theme.size_body,
                        ..default()
                    },
                    TextColor(theme.text_dim),
                    Node {
                        margin: UiRect::bottom(Val::Px(theme.pad_lg)),
                        ..default()
                    },
                ));
            } else {
                for project in &registry.projects {
                    parent.ui_button(
                        &project.name,
                        MainMenuButton::SelectProject(project.id.clone()),
                        ButtonVariant::Primary,
                        &theme,
                    );
                }
            }

            parent.ui_button(
                "New Project",
                MainMenuButton::NewProject,
                ButtonVariant::Primary,
                &theme,
            );
        });
}

pub fn handle_main_menu_button_press(
    mut commands: Commands,
    mut interaction_query: Query<
        (&Interaction, &MainMenuButton),
        (Changed<Interaction>, With<Button>),
    >,
    registry: Res<ProjectRegistry>,
    mut next_app_state: ResMut<NextState<AppState>>,
    mut next_menu_screen: ResMut<NextState<MenuScreen>>,
) {
    for (interaction, action) in interaction_query.iter_mut() {
        if *interaction != Interaction::Pressed {
            continue;
        }
        match action {
            MainMenuButton::SelectProject(id) => {
                let Some(manifest) = registry.find(id) else {
                    log::warn!("Selected project '{id}' not found in registry");
                    continue;
                };
                log::info!(
                    "Starting project '{}' (start_world='{}', plugins={:?})",
                    manifest.id,
                    manifest.start_world,
                    manifest.plugins
                );
                commands.insert_resource(ActiveProject(manifest.clone()));
                next_app_state.set(AppState::InGame);
            }
            MainMenuButton::NewProject => {
                log::info!("Opening new-project screen");
                next_menu_screen.set(MenuScreen::CreateProject);
            }
        }
    }
}
