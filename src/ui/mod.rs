mod anchors;
mod checkbox;
mod text_input;
mod theme;
mod widgets;

pub use anchors::{UiAnchors, setup_ui_anchors};
pub use checkbox::{Checkbox, CheckboxPlugin, spawn_checkbox};
pub use text_input::{
    TextInput, TextInputPlugin, clear_focus, spawn_text_input,
};
pub use theme::UiTheme;
pub use widgets::{ButtonVariant, UiExt, update_interactive_visuals};

use bevy::prelude::*;

pub struct UiPlugin;

impl Plugin for UiPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(UiTheme::default());
        app.add_systems(Startup, setup_ui_anchors);
        app.add_systems(Update, update_interactive_visuals);
        app.add_plugins((TextInputPlugin, CheckboxPlugin));
    }
}
