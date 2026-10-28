//! What the editor is working on, and the placed-entity tree helpers the
//! selection-aware systems share.
//!
//! [`Selection`] is the single answer to "which placed entity do the
//! inspector, Delete, and F act on". The viewport sets it with a click in the
//! Select tool, the outliner with a row click. Hover is tracked alongside so
//! the HUD can name what is under the cursor before it is clicked.
//!
//! Groups ([`PlacedKind::Group`]) are organisational: `PlacedObject.parent`
//! names a group's id, there is no ECS parenting, and members keep world
//! transforms. [`descendants`] walks that id tree over the loaded entities.
//!
//! Single selection only, by design (see `docs/EDITOR_UI_DESIGN.md` §2).

use bevy::camera::primitives::Aabb;
use bevy::prelude::*;
use engine_api::ScriptBindings;

use super::grab::GrabbedEntity;
use super::history::{Change, History};
use super::picking::{PlacedPicker, ViewportInput};
use crate::game::gfx::camera::PlayerCamera;
use crate::game::world::objects::{PlacedEntityRecord, PlacedObject, PlacedObjectRegistry};

const SELECTED_COLOR: Color = Color::srgb(1.0, 0.75, 0.2);
const HOVER_COLOR: Color = Color::srgba(1.0, 1.0, 1.0, 0.35);
/// Box drawn around mesh-less entities (lights, anchors), half-extent.
const POINT_HALF_EXTENT: f32 = 0.35;
/// Focus distance as a multiple of the selection's bounding radius.
const FOCUS_DISTANCE_FACTOR: f32 = 2.5;
const FOCUS_MIN_DISTANCE: f32 = 3.0;

#[derive(Resource, Default, Debug)]
pub struct Selection {
    pub primary: Option<Entity>,
    pub hovered: Option<Entity>,
    /// Set by the outliner / menu; consumed by [`focus_selection`].
    pub focus_requested: bool,
}

/// Every loaded placed entity, mutable: the shape the tree helpers take.
pub type PlacedTree<'w, 's> =
    Query<'w, 's, (Entity, &'static mut PlacedObject, &'static mut Transform)>;

/// Loaded entities filed (transitively) under the entity with `root_id`, in
/// breadth-first order. Members in unloaded chunks are simply absent.
pub fn descendants(root_id: u64, placed: &PlacedTree) -> Vec<Entity> {
    let mut out = Vec::new();
    let mut frontier = vec![root_id];
    while let Some(id) = frontier.pop() {
        for (entity, obj, _) in placed.iter() {
            if obj.parent == Some(id) && !out.contains(&entity) {
                out.push(entity);
                frontier.push(obj.id);
            }
        }
    }
    out
}

/// The group chain above an entity, nearest first, resolved through the
/// registry (so an unloaded ancestor ends the walk). `parent_of` looks up a
/// loaded entity's parent id, so callers can pass whichever query they hold.
pub fn ancestors(
    mut parent: Option<u64>,
    registry: &PlacedObjectRegistry,
    parent_of: impl Fn(Entity) -> Option<u64>,
) -> Vec<Entity> {
    let mut out = Vec::new();
    while let Some(id) = parent {
        let Some(&entity) = registry.by_id.get(&id) else {
            break;
        };
        if out.contains(&entity) {
            break; // defensive: a cycle on disk must not hang the editor
        }
        out.push(entity);
        parent = parent_of(entity);
    }
    out
}

/// Viewport click in the Select tool picks (or clears) the selection.
/// Alt+click selects the outermost group the hit belongs to.
pub fn select_on_click(
    viewport: Res<ViewportInput>,
    keys: Res<ButtonInput<KeyCode>>,
    registry: Res<PlacedObjectRegistry>,
    placed: Query<&PlacedObject>,
    mut picker: PlacedPicker,
    mut selection: ResMut<Selection>,
) {
    if viewport.clicked_px.is_none() {
        return;
    }
    let mut picked = picker.pick();
    let alt = keys.pressed(KeyCode::AltLeft) || keys.pressed(KeyCode::AltRight);
    if alt && let Some(hit) = picked {
        let chain = ancestors(
            placed.get(hit).ok().and_then(|o| o.parent),
            &registry,
            |e| placed.get(e).ok().and_then(|o| o.parent),
        );
        if let Some(top) = chain.last() {
            picked = Some(*top);
        }
    }
    if selection.primary != picked {
        selection.primary = picked;
    }
}

