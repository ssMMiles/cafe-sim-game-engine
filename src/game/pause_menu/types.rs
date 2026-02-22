use bevy::prelude::*;

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
