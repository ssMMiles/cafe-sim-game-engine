# Who owns what: plugins vs. level entities

Status: rules 1–3 are in force and the one violation in the tree is gone.
Rule 4's API is **deferred** — nothing needs it yet. The detector in
"Enforcement" is **proposed, not built**.

## The problem

Editor-mode isolation gave `DynamicPlugin` three hooks — `register`, `setup`,
`teardown` — and two ownership mechanisms:

| Mechanism | Covers | Cleaned up by |
|---|---|---|
| `ScriptOwned` (`engine_api`) | entities a plugin **spawns** | `despawn_script_owned` on teardown |
| Archetype diff during hydration | components a **script factory** inserts on a level entity | `dehydrate_plugin` → `remove_by_ids` |

Between them sits a third case with **no mechanism at all**: whatever a plugin
*system* does to a level entity it did not spawn. `cafe-sim`'s
`dress_cafe_shell` did exactly this — it walked the placed cafe shell's
hierarchy and inserted a private `CafeShellDressed` latch plus fresh material
handles on every sub-mesh.

Neither survives scrutiny:

- `dehydrate_plugin` removes only the `ComponentId`s the *factory* inserted,
  recorded by diffing the archetype across the `hydrate` call
  (`script_host.rs`). A component added later by a system is invisible to it.
- So on hot-reload the latch stays on the entity forever. Reload again and
  there are two, under different `ComponentId`s, because a rebuilt dylib gives
  its private types fresh `TypeId`s.

### What this does *not* cause

It is not a use-after-free. Unloaded libraries are `mem::forget`'d
(`script_host.rs`, three call sites), precisely so lingering vtable and
drop-glue pointers stay mapped. A stale plugin component is a leak and a
correctness hazard, not a crash.

The real costs are: level entities accumulate dead components across reloads;
plugin mutations to level state are never reverted, so unloading a plugin
leaves the level visibly changed; and "is the plugin loaded?" stops being
answerable from the entity.

## The rules

**1. A default belongs to the asset, not to a plugin.**

If a plugin system exists only to make level geometry look correct, that is
authoring data living in the wrong place. The symptom is unmistakable: the
object looks wrong in the editor and only comes right once gameplay starts.

`Cafe.gltf` now carries its own materials — brick walls with a
`KHR_texture_transform` tiling scale, a tinted floor — so the engine's
`ConvertToToon` dresses the shell as the scene loads, in both modes, with
`cafe-sim` stopped. `dress_cafe_shell` is deleted.

This does not foreclose re-skinnable wallpaper as a gameplay feature. It makes
it a deliberate, revertible override of a working default instead of the only
thing standing between the level and a white room.

**2. Plugin bookkeeping about a level entity lives in the plugin's schedule.**

A "have I processed this entity yet" latch is plugin state, not entity state.
Put it in a `Local<HashSet<Entity>>`. The `PluginSchedule` is dropped on
teardown, so its `Local`s die with it — no marker, no diff to record, nothing
to clean up. `CafeShellDressed` should always have been a `Local`.

(A plugin-private *resource* is not equivalent: `run_teardown` does not sweep
resources, so that just moves the leak.)

**3. A component a plugin genuinely needs on a level entity goes in via the
script factory.**

That is the path the archetype diff watches, so unload removes exactly what
hydration added. `cafe-shell`'s `CafeShell` marker is correct for this reason.

**4. If rule 3 is impossible, we need a tracked insert — and we don't have one.**

The case rule 3 can't serve: the component depends on the spawned scene
hierarchy, which does not exist yet when `hydrate` runs on the root.

The API would be a `Commands` extension in `engine-api` that queues an insert
*and* appends the resulting `ComponentId` to the entity's `HydratedScripts`
entry for that plugin, so `dehydrate_plugin` removes it with everything else.

**Deferred deliberately.** With `dress_cafe_shell` gone, nothing in the tree
needs it, and a mechanism with no caller is a guess about its own requirements.
Build it when a second real case turns up.

## Enforcement (proposed)

Rules 1–3 are conventions, and `spawn_owned` shows conventions get followed
right up until they don't. A cheap detector would catch violations at the
moment they matter:

On unload, before `dehydrate_plugin` clears its records, walk every entity
carrying `PlacedObject` and compare its components against the set the diff
recorded for this plugin. Any component whose type name starts with the
plugin's crate name (`cafe-sim` → `cafe_sim::`) but is *not* in
`HydratedScripts` was inserted behind the engine's back — warn, naming the
component and the entity.

Feasible: `ComponentInfo::name()` exists on bevy 0.18. Caveat: it returns a
`DebugName`, which carries the real string only when bevy's `debug` feature is
on, so this is a dev-build tool and must degrade to a silent no-op otherwise.
That is the right trade for a lint — plugin work happens in debug builds.

Whether to also *remove* what it finds, rather than only warning, is open.
Removing is more correct and makes rules 1–3 advisory rather than load-bearing;
warning is honest about the engine not knowing the plugin's intent.