pub fn update_hover(mut picker: PlacedPicker, mut selection: ResMut<Selection>) {
    let hovered = picker.pick();
    if selection.hovered != hovered {
        selection.hovered = hovered;
    }
}

/// `OnExit(EditorTool::Select)`: hover is only tracked (and drawn) in the
/// Select tool, so it must not survive a tool switch as stale state.
pub fn clear_hover(mut selection: ResMut<Selection>) {
    selection.hovered = None;
}

/// Drop references to entities that no longer exist (chunk unload, delete,
/// grab). Runs every frame, before anything reads the selection.
pub fn validate_selection(mut selection: ResMut<Selection>, placed: Query<(), With<PlacedObject>>) {
    if selection.primary.is_some_and(|e| !placed.contains(e)) {
        selection.primary = None;
    }
    if selection.hovered.is_some_and(|e| !placed.contains(e)) {
        selection.hovered = None;
    }
}

/// Read-only queries [`placed_bounds`] needs.
#[derive(bevy::ecs::system::SystemParam)]
pub struct BoundsQueries<'w, 's> {
    children: Query<'w, 's, &'static Children>,
    bounds: Query<'w, 's, (&'static Aabb, &'static GlobalTransform)>,
    transforms: Query<'w, 's, &'static GlobalTransform>,
    placed: Query<'w, 's, (Entity, &'static PlacedObject)>,
}

/// World-space bounds of a placed entity: the union of its mesh descendants'
/// `Aabb`s (for a group, of every loaded member's), or a small fixed box for
/// mesh-less kinds.
pub fn placed_bounds(root: Entity, q: &BoundsQueries) -> Option<(Vec3, Vec3)> {
    let mut min = Vec3::splat(f32::INFINITY);
    let mut max = Vec3::splat(f32::NEG_INFINITY);
    let mut any = false;
    let mut extend = |p: Vec3| {
        min = min.min(p);
        max = max.max(p);
        any = true;
    };

    // Group: members' bounds (recursively), plus the group's own origin.
    let mut roots = vec![root];
    if let Ok((_, obj)) = q.placed.get(root)
        && obj.kind.is_group()
    {
        let mut frontier = vec![obj.id];
        while let Some(id) = frontier.pop() {
            for (e, o) in q.placed.iter() {
                if o.parent == Some(id) && !roots.contains(&e) {
                    roots.push(e);
                    frontier.push(o.id);
                }
            }
        }
    }

    for entity in roots {
        let mut found_mesh = false;
        for e in std::iter::once(entity).chain(q.children.iter_descendants(entity)) {
            let Ok((aabb, gt)) = q.bounds.get(e) else {
                continue;
            };
            let c = Vec3::from(aabb.center);
            let h = Vec3::from(aabb.half_extents);
            for i in 0..8 {
                let corner = c + h * Vec3::new(
                    if i & 1 == 0 { -1.0 } else { 1.0 },
                    if i & 2 == 0 { -1.0 } else { 1.0 },
                    if i & 4 == 0 { -1.0 } else { 1.0 },
                );
                extend(gt.transform_point(corner));
            }
            found_mesh = true;
        }
        if !found_mesh && let Ok(gt) = q.transforms.get(entity) {
            let o = gt.translation();
            extend(o - Vec3::splat(POINT_HALF_EXTENT));
            extend(o + Vec3::splat(POINT_HALF_EXTENT));
        }
    }

    any.then(|| ((min + max) * 0.5, (max - min) * 0.5))
}

pub fn draw_selection_gizmos(mut gizmos: Gizmos, selection: Res<Selection>, q: BoundsQueries) {
    let mut draw = |entity: Entity, color: Color| {
        if let Some((center, half)) = placed_bounds(entity, &q) {
            let transform = Transform::from_translation(center).with_scale(half * 2.0);
            gizmos.cube(transform, color);
        }
        // A group also shows its own origin: that is what its transform
        // fields move, and what members orbit when it is rotated.
        if let Ok((_, obj)) = q.placed.get(entity)
            && obj.kind.is_group()
            && let Ok(gt) = q.transforms.get(entity)
        {
            gizmos.sphere(gt.translation(), POINT_HALF_EXTENT * 0.5, color);
        }
    };
    if let Some(hovered) = selection.hovered.filter(|h| Some(*h) != selection.primary) {
        draw(hovered, HOVER_COLOR);
    }
    if let Some(primary) = selection.primary {
        draw(primary, SELECTED_COLOR);
    }
}

/// `F` (or a focus request): frame the selection from the current view
/// direction.
pub fn focus_selection(
    input: Res<ButtonInput<KeyCode>>,
    mut selection: ResMut<Selection>,
    q: BoundsQueries,
    mut camera: Query<&mut Transform, With<PlayerCamera>>,
) {
    let requested = std::mem::take(&mut selection.focus_requested);
    if !requested && !input.just_pressed(KeyCode::KeyF) {
        return;
    }
    let Some(primary) = selection.primary else {
        return;
    };
    let Some((center, half)) = placed_bounds(primary, &q) else {
        return;
    };
    let Ok(mut cam) = camera.single_mut() else {
        return;
    };
    let distance = (half.length() * FOCUS_DISTANCE_FACTOR).max(FOCUS_MIN_DISTANCE);
    let forward = cam.forward().as_vec3();
    cam.translation = center - forward * distance;
}

/// `Delete` removes the selection; `Shift+Delete` removes a group *with* its
/// members. Bails while something is carried, where Delete means "destroy
/// the carried entity" (see `grab`).
pub fn delete_selected(
    mut commands: Commands,
    input: Res<ButtonInput<KeyCode>>,
    grabbed: Res<GrabbedEntity>,
    mut selection: ResMut<Selection>,
    mut registry: ResMut<PlacedObjectRegistry>,
    mut history: ResMut<History>,
    mut placed: PlacedTree,
    bindings: Query<&ScriptBindings>,
) {
    if grabbed.0.is_some() || !input.just_pressed(KeyCode::Delete) {
        return;
    }
    let Some(entity) = selection.primary.take() else {
        return;
    };
    let recursive = input.pressed(KeyCode::ShiftLeft) || input.pressed(KeyCode::ShiftRight);
    delete_placed(
        &mut commands,
        &mut registry,
        &mut history,
        &mut placed,
        &bindings,
        entity,
        recursive,
    );
}

/// Remove a placed entity from the world and schedule its chunk for saving,
/// recording one undo transaction.
///
/// Deleting a group re-files its members under the group's own parent, so
/// nothing is lost by accident; `recursive` deletes the members too.
pub fn delete_placed(
    commands: &mut Commands,
    registry: &mut PlacedObjectRegistry,
    history: &mut History,
    placed: &mut PlacedTree,
    bindings: &Query<&ScriptBindings>,
    entity: Entity,
    recursive: bool,
) {
    let record_of = |placed: &PlacedTree, entity: Entity| -> Option<PlacedEntityRecord> {
        let (_, obj, transform) = placed.get(entity).ok()?;
        Some(obj.to_record(transform, bindings.get(entity).ok()))
    };

    let Ok((_, obj, _)) = placed.get(entity) else {
        return;
    };
    let (id, chunk, label, parent, is_group) = (
        obj.id,
        obj.chunk,
        obj.kind.label(),
        obj.parent,
        obj.kind.is_group(),
    );
    let display = obj.display_name();
    let Some(record) = record_of(placed, entity) else {
        return;
    };
    // Member changes first, the group itself last: undo runs in reverse, so
    // the group exists again before its members are re-filed / re-spawned.
    let mut changes = Vec::new();

    if is_group {
        if recursive {
            for member in descendants(id, placed) {
                let Ok((_, m, _)) = placed.get(member) else {
                    continue;
                };
                let (m_id, m_chunk) = (m.id, m.chunk);
                if let Some(record) = record_of(placed, member) {
                    changes.push(Change::Despawn {
                        chunk: m_chunk,
                        record,
                    });
                }
                registry.by_id.remove(&m_id);
                registry.mark_dirty(m_chunk);
                commands.entity(member).despawn();
            }
        } else {
            for (_, mut m, _) in placed.iter_mut() {
                if m.parent == Some(id) {
                    changes.push(Change::Parent {
                        id: m.id,
                        before: Some(id),
                        after: parent,
                    });
                    m.parent = parent;
                    registry.mark_dirty(m.chunk);
                }
            }
        }
    }

    changes.push(Change::Despawn { chunk, record });
    registry.by_id.remove(&id);
    registry.mark_dirty(chunk);
    commands.entity(entity).despawn();
    history.push(format!("Delete '{display}'"), changes);
    info!("Deleted placed {label} id={id} from chunk {chunk:?}");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::world::objects::PlacedKind;

    fn placed(id: u64, kind: PlacedKind, parent: Option<u64>) -> (PlacedObject, Transform) {
        (
            PlacedObject {
                id,
                chunk: (0, 0),
                name: String::new(),
                kind,
                parent,
            },
            Transform::default(),
        )
    }

    #[test]
    fn selection_forgets_despawned_entities() {
        let mut app = App::new();
        app.init_resource::<Selection>();
        app.add_systems(Update, validate_selection);

        let kept = app
            .world_mut()
            .spawn(placed(1, PlacedKind::Empty, None))
            .id();
        let gone = app
            .world_mut()
            .spawn(placed(2, PlacedKind::Empty, None))
            .id();
        app.world_mut().resource_mut::<Selection>().primary = Some(gone);
        app.world_mut().resource_mut::<Selection>().hovered = Some(kept);

        app.world_mut().despawn(gone);
        app.update();

        let selection = app.world().resource::<Selection>();
        assert_eq!(selection.primary, None);
        assert_eq!(selection.hovered, Some(kept));
    }

    #[test]
    fn selection_forgets_entities_that_stop_being_placed() {
        let mut app = App::new();
        app.init_resource::<Selection>();
        app.add_systems(Update, validate_selection);

        let e = app
            .world_mut()
            .spawn(placed(1, PlacedKind::Empty, None))
            .id();
        app.world_mut().resource_mut::<Selection>().primary = Some(e);
        app.world_mut().entity_mut(e).remove::<PlacedObject>();
        app.update();

        assert_eq!(app.world().resource::<Selection>().primary, None);
    }

    #[test]
    fn deleting_a_group_refiles_members_under_its_parent() {
        let mut world = World::new();
        world.init_resource::<PlacedObjectRegistry>();
        world.init_resource::<History>();
        let outer = world.spawn(placed(1, PlacedKind::Group, None)).id();
        let inner = world.spawn(placed(2, PlacedKind::Group, Some(1))).id();
        let leaf = world.spawn(placed(3, PlacedKind::Empty, Some(2))).id();

        let mut system = IntoSystem::into_system(
            |mut commands: Commands,
             mut registry: ResMut<PlacedObjectRegistry>,
             mut history: ResMut<History>,
             mut placed: PlacedTree,
             bindings: Query<&ScriptBindings>| {
                let target = placed
                    .iter()
                    .find(|(_, o, _)| o.id == 2)
                    .map(|(e, _, _)| e)
                    .unwrap();
                delete_placed(
                    &mut commands,
                    &mut registry,
                    &mut history,
                    &mut placed,
                    &bindings,
                    target,
                    false,
                );
            },
        );
        system.initialize(&mut world);
        system.run((), &mut world).unwrap();
        system.apply_deferred(&mut world);

        assert!(world.get_entity(inner).is_err(), "group despawned");
        assert!(world.get_entity(outer).is_ok());
        assert_eq!(
            world.get::<PlacedObject>(leaf).unwrap().parent,
            Some(1),
            "member moves up to the deleted group's parent"
        );
        assert!(
            world
                .resource::<PlacedObjectRegistry>()
                .dirty_chunks
                .contains(&(0, 0))
        );
    }

    #[test]
    fn recursive_delete_takes_members_too() {
        let mut world = World::new();
        world.init_resource::<PlacedObjectRegistry>();
        world.init_resource::<History>();
        let group = world.spawn(placed(1, PlacedKind::Group, None)).id();
        let leaf = world.spawn(placed(2, PlacedKind::Empty, Some(1))).id();
        let other = world.spawn(placed(3, PlacedKind::Empty, None)).id();

        let mut system = IntoSystem::into_system(
            move |mut commands: Commands,
                  mut registry: ResMut<PlacedObjectRegistry>,
                  mut history: ResMut<History>,
                  mut placed: PlacedTree,
                  bindings: Query<&ScriptBindings>| {
                delete_placed(
                    &mut commands,
                    &mut registry,
                    &mut history,
                    &mut placed,
                    &bindings,
                    group,
                    true,
                );
            },
        );
        system.initialize(&mut world);
        system.run((), &mut world).unwrap();
        system.apply_deferred(&mut world);

        assert!(world.get_entity(group).is_err());
        assert!(world.get_entity(leaf).is_err());
        assert!(world.get_entity(other).is_ok());
        let history = world.resource::<History>();
        assert_eq!(
            history.undo_len(),
            1,
            "one transaction for the whole delete"
        );
        assert_eq!(history.undo_label(), Some("Delete 'Group'"));
    }
}
