use bevy::{
    diagnostic::{DiagnosticsStore, FrameTimeDiagnosticsPlugin},
    prelude::*,
};

use super::shared::{DisplayBuilder, DisplayPosition};

#[derive(Component)]
pub struct FpsDisplay;

pub fn setup_fps_display(mut commands: Commands) {
    create_fps_display(&mut commands, "...");
}

pub fn update_fps_display(
    mut commands: Commands,
    diagnostics: Res<DiagnosticsStore>,
    fps_query: Query<Entity, With<FpsDisplay>>,
) {
    for entity in fps_query.iter() {
        commands.entity(entity).despawn();
    }

    let fps = if let Some(fps_diagnostic) = diagnostics.get(&FrameTimeDiagnosticsPlugin::FPS) {
        if let Some(fps_smoothed) = fps_diagnostic.smoothed() {
            format!("{}", fps_smoothed.floor() as u32)
        } else {
            "...".to_string()
        }
    } else {
        "...".to_string()
    };

    create_fps_display(&mut commands, &fps);
}

fn create_fps_display(commands: &mut Commands, fps: &str) {
    DisplayBuilder::new()
        .position(DisplayPosition::TopLeft {
            top: 20.0,
            left: 20.0,
        })
        .build(commands, FpsDisplay, |parent| {
            parent.spawn((
                Text::new(format!("FPS: {}", fps)),
                TextFont {
                    font_size: 16.0,
                    ..default()
                },
                TextColor(Color::srgb(1.0, 1.0, 1.0)),
            ));
        });
}
