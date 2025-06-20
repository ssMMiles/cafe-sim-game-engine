use bevy::{prelude::*, window::CursorGrabMode};

use super::SimulationState;
use crate::{
    AppState,
    menu::ui_builder::{ButtonStyle, MenuCommandsExt, MenuContainerExt, UIColors},
    settings::{
        MouseSensitivitySetting, MovementSpeedSetting, SliderSetting, UserSettings,
        slider_constants,
    },
};

#[derive(Default, Clone, Copy, PartialEq, Eq, Hash, Debug, States)]
pub enum PauseMenuState {
    #[default]
    Main,
    Settings,
}

#[derive(Component)]
pub struct PauseMenu;

#[derive(Component)]
pub struct PauseSettings;

#[derive(Component)]
pub enum PauseMenuButton {
    Resume,
    Settings,
    ExitGame,
}

#[derive(Component)]
pub enum SettingsButton {
    Back,
}

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
    current_sim_state: &Res<State<SimulationState>>,
) {
    if let Ok(mut window) = windows.single_mut() {
        match current_sim_state.get() {
            SimulationState::Running => {
                window.cursor_options.grab_mode = CursorGrabMode::None;
                window.cursor_options.visible = true;
            }
            SimulationState::Paused => {
                let height = window.height();
                let width = window.width();

                window.set_cursor_position(Some(Vec2::new(width / 2.0, height / 2.0)));

                window.cursor_options.grab_mode = CursorGrabMode::Locked;
                window.cursor_options.visible = false;
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
    mut windows: Query<&mut Window>,
) {
    if keyboard_input.just_pressed(KeyCode::Escape) {
        handle_cursor_lock(windows, &current_sim_state);

        match current_sim_state.get() {
            SimulationState::Running => {
                sim_next_state.set(SimulationState::Paused);
            }
            SimulationState::Paused => {
                match current_menu_state.get() {
                    PauseMenuState::Settings => {
                        // If in settings, go back to main pause menu
                        menu_next_state.set(PauseMenuState::Main);
                    }
                    PauseMenuState::Main => {
                        // If in main pause menu, resume game
                        sim_next_state.set(SimulationState::Running);
                    }
                }
            }
        }
    }
}

pub fn setup_pause_menu(mut commands: Commands) {
    commands
        .spawn_menu_container(PauseMenu, UIColors::BACKGROUND_OVERLAY)
        .with_children(|parent| {
            parent.spawn_title("PAUSED");
            parent.spawn_button("Resume", PauseMenuButton::Resume, ButtonStyle::Primary);
            parent.spawn_button("Settings", PauseMenuButton::Settings, ButtonStyle::Primary);
            parent.spawn_button("Exit Game", PauseMenuButton::ExitGame, ButtonStyle::Danger);
        });
}

pub fn setup_pause_settings(mut commands: Commands, settings: Res<UserSettings>) {
    commands
        .spawn_menu_container(PauseSettings, UIColors::BACKGROUND_OVERLAY)
        .with_children(|parent| {
            parent.spawn_title("SETTINGS");
            parent.spawn_slider(
                MouseSensitivitySetting::get_label(),
                MouseSensitivitySetting::get_value(&settings),
                MouseSensitivitySetting::get_range().0,
                MouseSensitivitySetting::get_range().1,
                SliderHandle::<MouseSensitivitySetting>::default(),
                SliderLabel::<MouseSensitivitySetting>::default(),
                SliderTrack::<MouseSensitivitySetting>::default(),
            );
            parent.spawn_slider(
                MovementSpeedSetting::get_label(),
                MovementSpeedSetting::get_value(&settings),
                MovementSpeedSetting::get_range().0,
                MovementSpeedSetting::get_range().1,
                SliderHandle::<MovementSpeedSetting>::default(),
                SliderLabel::<MovementSpeedSetting>::default(),
                SliderTrack::<MovementSpeedSetting>::default(),
            );
            parent.spawn_button("Back", SettingsButton::Back, ButtonStyle::Primary);
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

pub fn handle_pause_menu(
    interaction_query: Query<
        (&Interaction, &PauseMenuButton),
        (Changed<Interaction>, With<Button>),
    >,
    mut simulation_state: ResMut<NextState<SimulationState>>,
    mut pause_menu_state: ResMut<NextState<PauseMenuState>>,
    mut app_state: ResMut<NextState<AppState>>,
    mut windows: Query<&mut Window>,
) {
    for (interaction, action) in interaction_query.iter() {
        if *interaction == Interaction::Pressed {
            match action {
                PauseMenuButton::Resume => {
                    simulation_state.set(SimulationState::Running);
                    // Lock cursor when resuming
                    if let Ok(mut window) = windows.single_mut() {
                        window.cursor_options.grab_mode = CursorGrabMode::Locked;
                        window.cursor_options.visible = false;
                    }
                }
                PauseMenuButton::Settings => {
                    pause_menu_state.set(PauseMenuState::Settings);
                }
                PauseMenuButton::ExitGame => {
                    app_state.set(AppState::MainMenu);
                    simulation_state.set(SimulationState::Running);
                    // Unlock cursor when exiting to main menu
                    if let Ok(mut window) = windows.single_mut() {
                        window.cursor_options.grab_mode = CursorGrabMode::None;
                        window.cursor_options.visible = true;
                    }
                }
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

pub fn handle_slider<T: Component + SliderSetting>(
    mut handle_query: Query<(&Interaction, &mut Node), With<SliderHandle<T>>>,
    track_query: Query<&Interaction, (With<SliderTrack<T>>, Without<SliderHandle<T>>)>,
    mut settings: ResMut<UserSettings>,
    mouse_input: Res<ButtonInput<MouseButton>>,
    windows: Query<&Window>,
) {
    let (min_value, max_value) = T::get_range();

    // Check if we're currently dragging the handle
    let is_dragging = handle_query.iter().any(|(interaction, _)| {
        (*interaction == Interaction::Pressed || *interaction == Interaction::Hovered)
            && mouse_input.pressed(MouseButton::Left)
    });

    // Handle track clicks (immediate jump to position)
    for interaction in track_query.iter() {
        if *interaction == Interaction::Pressed {
            if let Ok(window) = windows.single() {
                if let Some(cursor_pos) = window.cursor_position() {
                    let normalized_value = calculate_normalized_position(cursor_pos, window);

                    // Update handle position
                    if let Ok((_, mut handle_node)) = handle_query.single_mut() {
                        handle_node.left =
                            Val::Percent(calculate_handle_position(normalized_value));
                    }

                    // Update setting value
                    let new_value = min_value + normalized_value * (max_value - min_value);
                    T::set_value(&mut settings, new_value);
                }
            }
        }
    }

    // Handle dragging (continuous movement while mouse is held down)
    if is_dragging {
        if let Ok(window) = windows.single() {
            if let Some(cursor_pos) = window.cursor_position() {
                let normalized_value = calculate_normalized_position(cursor_pos, window);

                // Update handle position
                if let Ok((_, mut handle_node)) = handle_query.single_mut() {
                    handle_node.left = Val::Percent(calculate_handle_position(normalized_value));
                }

                // Update setting value
                let new_value = min_value + normalized_value * (max_value - min_value);
                T::set_value(&mut settings, new_value);
            }
        }
    }
}

fn calculate_normalized_position(cursor_pos: Vec2, window: &Window) -> f32 {
    // For sliders, we'll use a simplified relative positioning
    // Map cursor to a normalized value between 0 and 1
    let window_width = window.resolution.width();
    let window_center_x = window_width / 2.0;

    // Calculate relative position from window center
    let relative_x = cursor_pos.x - window_center_x;

    // Map to slider range (using constants)
    let slider_half_width = slider_constants::TRACK_WIDTH / 2.0;
    let clamped_x = relative_x.clamp(-slider_half_width, slider_half_width);
    (clamped_x + slider_half_width) / (slider_constants::TRACK_WIDTH)
}

fn calculate_handle_position(normalized_value: f32) -> f32 {
    // Calculate position to keep handle center within track bounds
    let usable_width = slider_constants::TRACK_WIDTH - slider_constants::HANDLE_WIDTH;

    // Position handle so its center stays within track bounds
    let handle_left_position = normalized_value * usable_width;

    // Convert to percentage
    (handle_left_position / slider_constants::TRACK_WIDTH) * 100.0
}

pub fn update_slider_label<T: Component + SliderSetting>(
    settings: Res<UserSettings>,
    mut label_query: Query<&mut Text, With<SliderLabel<T>>>,
) {
    if settings.is_changed() {
        for mut text in label_query.iter_mut() {
            let current_value = T::get_value(&settings);
            let format_str = T::get_format();
            let formatted_value = if format_str == "{:.3}" {
                format!("{:.3}", current_value)
            } else if format_str == "{:.2}" {
                format!("{:.2}", current_value)
            } else {
                format!("{}", current_value)
            };
            **text = format!("{}: {}", T::get_label(), formatted_value);
        }
    }
}
