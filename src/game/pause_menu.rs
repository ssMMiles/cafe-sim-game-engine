use bevy::{
    log,
    prelude::*,
    window::{CursorGrabMode, CursorOptions},
};

use super::SimulationState;
use crate::{
    AppState,
    game::world::{
        self,
        objects::{PlacedObject, PlacedObjectRegistry},
        streaming::StreamingState,
        terrain::{
            TerrainChunk,
            bootstrap::TerrainSetupMarker,
            storage::WorldConfig,
        },
    },
    projects::{self, ActiveProject, ProjectRegistry},
    settings::{
        EdgeDepthSensitivitySetting, EdgeNormalSensitivitySetting, MouseSensitivitySetting,
        MovementSpeedSetting, SliderSetting, UserSettings, slider_constants,
    },
    ui::{ButtonVariant, TextInput, UiExt, UiTheme, spawn_dropdown, spawn_text_input},
};
use engine_api::Player;

#[derive(Default, Clone, Copy, PartialEq, Eq, Hash, Debug, States)]
pub enum PauseMenuState {
    #[default]
    Main,
    Settings,
    AddWorld,
    AddPlugin,
}

#[derive(Component)]
pub struct PauseMenu;

#[derive(Component)]
pub struct PauseSettings;

#[derive(Component)]
pub struct PauseAddWorld;

#[derive(Component)]
pub struct PauseAddPlugin;

#[derive(Component)]
pub enum PauseMenuButton {
    Resume,
    Settings,
    ExitGame,
    SwitchWorld(String),
    AddWorld,
    RemovePlugin(String),
    OpenAddPlugin,
}

#[derive(Component, Debug)]
pub enum AddPluginButton {
    Add(String),
    Cancel,
}

#[derive(Component)]
pub enum SettingsButton {
    Back,
}

#[derive(Component, Debug)]
pub struct AddWorldIdInput;

#[derive(Component, Debug)]
pub enum AddWorldButton {
    Create,
    Cancel,
}

#[derive(Component, Debug)]
pub struct AddWorldStatusLine;

#[derive(Component, Debug)]
pub struct WorldsDropdown;

#[derive(Component, Debug)]
pub struct PluginsDropdown;

// Generic slider components
#[derive(Component)]
pub struct SliderHandle<T: Component>(pub std::marker::PhantomData<T>);

#[derive(Component)]
pub struct SliderLabel<T: Component>(pub std::marker::PhantomData<T>);

#[derive(Component)]
pub struct SliderTrack<T: Component>(pub std::marker::PhantomData<T>);

impl<T: Component> Default for SliderHandle<T> {
    fn default() -> Self {
        Self(std::marker::PhantomData)
    }
}

impl<T: Component> Default for SliderLabel<T> {
    fn default() -> Self {
        Self(std::marker::PhantomData)
    }
}

impl<T: Component> Default for SliderTrack<T> {
    fn default() -> Self {
        Self(std::marker::PhantomData)
    }
}

pub fn handle_cursor_lock(
    mut windows: Query<&mut Window>,
    mut cursor_options: Single<&mut CursorOptions>,
    current_sim_state: &Res<State<SimulationState>>,
) {
    if let Ok(mut window) = windows.single_mut() {
        match current_sim_state.get() {
            SimulationState::Running => {
                cursor_options.grab_mode = CursorGrabMode::None;
                cursor_options.visible = true;
            }
            SimulationState::Paused => {
                let height = window.height();
                let width = window.width();

                window.set_cursor_position(Some(Vec2::new(width / 2.0, height / 2.0)));

                cursor_options.grab_mode = CursorGrabMode::Locked;
                cursor_options.visible = false;
            }
        }
    }
}

pub fn handle_pause_input(
    keyboard_input: Res<ButtonInput<KeyCode>>,
    current_sim_state: Res<State<SimulationState>>,
    current_menu_state: Res<State<PauseMenuState>>,
    mut sim_next_state: ResMut<NextState<SimulationState>>,
    mut menu_next_state: ResMut<NextState<PauseMenuState>>,
    windows: Query<&mut Window>,
    cursor_options: Single<&mut CursorOptions>,
) {
    if keyboard_input.just_pressed(KeyCode::Escape) {
        handle_cursor_lock(windows, cursor_options, &current_sim_state);

        match current_sim_state.get() {
            SimulationState::Running => {
                sim_next_state.set(SimulationState::Paused);
            }
            SimulationState::Paused => match current_menu_state.get() {
                PauseMenuState::Settings
                | PauseMenuState::AddWorld
                | PauseMenuState::AddPlugin => {
                    menu_next_state.set(PauseMenuState::Main);
                }
                PauseMenuState::Main => {
                    sim_next_state.set(SimulationState::Running);
                }
            },
        }
    }
}

