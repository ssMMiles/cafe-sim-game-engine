use bevy::prelude::*;

use crate::game::world::terrain::save::SaveTerrainMessage;

/// Watch for Shift+S and request a terrain save. The actual persistence work
/// runs in `world::terrain::save::save_terrain` (still in the world module
/// so the bake/streaming code can re-use it).
pub fn trigger_save_terrain(
    input: Res<ButtonInput<KeyCode>>,
    mut writer: MessageWriter<SaveTerrainMessage>,
) {
    if input.just_pressed(KeyCode::KeyS)
        && (input.pressed(KeyCode::ShiftLeft) || input.pressed(KeyCode::ShiftRight))
    {
        writer.write(SaveTerrainMessage);
    }
}
