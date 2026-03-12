use bevy::{
    prelude::*,
    window::{CursorGrabMode, CursorOptions},
};

use super::SimulationState;
use crate::{
    AppState,
    settings::{
        EdgeDepthSensitivitySetting, EdgeNormalSensitivitySetting, MouseSensitivitySetting,
        MovementSpeedSetting, SliderSetting, UserSettings, slider_constants,
    },
    ui::{ButtonVariant, UiExt, UiTheme},
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
                PauseMenuState::Settings => {
                    menu_next_state.set(PauseMenuState::Main);
                }
                PauseMenuState::Main => {
                    sim_next_state.set(SimulationState::Running);
                }
            },
        }
    }
}

pub fn setup_pause_menu(mut commands: Commands, theme: Res<UiTheme>) {
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

pub fn handle_pause_menu(
    interaction_query: Query<
        (&Interaction, &PauseMenuButton),
        (Changed<Interaction>, With<Button>),
    >,
    mut simulation_state: ResMut<NextState<SimulationState>>,
    mut pause_menu_state: ResMut<NextState<PauseMenuState>>,
    mut app_state: ResMut<NextState<AppState>>,
    mut cursor_options: Single<&mut CursorOptions>,
) {
    for (interaction, action) in interaction_query.iter() {
        if *interaction == Interaction::Pressed {
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
