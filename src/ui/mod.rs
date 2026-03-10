mod anchors;
mod theme;
mod widgets;

pub use anchors::{UiAnchors, setup_ui_anchors};
pub use theme::UiTheme;
pub use widgets::{ButtonVariant, UiExt};

use bevy::prelude::*;

pub struct UiPlugin;

impl Plugin for UiPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(UiTheme::default());
        app.add_systems(Startup, setup_ui_anchors);
    }
}
