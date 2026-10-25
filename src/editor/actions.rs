//! Editor-level commands issued by the menu bar, toolbar, and panels.
//!
//! The egui pass runs in `PostUpdate` with a thin set of parameters; anything
//! that needs the wider ECS (world switching, plugin load/unload, state
//! changes) is sent as an [`EditorAction`] and applied here in `Update`. Keys
//! that mean the same thing (Shift+S, Shift+B, Delete, F) go through the same
//! messages, so there is one implementation per command.

use bevy::ecs::system::SystemParam;
use bevy::prelude::*;
use engine_api::Player;

use super::camera::place_camera_at_spawn;
use super::edits::RespawnPlaced;
use super::selection::{PlacedTree, Selection, ancestors, delete_placed, descendants};
use super::tool::EditorTool;
use super::ui::MenuState;
use crate::AppState;
use crate::game::gfx::camera::PlayerCamera;
use crate::game::world;
use crate::game::world::baking::BakeMessage;
use crate::game::world::objects::{PlacedEntityRecord, PlacedKind, spawn_entity_record};
use crate::game::world::objects::{PlacedObject, PlacedObjectRegistry};
use crate::game::world::streaming::StreamingState;
use crate::game::world::terrain::{
    TerrainChunk, bootstrap::TerrainSetupMarker, save::SaveTerrainMessage, storage::WorldConfig,
};
use crate::projects::{self, ActiveProject, ProjectRegistry};
use crate::script_host::ReloadRequested;

#[derive(Message, Debug, Clone, PartialEq, Eq)]
pub enum EditorAction {
    Save,
    Bake,
    ReloadPlugins,
    ExitToMenu,
    SwitchWorld(String),
    CreateWorld(String),
    AddPlugin(String),
    RemovePlugin(String),
    SetTool(EditorTool),
    DeleteSelection,
    /// Delete a group together with its members.
    DeleteSelectionRecursive,
    DuplicateSelection,
    FocusSelection,
    Deselect,
    /// File `child` under `parent` (a group), or at the top level.
    SetParent {
        child: Entity,
        parent: Option<Entity>,
    },
    /// Wrap the selection in a new group at its position.
    GroupSelection,
    /// Dissolve the selected group, re-filing its members under its parent.
    UngroupSelection,
}

/// Everything `world::switch_world` needs, plus the editor camera to re-seat.
#[derive(SystemParam)]
pub struct WorldSwitch<'w, 's> {
    world_config: Option<ResMut<'w, WorldConfig>>,
    chunk_query: Query<'w, 's, Entity, With<TerrainChunk>>,
    marker_query: Query<'w, 's, Entity, With<TerrainSetupMarker>>,
    object_query: Query<'w, 's, Entity, With<PlacedObject>>,
    placed_registry: ResMut<'w, PlacedObjectRegistry>,
    streaming_state: ResMut<'w, StreamingState>,
    player: Query<'w, 's, &'static mut Transform, (With<Player>, Without<PlayerCamera>)>,
    camera: Query<'w, 's, (&'static mut Transform, &'static mut PlayerCamera), Without<Player>>,
}

impl WorldSwitch<'_, '_> {
    fn switch(&mut self, commands: &mut Commands, target: &str) {
        let Some(world_config) = self.world_config.as_deref_mut() else {
            return;
        };
        if world_config.world_id == target {
            return;
        }
        let Ok(mut player_transform) = self.player.single_mut() else {
            warn!("SwitchWorld: no player entity found");
            return;
        };
        world::switch_world(
            target,
            commands,
            world_config,
            &self.chunk_query,
            &self.marker_query,
            &self.object_query,
            &mut self.placed_registry,
            &mut self.streaming_state,
            &mut player_transform,
        );
        if let Ok((mut t, mut cam)) = self.camera.single_mut() {
            place_camera_at_spawn(&mut t, &mut cam, player_transform.translation);
        }
    }
}

