use bevy::prelude::*;

use crate::game::world::baking::BakeMessage;

/// Watch for Shift+B and request a lightmap bake. The actual bake pipeline
/// lives in `world::baking` and listens for `BakeMessage`.
pub fn trigger_bake(
    input: Res<ButtonInput<KeyCode>>,
    mut bake_messages: MessageWriter<BakeMessage>,
) {
    if input.just_pressed(KeyCode::KeyB)
        && (input.pressed(KeyCode::ShiftLeft) || input.pressed(KeyCode::ShiftRight))
    {
        info!("Bake triggered (Shift+B)");
        bake_messages.write(BakeMessage);
    }
}