/// Editor pause menu — includes worlds/plugins management. Used while in
/// `EditorMode::Editor`. The slimmer gameplay variant is [`setup_game_pause_menu`].
pub fn setup_editor_pause_menu(
    mut commands: Commands,
    theme: Res<UiTheme>,
    active: Option<Res<ActiveProject>>,
    world_config: Option<Res<WorldConfig>>,
) {
    commands
        .spawn((
            PauseMenu,
            Node {
                width: Val::Percent(100.0),
                height: Val::Percent(100.0),
                align_items: AlignItems::Center,
                justify_content: JustifyContent::Center,
                flex_direction: FlexDirection::Column,
                ..default()
            },
            BackgroundColor(theme.bg_overlay),
        ))
        .with_children(|parent| {
            parent.ui_title("PAUSED", &theme);
            parent.ui_button(
                "Resume",
                PauseMenuButton::Resume,
                ButtonVariant::Primary,
                &theme,
            );
            parent.ui_button(
                "Settings",
                PauseMenuButton::Settings,
                ButtonVariant::Primary,
                &theme,
            );

            // Worlds section — only shown when an ActiveProject is set.
            if let Some(active) = active.as_deref() {
                let current_world = world_config
                    .as_deref()
                    .map(|c| c.world_id.as_str())
                    .unwrap_or("");

                let worlds_label = if current_world.is_empty() {
                    "Worlds".to_string()
                } else {
                    format!("Worlds: {current_world}")
                };
                spawn_dropdown(
                    parent,
                    &worlds_label,
                    false,
                    WorldsDropdown,
                    &theme,
                    |body| {
                        for world_id in &active.0.worlds {
                            let is_current = world_id == current_world;
                            let label = if is_current {
                                format!("> {world_id}")
                            } else {
                                world_id.clone()
                            };
                            body.ui_button(
                                &label,
                                PauseMenuButton::SwitchWorld(world_id.clone()),
                                ButtonVariant::Primary,
                                &theme,
                            );
                        }

                        body.ui_button(
                            "+ Add World",
                            PauseMenuButton::AddWorld,
                            ButtonVariant::Primary,
                            &theme,
                        );
                    },
                );

                spawn_dropdown(
                    parent,
                    "Plugins",
                    false,
                    PluginsDropdown,
                    &theme,
                    |body| {
                        if active.0.plugins.is_empty() {
                            body.spawn((
                                Text::new("(none — add one to enable scripted behavior)"),
                                TextFont {
                                    font_size: theme.size_caption,
                                    ..default()
                                },
                                TextColor(theme.text_dim),
                                Node {
                                    margin: UiRect::bottom(Val::Px(theme.pad_sm)),
                                    ..default()
                                },
                            ));
                        } else {
                            for plugin_id in &active.0.plugins {
                                body.ui_button(
                                    &format!("- {plugin_id}"),
                                    PauseMenuButton::RemovePlugin(plugin_id.clone()),
                                    ButtonVariant::Primary,
                                    &theme,
                                );
                            }
                        }

                        body.ui_button(
                            "+ Add Plugin",
                            PauseMenuButton::OpenAddPlugin,
                            ButtonVariant::Primary,
                            &theme,
                        );
                    },
                );
            }

            parent.ui_button(
                "Exit Game",
                PauseMenuButton::ExitGame,
                ButtonVariant::Danger,
                &theme,
            );
        });
}

