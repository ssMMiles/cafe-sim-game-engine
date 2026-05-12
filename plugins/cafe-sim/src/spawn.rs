//! Spawn helpers that auto-tag every entity with [`engine_api::ScriptOwned`]
//! so plugin teardown sweeps the whole tree without each call site having to
//! remember the marker.
//!
//! Bring it into scope with `use crate::spawn::*;` and reach for
//! `spawn_owned` / `with_child_owned` instead of `spawn` / `with_child`.

use bevy::ecs::relationship::RelatedSpawnerCommands;
use bevy::ecs::system::EntityCommands;
use bevy::ecs::world::EntityWorldMut;
use bevy::prelude::*;
use engine_api::ScriptOwned;

use crate::PLUGIN_NAME;

#[inline]
fn owner() -> ScriptOwned {
    ScriptOwned::new(PLUGIN_NAME)
}

/// `spawn_owned` for command-style spawners (top-level `Commands` and the
/// `with_children` inner builder).
pub trait SpawnOwnedCommands {
    fn spawn_owned<B: Bundle>(&mut self, bundle: B) -> EntityCommands<'_>;
}

impl SpawnOwnedCommands for Commands<'_, '_> {
    fn spawn_owned<B: Bundle>(&mut self, bundle: B) -> EntityCommands<'_> {
        self.spawn((owner(), bundle))
    }
}

impl SpawnOwnedCommands for RelatedSpawnerCommands<'_, ChildOf> {
    fn spawn_owned<B: Bundle>(&mut self, bundle: B) -> EntityCommands<'_> {
        self.spawn((owner(), bundle))
    }
}

/// `spawn_owned` for exclusive-`World` setup code.
pub trait SpawnOwnedWorld {
    fn spawn_owned<B: Bundle>(&mut self, bundle: B) -> EntityWorldMut<'_>;
}

impl SpawnOwnedWorld for World {
    fn spawn_owned<B: Bundle>(&mut self, bundle: B) -> EntityWorldMut<'_> {
        self.spawn((owner(), bundle))
    }
}

/// `with_child_owned` for the single-child fluent form on `EntityCommands`.
pub trait WithChildOwned {
    fn with_child_owned<B: Bundle>(&mut self, bundle: B) -> &mut Self;
}

impl WithChildOwned for EntityCommands<'_> {
    fn with_child_owned<B: Bundle>(&mut self, bundle: B) -> &mut Self {
        self.with_child((owner(), bundle))
    }
}
