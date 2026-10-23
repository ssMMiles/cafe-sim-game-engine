use bevy::{log, prelude::*};

use crate::{
    projects::{self, ProjectRegistry},
    ui::{
        ButtonVariant, Checkbox, TextInput, UiExt, UiTheme, spawn_checkbox, spawn_dropdown,
        spawn_text_input,
    },
};

use super::MenuScreen;

#[derive(Component, Debug)]
pub struct CreateProjectScreen;

#[derive(Component, Debug)]
pub struct IdInput;

#[derive(Component, Debug)]
pub struct NameInput;

#[derive(Component, Debug)]
pub struct WorldInput;

#[derive(Component, Debug)]
pub struct PluginCheckbox(pub String);

#[derive(Component, Debug)]
pub struct PluginsDropdown;

#[derive(Component, Debug)]
pub enum CreateAction {
    Create,
    Cancel,
}

#[derive(Component, Debug)]
pub struct StatusLine;

pub fn setup(mut commands: Commands, theme: Res<UiTheme>) {
    log::info!("Setting up create-project screen");

    let plugin_ids = projects::discover_plugin_ids();

    commands
        .spawn((
            CreateProjectScreen,
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
        .with_children(|root| {
            root.ui_title("New Project", &theme);

            spawn_text_input(root, "Project ID", "", "my-game", IdInput, &theme);
            spawn_text_input(root, "Display Name", "", "My Game", NameInput, &theme);
            spawn_text_input(root, "Start World", "", "default", WorldInput, &theme);

            spawn_dropdown(root, "Plugins", false, PluginsDropdown, &theme, |body| {
                if plugin_ids.is_empty() {
                    body.spawn((
                        Text::new("(no plugins discovered under `plugins/`)"),
                        TextFont {
                            font_size: theme.size_caption,
                            ..default()
                        },
                        TextColor(theme.text_dim),
                    ));
                } else {
                    for id in plugin_ids {
                        let label = id.clone();
                        spawn_checkbox(body, &label, false, PluginCheckbox(id), &theme);
                    }
                }
            });

            root.spawn((Node {
                flex_direction: FlexDirection::Row,
                margin: UiRect::top(Val::Px(theme.pad_lg)),
                ..default()
            },))
                .with_children(|row| {
                    row.ui_button(
                        "Create",
                        CreateAction::Create,
                        ButtonVariant::Primary,
                        &theme,
                    );
                    row.ui_button(
                        "Cancel",
                        CreateAction::Cancel,
                        ButtonVariant::Danger,
                        &theme,
                    );
                });

            root.spawn((
                Text::new(""),
                TextFont {
                    font_size: theme.size_caption,
                    ..default()
                },
                TextColor(theme.danger),
                Node {
                    margin: UiRect::top(Val::Px(theme.pad_md)),
                    ..default()
                },
                StatusLine,
            ));
        });
}

pub fn handle_actions(
    interactions: Query<(&Interaction, &CreateAction), (Changed<Interaction>, With<Button>)>,
    id_q: Query<&TextInput, With<IdInput>>,
    name_q: Query<&TextInput, With<NameInput>>,
    world_q: Query<&TextInput, With<WorldInput>>,
    plugin_q: Query<(&Checkbox, &PluginCheckbox)>,
    mut registry: ResMut<ProjectRegistry>,
    mut next_screen: ResMut<NextState<MenuScreen>>,
    mut status_q: Query<(&mut Text, &mut TextColor), With<StatusLine>>,
    theme: Res<UiTheme>,
) {
    for (interaction, action) in &interactions {
        if *interaction != Interaction::Pressed {
            continue;
        }
        match action {
            CreateAction::Cancel => {
                next_screen.set(MenuScreen::Main);
                return;
            }
            CreateAction::Create => {
                let id = id_q
                    .single()
                    .map(|t| t.value.trim().to_owned())
                    .unwrap_or_default();
                let name = name_q
                    .single()
                    .map(|t| t.value.trim().to_owned())
                    .unwrap_or_default();
                let world = world_q
                    .single()
                    .map(|t| t.value.trim().to_owned())
                    .unwrap_or_default();
                let selected_plugins: Vec<String> = plugin_q
                    .iter()
                    .filter(|(cb, _)| cb.checked)
                    .map(|(_, p)| p.0.clone())
                    .collect();

                if let Err(msg) = validate(&id, &name, &world, &registry) {
                    set_status(&mut status_q, &msg, theme.danger);
                    continue;
                }

                let display_name = if name.is_empty() { id.clone() } else { name };
                let start_world = if world.is_empty() { id.clone() } else { world };

                #[cfg(not(target_arch = "wasm32"))]
                {
                    let manifest = projects::ProjectManifest {
                        id: id.clone(),
                        name: display_name,
                        description: None,
                        plugins: selected_plugins.clone(),
                        worlds: vec![start_world.clone()],
                        start_world: start_world.clone(),
                    };
                    match projects::write_manifest(&manifest) {
                        Ok(()) => {
                            log::info!(
                                "Created project '{id}' (plugins={:?}, start_world='{start_world}')",
                                selected_plugins
                            );
                            registry.projects.push(manifest);
                            registry.projects.sort_by(|a, b| a.id.cmp(&b.id));
                            next_screen.set(MenuScreen::Main);
                            return;
                        }
                        Err(e) => {
                            let msg = format!("Failed to write manifest: {e}");
                            log::error!("{msg}");
                            set_status(&mut status_q, &msg, theme.danger);
                        }
                    }
                }

                #[cfg(target_arch = "wasm32")]
                {
                    let _ = (selected_plugins, display_name, start_world);
                    set_status(
                        &mut status_q,
                        "Creating projects from the browser is not supported.",
                        theme.danger,
                    );
                }
            }
        }
    }
}

fn validate(id: &str, _name: &str, _world: &str, registry: &ProjectRegistry) -> Result<(), String> {
    if id.is_empty() {
        return Err("Project ID is required.".into());
    }
    if !id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err("Project ID may only contain letters, digits, '-', and '_'.".into());
    }
    if registry.find(id).is_some() {
        return Err(format!("A project named '{id}' already exists."));
    }
    Ok(())
}

fn set_status(
    status_q: &mut Query<(&mut Text, &mut TextColor), With<StatusLine>>,
    msg: &str,
    color: Color,
) {
    if let Ok((mut text, mut text_color)) = status_q.single_mut() {
        *text = Text::new(msg.to_owned());
        text_color.0 = color;
    }
}