pub fn setup_pause_settings(
    mut commands: Commands,
    settings: Res<UserSettings>,
    theme: Res<UiTheme>,
) {
    commands
        .spawn((
            PauseSettings,
            Node {
                width: Val::Percent(100.0),
                height: Val::Percent(100.0),
                align_items: AlignItems::Center,
                justify_content: JustifyContent::Center,
                flex_direction: FlexDirection::Column,
                ..default()
            },
            BackgroundColor(theme.bg_overlay),
        ))
        .with_children(|parent| {
            parent.ui_title("SETTINGS", &theme);
            parent.ui_slider(
                MouseSensitivitySetting::get_label(),
                MouseSensitivitySetting::get_value(&settings),
                MouseSensitivitySetting::get_range().0,
                MouseSensitivitySetting::get_range().1,
                SliderHandle::<MouseSensitivitySetting>::default(),
                SliderLabel::<MouseSensitivitySetting>::default(),
                SliderTrack::<MouseSensitivitySetting>::default(),
                &theme,
            );
            parent.ui_slider(
                MovementSpeedSetting::get_label(),
                MovementSpeedSetting::get_value(&settings),
                MovementSpeedSetting::get_range().0,
                MovementSpeedSetting::get_range().1,
                SliderHandle::<MovementSpeedSetting>::default(),
                SliderLabel::<MovementSpeedSetting>::default(),
                SliderTrack::<MovementSpeedSetting>::default(),
                &theme,
            );
            parent.ui_slider(
                EdgeDepthSensitivitySetting::get_label(),
                EdgeDepthSensitivitySetting::get_value(&settings),
                EdgeDepthSensitivitySetting::get_range().0,
                EdgeDepthSensitivitySetting::get_range().1,
                SliderHandle::<EdgeDepthSensitivitySetting>::default(),
                SliderLabel::<EdgeDepthSensitivitySetting>::default(),
                SliderTrack::<EdgeDepthSensitivitySetting>::default(),
                &theme,
            );
            parent.ui_slider(
                EdgeNormalSensitivitySetting::get_label(),
                EdgeNormalSensitivitySetting::get_value(&settings),
                EdgeNormalSensitivitySetting::get_range().0,
                EdgeNormalSensitivitySetting::get_range().1,
                SliderHandle::<EdgeNormalSensitivitySetting>::default(),
                SliderLabel::<EdgeNormalSensitivitySetting>::default(),
                SliderTrack::<EdgeNormalSensitivitySetting>::default(),
                &theme,
            );
            parent.ui_button("Back", SettingsButton::Back, ButtonVariant::Primary, &theme);
        });
}

pub fn cleanup_pause_menu(mut commands: Commands, query: Query<Entity, With<PauseMenu>>) {
    for entity in query.iter() {
        commands.entity(entity).despawn();
    }
}

pub fn cleanup_pause_settings(mut commands: Commands, query: Query<Entity, With<PauseSettings>>) {
    for entity in query.iter() {
        commands.entity(entity).despawn();
    }
}

pub fn setup_pause_add_world(mut commands: Commands, theme: Res<UiTheme>) {
    commands
        .spawn((
            PauseAddWorld,
            Node {
                width: Val::Percent(100.0),
                height: Val::Percent(100.0),
                align_items: AlignItems::Center,
                justify_content: JustifyContent::Center,
                flex_direction: FlexDirection::Column,
                ..default()
            },
            BackgroundColor(theme.bg_overlay),
        ))
        .with_children(|parent| {
            parent.ui_title("ADD WORLD", &theme);
            spawn_text_input(
                parent,
                "World ID",
                "",
                "my-world",
                AddWorldIdInput,
                &theme,
            );

            parent
                .spawn((Node {
                    flex_direction: FlexDirection::Row,
                    margin: UiRect::top(Val::Px(theme.pad_lg)),
                    ..default()
                },))
                .with_children(|row| {
                    row.ui_button(
                        "Create",
                        AddWorldButton::Create,
                        ButtonVariant::Primary,
                        &theme,
                    );
                    row.ui_button(
                        "Cancel",
                        AddWorldButton::Cancel,
                        ButtonVariant::Danger,
                        &theme,
                    );
                });

            parent.spawn((
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
                AddWorldStatusLine,
            ));
        });
}

pub fn cleanup_pause_add_world(mut commands: Commands, query: Query<Entity, With<PauseAddWorld>>) {
    for entity in query.iter() {
        commands.entity(entity).despawn();
    }
}