/// Save / bake / plugins / worlds / exit.
#[allow(clippy::too_many_arguments)]
pub fn apply_level_actions(
    mut actions: MessageReader<EditorAction>,
    mut commands: Commands,
    mut save: MessageWriter<SaveTerrainMessage>,
    mut bake: MessageWriter<BakeMessage>,
    mut reload: ResMut<ReloadRequested>,
    mut app_state: ResMut<NextState<AppState>>,
    mut world_switch: WorldSwitch,
    mut active: Option<ResMut<ActiveProject>>,
    mut project_registry: ResMut<ProjectRegistry>,
    mut menu: ResMut<MenuState>,
) {
    for action in actions.read() {
        match action {
            EditorAction::Save => {
                save.write(SaveTerrainMessage);
            }
            EditorAction::Bake => {
                info!("Bake requested");
                bake.write(BakeMessage);
            }
            EditorAction::ReloadPlugins => {
                reload.0 = true;
            }
            EditorAction::ExitToMenu => {
                app_state.set(AppState::MainMenu);
            }
            EditorAction::SwitchWorld(target) => {
                world_switch.switch(&mut commands, target);
            }
            EditorAction::CreateWorld(world_id) => {
                let Some(active) = active.as_deref_mut() else {
                    menu.new_world_error = Some("No active project.".into());
                    continue;
                };
                if let Err(msg) = validate_world_id(world_id, &active.0.worlds) {
                    menu.new_world_error = Some(msg);
                    continue;
                }
                active.0.worlds.push(world_id.clone());
                #[cfg(not(target_arch = "wasm32"))]
                if let Err(e) = projects::write_manifest(&active.0) {
                    active.0.worlds.retain(|w| w != world_id);
                    menu.new_world_error = Some(format!("Failed to update manifest: {e}"));
                    continue;
                }
                mirror_into_registry(&mut project_registry, active);
                info!("Added world '{world_id}' to project '{}'", active.0.id);
                menu.new_world_open = false;
                menu.new_world_error = None;
                // Jump straight into the new world.
                commands.write_message(EditorAction::SwitchWorld(world_id.clone()));
            }
            EditorAction::AddPlugin(plugin_id) => {
                let Some(active) = active.as_deref_mut() else {
                    continue;
                };
                if active.0.plugins.iter().any(|p| p == plugin_id) {
                    continue;
                }
                active.0.plugins.push(plugin_id.clone());
                active.0.plugins.sort();
                #[cfg(not(target_arch = "wasm32"))]
                if let Err(e) = projects::write_manifest(&active.0) {
                    error!("Failed to update manifest: {e}");
                    active.0.plugins.retain(|p| p != plugin_id);
                    continue;
                }
                mirror_into_registry(&mut project_registry, active);
                let id = plugin_id.clone();
                commands.queue(move |world: &mut World| {
                    crate::script_host::load_plugin(world, &id);
                });
                info!("Added plugin '{plugin_id}' to project '{}'", active.0.id);
            }
            EditorAction::RemovePlugin(plugin_id) => {
                let Some(active) = active.as_deref_mut() else {
                    continue;
                };
                if !active.0.plugins.iter().any(|p| p == plugin_id) {
                    continue;
                }
                active.0.plugins.retain(|p| p != plugin_id);
                #[cfg(not(target_arch = "wasm32"))]
                if let Err(e) = projects::write_manifest(&active.0) {
                    error!("Failed to update manifest: {e}");
                    active.0.plugins.push(plugin_id.clone());
                    active.0.plugins.sort();
                    continue;
                }
                mirror_into_registry(&mut project_registry, active);
                let id = plugin_id.clone();
                commands.queue(move |world: &mut World| {
                    crate::script_host::unload_plugin(world, &id);
                });
                info!(
                    "Removed plugin '{plugin_id}' from project '{}'",
                    active.0.id
                );
            }
            _ => {}
        }
    }
}

