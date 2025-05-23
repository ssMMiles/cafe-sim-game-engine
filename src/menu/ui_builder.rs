use bevy::prelude::*;

pub struct UIColors;

impl UIColors {
    pub const BACKGROUND_DARK: Color = Color::srgba(0.1, 0.1, 0.8, 1.0);
    pub const BACKGROUND_OVERLAY: Color = Color::srgba(0.0, 0.0, 0.0, 0.8);
    pub const BUTTON_PRIMARY: Color = Color::srgb(0.4, 0.4, 0.2);
    pub const BUTTON_DANGER: Color = Color::srgb(0.6, 0.2, 0.2);
    pub const TEXT_PRIMARY: Color = Color::srgb(0.9, 0.9, 0.9);
    pub const TEXT_WHITE: Color = Color::WHITE;
}

pub struct UISizes;

impl UISizes {
    pub const BUTTON_WIDTH: f32 = 200.0;
    pub const BUTTON_HEIGHT: f32 = 65.0;
    pub const BUTTON_BORDER: f32 = 5.0;
    pub const BUTTON_MARGIN: f32 = 20.0;
    pub const TITLE_SIZE: f32 = 48.0;
    pub const TITLE_MARGIN: f32 = 50.0;
}


pub trait MenuCommandsExt {
    fn spawn_title(&mut self, text: &str) -> Entity;
    fn spawn_button<T: Component>(&mut self, text: &str, action: T, style: ButtonStyle) -> Entity;
}

pub trait MenuContainerExt {
    fn spawn_menu_container<T: Component>(&mut self, component: T, background_color: Color) -> EntityCommands;
}

#[derive(Clone, Copy)]
pub enum ButtonStyle {
    Primary,
    Danger,
}

impl ButtonStyle {
    fn color(&self) -> Color {
        match self {
            ButtonStyle::Primary => UIColors::BUTTON_PRIMARY,
            ButtonStyle::Danger => UIColors::BUTTON_DANGER,
        }
    }
}

impl<'w, 's> MenuCommandsExt for Commands<'w, 's> {
    fn spawn_title(&mut self, text: &str) -> Entity {
        self.spawn((
            Text::new(text),
            TextFont {
                font_size: UISizes::TITLE_SIZE,
                ..Default::default()
            },
            TextColor(UIColors::TEXT_WHITE),
            Node {
                margin: UiRect::bottom(Val::Px(UISizes::TITLE_MARGIN)),
                ..Default::default()
            },
        ))
        .id()
    }

    fn spawn_button<T: Component>(&mut self, text: &str, action: T, style: ButtonStyle) -> Entity {
        self.spawn((
            Button,
            Node {
                width: Val::Px(UISizes::BUTTON_WIDTH),
                height: Val::Px(UISizes::BUTTON_HEIGHT),
                border: UiRect::all(Val::Px(UISizes::BUTTON_BORDER)),
                justify_content: JustifyContent::Center,
                align_items: AlignItems::Center,
                margin: UiRect::bottom(Val::Px(UISizes::BUTTON_MARGIN)),
                ..default()
            },
            BackgroundColor(style.color()),
            action,
        ))
        .with_child((Text::new(text), TextColor(UIColors::TEXT_PRIMARY)))
        .id()
    }
}

impl<'w, 's> MenuContainerExt for Commands<'w, 's> {
    fn spawn_menu_container<T: Component>(&mut self, component: T, background_color: Color) -> EntityCommands {
        let background = Node {
            width: Val::Percent(100.0),
            height: Val::Percent(100.0),
            align_items: AlignItems::Center,
            justify_content: JustifyContent::Center,
            flex_direction: FlexDirection::Column,
            ..Default::default()
        };

        self.spawn((component, background, BackgroundColor(background_color)))
    }
}

impl MenuCommandsExt for ChildBuilder<'_> {
    fn spawn_title(&mut self, text: &str) -> Entity {
        self.spawn((
            Text::new(text),
            TextFont {
                font_size: UISizes::TITLE_SIZE,
                ..Default::default()
            },
            TextColor(UIColors::TEXT_WHITE),
            Node {
                margin: UiRect::bottom(Val::Px(UISizes::TITLE_MARGIN)),
                ..Default::default()
            },
        ))
        .id()
    }

    fn spawn_button<T: Component>(&mut self, text: &str, action: T, style: ButtonStyle) -> Entity {
        self.spawn((
            Button,
            Node {
                width: Val::Px(UISizes::BUTTON_WIDTH),
                height: Val::Px(UISizes::BUTTON_HEIGHT),
                border: UiRect::all(Val::Px(UISizes::BUTTON_BORDER)),
                justify_content: JustifyContent::Center,
                align_items: AlignItems::Center,
                margin: UiRect::bottom(Val::Px(UISizes::BUTTON_MARGIN)),
                ..default()
            },
            BackgroundColor(style.color()),
            action,
        ))
        .with_child((Text::new(text), TextColor(UIColors::TEXT_PRIMARY)))
        .id()
    }
}