pub fn setup_pause_add_plugin(
    mut commands: Commands,
    theme: Res<UiTheme>,
    active: Option<Res<ActiveProject>>,
) {
    let on_disk = projects::discover_plugin_ids();
    let already: Vec<&str> = active
        .as_deref()
        .map(|a| a.0.plugins.iter().map(|s| s.as_str()).collect())
        .unwrap_or_default();
    let candidates: Vec<String> = on_disk
        .into_iter()
        .filter(|id| !already.contains(&id.as_str()))
        .collect();

    commands
        .spawn((
            PauseAddPlugin,
            Node {
                width: Val::Percent(100.0),
                height: Val::Percent(100.0),
                align_items: AlignItems::Center,
                justify_content: JustifyContent::Center,
                flex_direction: FlexDirection::Column,
                ..default()
            },
            BackgroundColor(theme.bg_overlay),
        ))
        .with_children(|parent| {
            parent.ui_title("ADD PLUGIN", &theme);

            if candidates.is_empty() {
                parent.spawn((
                    Text::new(
                        "All discoverable plugins are already in this project. Build a new \
                         crate under `plugins/<id>/` to add more.",
                    ),
                    TextFont {
                        font_size: theme.size_body,
                        ..default()
                    },
                    TextColor(theme.text_dim),
                    Node {
                        max_width: Val::Px(theme.btn_width * 2.0),
                        margin: UiRect::bottom(Val::Px(theme.pad_lg)),
                        ..default()
                    },
                ));
            } else {
                for id in candidates {
                    parent.ui_button(
                        &id,
                        AddPluginButton::Add(id.clone()),
                        ButtonVariant::Primary,
                        &theme,
                    );
                }
            }

            parent.ui_button(
                "Cancel",
                AddPluginButton::Cancel,
                ButtonVariant::Danger,
                &theme,
            );
        });
}

pub fn cleanup_pause_add_plugin(
    mut commands: Commands,
    query: Query<Entity, With<PauseAddPlugin>>,
) {
    for entity in query.iter() {
        commands.entity(entity).despawn();
    }
}

pub fn handle_pause_menu(
    mut commands: Commands,
    interaction_query: Query<
        (&Interaction, &PauseMenuButton),
        (Changed<Interaction>, With<Button>),
    >,
    mut simulation_state: ResMut<NextState<SimulationState>>,
    mut pause_menu_state: ResMut<NextState<PauseMenuState>>,
    mut app_state: ResMut<NextState<AppState>>,
    mut cursor_options: Single<&mut CursorOptions>,
    mut world_config: ResMut<WorldConfig>,
    chunk_query: Query<Entity, With<TerrainChunk>>,
    marker_query: Query<Entity, With<TerrainSetupMarker>>,
    object_query: Query<Entity, With<PlacedObject>>,
    mut placed_registry: ResMut<PlacedObjectRegistry>,
    mut streaming_state: ResMut<StreamingState>,
    mut player: Query<&mut Transform, With<Player>>,
    mut active: Option<ResMut<ActiveProject>>,
    mut registry: ResMut<ProjectRegistry>,
) {
    for (interaction, action) in interaction_query.iter() {
        if *interaction != Interaction::Pressed {
            continue;
        }
        match action {
            PauseMenuButton::Resume => {
                simulation_state.set(SimulationState::Running);
                cursor_options.grab_mode = CursorGrabMode::Locked;
                cursor_options.visible = false;
            }
            PauseMenuButton::Settings => {
                pause_menu_state.set(PauseMenuState::Settings);
            }
            PauseMenuButton::ExitGame => {
                app_state.set(AppState::MainMenu);
                simulation_state.set(SimulationState::Running);
                cursor_options.grab_mode = CursorGrabMode::None;
                cursor_options.visible = true;
            }
            PauseMenuButton::SwitchWorld(target) => {
                if world_config.world_id == *target {
                    // Already in this world — just resume.
                    simulation_state.set(SimulationState::Running);
                    cursor_options.grab_mode = CursorGrabMode::Locked;
                    cursor_options.visible = false;
                    continue;
                }
                let Ok(mut player_transform) = player.single_mut() else {
                    log::warn!("SwitchWorld: no player entity found");
                    continue;
                };
                world::switch_world(
                    target,
                    &mut commands,
                    &mut world_config,
                    &chunk_query,
                    &marker_query,
                    &object_query,
                    &mut placed_registry,
                    &mut streaming_state,
                    &mut player_transform,
                );
                simulation_state.set(SimulationState::Running);
                cursor_options.grab_mode = CursorGrabMode::Locked;
                cursor_options.visible = false;
            }
            PauseMenuButton::AddWorld => {
                pause_menu_state.set(PauseMenuState::AddWorld);
            }
            PauseMenuButton::OpenAddPlugin => {
                pause_menu_state.set(PauseMenuState::AddPlugin);
            }
            PauseMenuButton::RemovePlugin(plugin_id) => {
                let Some(active) = active.as_deref_mut() else {
                    log::warn!("RemovePlugin: no ActiveProject");
                    continue;
                };
                if !active.0.plugins.iter().any(|p| p == plugin_id) {
                    continue;
                }

                active.0.plugins.retain(|p| p != plugin_id);

                #[cfg(not(target_arch = "wasm32"))]
                {
                    if let Err(e) = projects::write_manifest(&active.0) {
                        log::error!("Failed to update manifest: {e}");
                        // Roll back the in-memory mutation.
                        active.0.plugins.push(plugin_id.clone());
                        active.0.plugins.sort();
                        continue;
                    }
                }

                if let Some(slot) = registry
                    .projects
                    .iter_mut()
                    .find(|m| m.id == active.0.id)
                {
                    slot.plugins = active.0.plugins.clone();
                }

                let id_for_unload = plugin_id.clone();
                commands.queue(move |world: &mut World| {
                    crate::script_host::unload_plugin(world, &id_for_unload);
                    // Rebuild the pause menu so the removed entry disappears.
                    let entities: Vec<Entity> = world
                        .query_filtered::<Entity, With<PauseMenu>>()
                        .iter(world)
                        .collect();
                    for e in entities {
                        if let Ok(em) = world.get_entity_mut(e) {
                            em.despawn();
                        }
                    }
                    let _ = world.try_run_schedule(OnEnter(PauseMenuState::Main));
                });
                log::info!("Removed plugin '{plugin_id}' from project '{}'", active.0.id);
            }
        }
    }
}