/// Tool and selection commands.
#[allow(clippy::too_many_arguments)]
pub fn apply_selection_actions(
    mut actions: MessageReader<EditorAction>,
    mut commands: Commands,
    asset_server: Res<AssetServer>,
    tool: Res<State<EditorTool>>,
    mut next_tool: ResMut<NextState<EditorTool>>,
    mut selection: ResMut<Selection>,
    mut registry: ResMut<PlacedObjectRegistry>,
    mut respawns: MessageWriter<RespawnPlaced>,
    mut placed: PlacedTree,
) {
    for action in actions.read() {
        match action {
            EditorAction::SetTool(target) => {
                if *tool.get() != *target {
                    next_tool.set(*target);
                }
            }
            EditorAction::DeleteSelection | EditorAction::DeleteSelectionRecursive => {
                if let Some(entity) = selection.primary.take() {
                    let recursive = *action == EditorAction::DeleteSelectionRecursive;
                    delete_placed(&mut commands, &mut registry, &mut placed, entity, recursive);
                }
            }
            EditorAction::DuplicateSelection => {
                if let Some(entity) = selection.primary {
                    respawns.write(RespawnPlaced::Duplicate(entity));
                }
            }
            EditorAction::FocusSelection => {
                selection.focus_requested = true;
            }
            EditorAction::Deselect => {
                selection.primary = None;
            }
            EditorAction::SetParent { child, parent } => {
                set_parent(*child, *parent, &mut placed, &mut registry);
            }
            EditorAction::GroupSelection => {
                let Some(entity) = selection.primary else {
                    continue;
                };
                if !registry.ids_seeded {
                    warn!("Group unavailable: world entity-id counter not seeded yet");
                    continue;
                }
                let Ok((_, obj, transform)) = placed.get(entity) else {
                    continue;
                };
                let (child_id, child_parent, chunk) = (obj.id, obj.parent, obj.chunk);
                let id = registry.alloc_id();
                let record = PlacedEntityRecord::new(
                    id,
                    PlacedKind::Group,
                    &Transform::from_translation(transform.translation),
                )
                .with_parent(child_parent);
                let group = spawn_entity_record(
                    &mut commands,
                    &asset_server,
                    &mut registry,
                    chunk,
                    &record,
                );
                registry.mark_dirty(chunk);
                if let Ok((_, mut obj, _)) = placed.get_mut(entity) {
                    obj.parent = Some(id);
                }
                selection.primary = Some(group);
                info!("Grouped placed id={child_id} under new group id={id}");
            }
            EditorAction::UngroupSelection => {
                let Some(entity) = selection.primary.take() else {
                    continue;
                };
                if placed.get(entity).is_ok_and(|(_, o, _)| o.kind.is_group()) {
                    delete_placed(&mut commands, &mut registry, &mut placed, entity, false);
                }
            }
            _ => {}
        }
    }
}

/// Re-file `child` under `parent`. Refuses anything that isn't a group, and
/// any move that would make a group its own ancestor.
fn set_parent(
    child: Entity,
    parent: Option<Entity>,
    placed: &mut PlacedTree,
    registry: &mut PlacedObjectRegistry,
) {
    let Ok((_, child_obj, _)) = placed.get(child) else {
        return;
    };
    let child_id = child_obj.id;
    let parent_id = match parent {
        None => None,
        Some(p) => {
            let Ok((_, parent_obj, _)) = placed.get(p) else {
                return;
            };
            if !parent_obj.kind.is_group() {
                info!("Only groups can contain entities");
                return;
            }
            let parent_id = parent_obj.id;
            let parent_of = |e: Entity| placed.get(e).ok().and_then(|(_, o, _)| o.parent);
            if p == child
                || descendants(child_id, placed).contains(&p)
                || ancestors(Some(parent_id), registry, parent_of).contains(&child)
            {
                info!("Refusing to make a group its own ancestor");
                return;
            }
            Some(parent_id)
        }
    };
    let Ok((_, mut obj, _)) = placed.get_mut(child) else {
        return;
    };
    if obj.parent == parent_id {
        return;
    }
    obj.parent = parent_id;
    registry.mark_dirty(obj.chunk);
}

/// Keep the main menu's view of the project in step with the live one.
fn mirror_into_registry(registry: &mut ProjectRegistry, active: &ActiveProject) {
    if let Some(slot) = registry.projects.iter_mut().find(|m| m.id == active.0.id) {
        slot.plugins = active.0.plugins.clone();
        slot.worlds = active.0.worlds.clone();
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn world_ids_are_validated() {
        let existing = vec!["cafe".to_owned()];
        assert!(validate_world_id("new-world_1", &existing).is_ok());
        assert!(validate_world_id("", &existing).is_err());
        assert!(validate_world_id("has space", &existing).is_err());
        assert!(validate_world_id("cafe", &existing).is_err());
    }
}
