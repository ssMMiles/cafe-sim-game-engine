use bevy::{log, prelude::*, ui::Pressed};

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
    build_main_menu(&mut commands, &theme, &registry);
}

/// Rebuild the screen when the project list changes underneath it.
///
/// Project discovery is asynchronous on wasm (an OPFS scan), so the registry
/// routinely gains its real contents several frames *after* the menu was
/// built from the seeded fallback. Without this the browser would show a
/// stale list until you navigated away and back.
///
/// Skipped when nothing has been spawned yet: on the frame `setup_main_menu`
/// runs its spawn is still a queued command, so the query is empty and a
/// rebuild here would duplicate the whole screen. The menu is built from the
/// same registry value in that case anyway.
pub fn rebuild_main_menu_on_registry_change(
    mut commands: Commands,
    theme: Res<UiTheme>,
    registry: Res<ProjectRegistry>,
    existing: Query<Entity, With<MainMenuScreen>>,
) {
    if !registry.is_changed() || existing.is_empty() {
        return;
    }
    log::info!("Project list changed; rebuilding the main menu");
    for entity in &existing {
        commands.entity(entity).despawn();
    }
    build_main_menu(&mut commands, &theme, &registry);
}

fn build_main_menu(commands: &mut Commands, theme: &UiTheme, registry: &ProjectRegistry) {
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
            parent.ui_title("Projects", theme);

            if registry.projects.is_empty() {
                parent.spawn((
                    Text::new(
                        "No projects found in `projects/`. Create a project.toml to get started.",
                    ),
                    TextFont {
                        font_size: FontSize::Px(theme.size_body),
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
                        theme,
                    );
                }
            }

            parent.ui_button(
                "New Project",
                MainMenuButton::NewProject,
                ButtonVariant::Primary,
                theme,
            );
        });
}

pub fn handle_main_menu_button_press(
    mut commands: Commands,
    pressed: Query<&MainMenuButton, Added<Pressed>>,
    registry: Res<ProjectRegistry>,
    mut next_app_state: ResMut<NextState<AppState>>,
    mut next_menu_screen: ResMut<NextState<MenuScreen>>,
) {
    for action in &pressed {
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