pub fn handle_settings_menu(
    interaction_query: Query<(&Interaction, &SettingsButton), (Changed<Interaction>, With<Button>)>,
    mut pause_menu_state: ResMut<NextState<PauseMenuState>>,
) {
    for (interaction, action) in interaction_query.iter() {
        if *interaction == Interaction::Pressed {
            match action {
                SettingsButton::Back => {
                    pause_menu_state.set(PauseMenuState::Main);
                }
            }
        }
    }
}

pub fn handle_add_world(
    interactions: Query<(&Interaction, &AddWorldButton), (Changed<Interaction>, With<Button>)>,
    id_q: Query<&TextInput, With<AddWorldIdInput>>,
    mut active: ResMut<ActiveProject>,
    mut registry: ResMut<ProjectRegistry>,
    mut pause_menu_state: ResMut<NextState<PauseMenuState>>,
    mut status_q: Query<(&mut Text, &mut TextColor), With<AddWorldStatusLine>>,
    theme: Res<UiTheme>,
) {
    for (interaction, action) in interactions.iter() {
        if *interaction != Interaction::Pressed {
            continue;
        }
        match action {
            AddWorldButton::Cancel => {
                pause_menu_state.set(PauseMenuState::Main);
                return;
            }
            AddWorldButton::Create => {
                let world_id = id_q
                    .single()
                    .map(|t| t.value.trim().to_owned())
                    .unwrap_or_default();

                if let Err(msg) = validate_world_id(&world_id, &active.0.worlds) {
                    set_status(&mut status_q, &msg, theme.danger);
                    continue;
                }

                active.0.worlds.push(world_id.clone());

                #[cfg(not(target_arch = "wasm32"))]
                {
                    if let Err(e) = projects::write_manifest(&active.0) {
                        let msg = format!("Failed to update manifest: {e}");
                        log::error!("{msg}");
                        // Roll back the in-memory mutation so we stay consistent.
                        active.0.worlds.retain(|w| w != &world_id);
                        set_status(&mut status_q, &msg, theme.danger);
                        continue;
                    }
                }

                // Mirror the change into ProjectRegistry so the main menu also
                // sees the new world if the user heads back.
                if let Some(slot) = registry
                    .projects
                    .iter_mut()
                    .find(|m| m.id == active.0.id)
                {
                    slot.worlds = active.0.worlds.clone();
                }

                log::info!("Added world '{world_id}' to project '{}'", active.0.id);
                pause_menu_state.set(PauseMenuState::Main);
                return;
            }
        }
    }
}

pub fn handle_add_plugin(
    mut commands: Commands,
    interactions: Query<(&Interaction, &AddPluginButton), (Changed<Interaction>, With<Button>)>,
    mut active: ResMut<ActiveProject>,
    mut registry: ResMut<ProjectRegistry>,
    mut pause_menu_state: ResMut<NextState<PauseMenuState>>,
) {
    for (interaction, action) in interactions.iter() {
        if *interaction != Interaction::Pressed {
            continue;
        }
        match action {
            AddPluginButton::Cancel => {
                pause_menu_state.set(PauseMenuState::Main);
                return;
            }
            AddPluginButton::Add(plugin_id) => {
                if active.0.plugins.iter().any(|p| p == plugin_id) {
                    pause_menu_state.set(PauseMenuState::Main);
                    return;
                }
                active.0.plugins.push(plugin_id.clone());
                active.0.plugins.sort();

                #[cfg(not(target_arch = "wasm32"))]
                {
                    if let Err(e) = projects::write_manifest(&active.0) {
                        log::error!("Failed to update manifest: {e}");
                        active.0.plugins.retain(|p| p != plugin_id);
                        continue;
                    }
                }

                if let Some(slot) = registry
                    .projects
                    .iter_mut()
                    .find(|m| m.id == active.0.id)
                {
                    slot.plugins = active.0.plugins.clone();
                }

                let id_for_load = plugin_id.clone();
                commands.queue(move |world: &mut World| {
                    crate::script_host::load_plugin(world, &id_for_load);
                });
                log::info!("Added plugin '{plugin_id}' to project '{}'", active.0.id);

                pause_menu_state.set(PauseMenuState::Main);
                return;
            }
        }
    }
}

fn validate_world_id(id: &str, existing: &[String]) -> Result<(), String> {
    if id.is_empty() {
        return Err("World ID is required.".into());
    }
    if !id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err("World ID may only contain letters, digits, '-', and '_'.".into());
    }
    if existing.iter().any(|w| w == id) {
        return Err(format!("World '{id}' already exists in this project."));
    }
    Ok(())
}

fn set_status(
    status_q: &mut Query<(&mut Text, &mut TextColor), With<AddWorldStatusLine>>,
    msg: &str,
    color: Color,
) {
    if let Ok((mut text, mut color_)) = status_q.single_mut() {
        *text = Text::new(msg.to_owned());
        color_.0 = color;
    }
}

pub fn handle_slider<T: Component + SliderSetting>(
    mut handle_query: Query<(&Interaction, &mut Node), With<SliderHandle<T>>>,
    track_query: Query<&Interaction, (With<SliderTrack<T>>, Without<SliderHandle<T>>)>,
    mut settings: ResMut<UserSettings>,
    mouse_input: Res<ButtonInput<MouseButton>>,
    windows: Query<&Window>,
) {
    let (min_value, max_value) = T::get_range();

    let is_dragging = handle_query.iter().any(|(interaction, _)| {
        (*interaction == Interaction::Pressed || *interaction == Interaction::Hovered)
            && mouse_input.pressed(MouseButton::Left)
    });

    let track_pressed = track_query
        .iter()
        .any(|interaction| *interaction == Interaction::Pressed);

    if !is_dragging && !track_pressed {
        return;
    }

    let Ok(window) = windows.single() else { return };
    let Some(cursor_pos) = window.cursor_position() else {
        return;
    };
    let normalized_value = calculate_normalized_position(cursor_pos, window);

    if let Ok((_, mut handle_node)) = handle_query.single_mut() {
        handle_node.left = Val::Percent(calculate_handle_position(normalized_value));
    }

    let new_value = min_value + normalized_value * (max_value - min_value);
    T::set_value(&mut settings, new_value);
}

fn calculate_normalized_position(cursor_pos: Vec2, window: &Window) -> f32 {
    let window_width = window.resolution.width();
    let window_center_x = window_width / 2.0;
    let relative_x = cursor_pos.x - window_center_x;
    let slider_half_width = slider_constants::TRACK_WIDTH / 2.0;
    let clamped_x = relative_x.clamp(-slider_half_width, slider_half_width);
    (clamped_x + slider_half_width) / slider_constants::TRACK_WIDTH
}

fn calculate_handle_position(normalized_value: f32) -> f32 {
    let usable_width = slider_constants::TRACK_WIDTH - slider_constants::HANDLE_WIDTH;
    let handle_left_position = normalized_value * usable_width;
    (handle_left_position / slider_constants::TRACK_WIDTH) * 100.0
}

pub fn update_slider_label<T: Component + SliderSetting>(
    settings: Res<UserSettings>,
    mut label_query: Query<&mut Text, With<SliderLabel<T>>>,
) {
    if settings.is_changed() {
        for mut text in label_query.iter_mut() {
            let current_value = T::get_value(&settings);
            **text = format!("{}: {}", T::get_label(), T::format_value(current_value));
        }
    }
}
