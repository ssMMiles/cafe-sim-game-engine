//! Script bindings: the data that associates a placed level entity with a
//! named script exported by a dynamic plugin.
//!
//! # Why these types live here
//!
//! A plugin's own component types have a `TypeId` that changes on every
//! rebuild, so the host can never name them. What the host *can* do is hold
//! stable, `engine-api`-owned data on the entity ([`ScriptBindings`]) and call
//! back into the plugin to turn that data into plugin components. Everything
//! in this module is therefore shared across the FFI boundary and must keep a
//! stable layout — treat it like the ABI it is, and bump
//! [`ENGINE_SCRIPTS_ABI_VERSION`](crate::ENGINE_SCRIPTS_ABI_VERSION) on any
//! breaking change.
//!
//! [`ScriptBinding`] doubles as the engine's on-disk record type, which is why
//! it carries rkyv derives: there is exactly one definition of a binding, not
//! a serialized form plus a runtime mirror that could drift apart.
//!
//! # Lifecycle
//!
//! 1. A plugin calls [`ScriptRegistry::register_script`] in its `setup`.
//! 2. The host finds entities carrying [`ScriptBindings`] and, for each
//!    binding naming a registered script, calls the registered
//!    [`ScriptHydrateFn`] to insert the plugin's components.
//! 3. On unload or hot-reload the host removes exactly the components that
//!    the factory added and re-runs step 2 against the new dylib.
//!
//! Anything a factory *spawns* (children, UI) is not tracked by that diff, so
//! it must still be tagged [`ScriptOwned`](crate::ScriptOwned) for teardown to
//! sweep it.

use bevy::ecs::world::EntityWorldMut;
use bevy::prelude::*;
use rkyv::{Archive, Deserialize, Serialize};
use std::collections::HashMap;

// ---------------------------------------------------------------------------
// Binding data
// ---------------------------------------------------------------------------

/// Associates a placed entity with one named script from one plugin.
///
/// Also the on-disk record: the engine serializes these straight into a
/// level's `.objects` chunk files.
#[derive(Archive, Deserialize, Serialize, Clone, Debug, PartialEq)]
pub struct ScriptBinding {
    /// Plugin id — the directory name under `plugins/`, e.g. `"cafe-sim"`.
    pub plugin: String,
    /// Script name registered by that plugin, e.g. `"coffee_machine"`.
    pub script: String,
    pub props: Vec<ScriptProp>,
}

impl ScriptBinding {
    pub fn new(plugin: impl Into<String>, script: impl Into<String>) -> Self {
        Self {
            plugin: plugin.into(),
            script: script.into(),
            props: Vec::new(),
        }
    }

    pub fn with_props(mut self, props: Vec<ScriptProp>) -> Self {
        self.props = props;
        self
    }

    /// Raw property lookup. Prefer the typed `*_or` accessors below.
    pub fn get(&self, key: &str) -> Option<&PropValue> {
        self.props.iter().find(|p| p.key == key).map(|p| &p.value)
    }

    pub fn bool_or(&self, key: &str, fallback: bool) -> bool {
        match self.get(key) {
            Some(PropValue::Bool(v)) => *v,
            _ => fallback,
        }
    }

    pub fn int_or(&self, key: &str, fallback: i64) -> i64 {
        match self.get(key) {
            Some(PropValue::Int(v)) => *v,
            _ => fallback,
        }
    }

    pub fn float_or(&self, key: &str, fallback: f32) -> f32 {
        match self.get(key) {
            Some(PropValue::Float(v)) => *v,
            _ => fallback,
        }
    }

    pub fn str_or<'a>(&'a self, key: &str, fallback: &'a str) -> &'a str {
        match self.get(key) {
            Some(PropValue::Str(v)) => v.as_str(),
            _ => fallback,
        }
    }

    pub fn vec3_or(&self, key: &str, fallback: Vec3) -> Vec3 {
        match self.get(key) {
            Some(PropValue::Vec3(v)) => Vec3::from(*v),
            _ => fallback,
        }
    }

    pub fn color_or(&self, key: &str, fallback: Color) -> Color {
        match self.get(key) {
            Some(PropValue::Color(c)) => Color::linear_rgba(c[0], c[1], c[2], c[3]),
            _ => fallback,
        }
    }

    /// The referenced placed-entity id, if this prop is an entity reference.
    /// Resolving it to a live [`Entity`] is the plugin's job (the engine's
    /// placed-object registry maps id → entity).
    pub fn entity_ref(&self, key: &str) -> Option<u64> {
        match self.get(key) {
            Some(PropValue::EntityRef(id)) => Some(*id),
            _ => None,
        }
    }
}

/// One authored property on a binding.
#[derive(Archive, Deserialize, Serialize, Clone, Debug, PartialEq)]
pub struct ScriptProp {
    pub key: String,
    pub value: PropValue,
}

impl ScriptProp {
    pub fn new(key: impl Into<String>, value: PropValue) -> Self {
        Self {
            key: key.into(),
            value,
        }
    }
}

/// Typed property value.
///
/// New variants must be **appended, never reordered** — rkyv encodes enum
/// variants positionally, so reordering silently corrupts existing levels.
#[derive(Archive, Deserialize, Serialize, Clone, Debug, PartialEq)]
pub enum PropValue {
    Bool(bool),
    Int(i64),
    Float(f32),
    Str(String),
    Vec3([f32; 3]),
    /// Linear RGBA.
    Color([f32; 4]),
    /// Another placed entity's stable id within the same world.
    EntityRef(u64),
}

/// Authored script bindings, attached by the engine to any placed entity whose
/// level record carries them.
///
/// Present whether or not the naming plugin is loaded: a level that references
/// a plugin the project does not load keeps its bindings dormant rather than
/// dropping them, so adding the plugin later is all that is needed.
#[derive(Component, Clone, Debug, Default)]
pub struct ScriptBindings(pub Vec<ScriptBinding>);

impl ScriptBindings {
    pub fn iter(&self) -> impl Iterator<Item = &ScriptBinding> {
        self.0.iter()
    }

    /// Whether any binding names `plugin`.
    pub fn references_plugin(&self, plugin: &str) -> bool {
        self.0.iter().any(|b| b.plugin == plugin)
    }
}

// ---------------------------------------------------------------------------
// Schema
// ---------------------------------------------------------------------------

/// A property a script understands, with the value the editor seeds when the
/// binding is first attached.
///
/// Everything beyond `key` + `default` is editor presentation: the inspector
/// falls back to the raw key and generic widgets when it is absent, so the
/// metadata is never required for a script to work.
#[derive(Clone, Copy, Debug)]
pub struct PropDef {
    pub key: &'static str,
    pub default: PropDefault,
    /// Human-facing row label in the inspector; the raw `key` when absent.
    pub label: Option<&'static str>,
    /// Hover text for the row in the inspector.
    pub doc: Option<&'static str>,
    /// Drag bounds for `Int` / `Float` props; other kinds ignore it.
    pub range: Option<(f64, f64)>,
    /// Drag speed (value change per point dragged) for `Int` / `Float` props.
    pub speed: Option<f64>,
}

impl PropDef {
    pub const fn new(key: &'static str, default: PropDefault) -> Self {
        Self {
            key,
            default,
            label: None,
            doc: None,
            range: None,
            speed: None,
        }
    }

    pub const fn with_label(mut self, label: &'static str) -> Self {
        self.label = Some(label);
        self
    }

    pub const fn with_doc(mut self, doc: &'static str) -> Self {
        self.doc = Some(doc);
        self
    }

    pub const fn with_range(mut self, min: f64, max: f64) -> Self {
        self.range = Some((min, max));
        self
    }

    pub const fn with_speed(mut self, speed: f64) -> Self {
        self.speed = Some(speed);
        self
    }

    /// What the editor prints for this prop's row.
    pub fn title(&self) -> &'static str {
        self.label.unwrap_or(self.key)
    }
}

/// `const`-constructible mirror of [`PropValue`], so a schema can live in a
/// `static` (a `PropValue::Str` holds a `String`, which cannot).
#[derive(Clone, Copy, Debug)]
pub enum PropDefault {
    Bool(bool),
    Int(i64),
    Float(f32),
    Str(&'static str),
    Vec3([f32; 3]),
    Color([f32; 4]),
    EntityRef(u64),
}

impl PropDefault {
    pub fn to_value(self) -> PropValue {
        match self {
            PropDefault::Bool(v) => PropValue::Bool(v),
            PropDefault::Int(v) => PropValue::Int(v),
            PropDefault::Float(v) => PropValue::Float(v),
            PropDefault::Str(v) => PropValue::Str(v.to_owned()),
            PropDefault::Vec3(v) => PropValue::Vec3(v),
            PropDefault::Color(v) => PropValue::Color(v),
            PropDefault::EntityRef(v) => PropValue::EntityRef(v),
        }
    }
}

/// What a plugin tells the engine about one of its scripts, so the editor can
/// list it and seed sensible props without the plugin owning any UI.
#[derive(Clone, Copy, Debug)]
pub struct ScriptSchema {
    /// Script name, unique within the plugin. This is the stable id bindings
    /// store and save files carry — renaming it orphans existing bindings.
    pub name: &'static str,
    /// Human-facing name the editor shows; `name` when absent. Purely
    /// presentation, safe to change at any time.
    pub display_name: Option<&'static str>,
    pub props: &'static [PropDef],
    /// Mesh the editor offers as a preset when placing this script, letting a
    /// plugin's catalog entry be a one-click "mesh + binding" placement.
    pub default_mesh: Option<&'static str>,
}

impl ScriptSchema {
    pub const fn new(name: &'static str) -> Self {
        Self {
            name,
            display_name: None,
            props: &[],
            default_mesh: None,
        }
    }

    pub const fn with_display_name(mut self, display_name: &'static str) -> Self {
        self.display_name = Some(display_name);
        self
    }

    pub const fn with_props(mut self, props: &'static [PropDef]) -> Self {
        self.props = props;
        self
    }

    pub const fn with_default_mesh(mut self, asset_path: &'static str) -> Self {
        self.default_mesh = Some(asset_path);
        self
    }

    /// What the editor prints for this script.
    pub fn title(&self) -> &'static str {
        self.display_name.unwrap_or(self.name)
    }

    /// The prop list the editor attaches for a fresh binding.
    pub fn default_props(&self) -> Vec<ScriptProp> {
        self.props
            .iter()
            .map(|d| ScriptProp::new(d.key, d.default.to_value()))
            .collect()
    }
}

// ---------------------------------------------------------------------------
// Registry
// ---------------------------------------------------------------------------

/// Turns a binding into plugin components on the entity.
///
/// The host records which components the call added and removes exactly those
/// on unload, so a factory should **insert** rather than spawn. Use plain
/// `fn` pointers (not closures): there is no state to capture — plugins keep
/// state in resources — and nothing with a vtable to keep alive.
pub type ScriptHydrateFn = fn(&mut EntityWorldMut, &ScriptBinding);

/// Per-plugin hooks for level-scoped data stored in the world's metadata.
///
/// `save` is called when the level is saved; returning `None` stores nothing.
/// `load` is called once per level load, and again after a hot-reload, with
/// whatever `save` last produced.
#[derive(Clone, Copy)]
pub struct LevelDataHooks {
    pub save: fn(&mut World) -> Option<Vec<u8>>,
    pub load: fn(&mut World, &[u8]),
}

/// One registered script.
pub struct RegisteredScript {
    pub plugin: String,
    pub schema: ScriptSchema,
    pub hydrate: ScriptHydrateFn,
}

/// Scripts and level-data hooks exported by the currently loaded plugins.
///
/// Inserted by the engine before any plugin loads. Entries are dropped when
/// their plugin unloads, and re-added by the new dylib on hot-reload — so a
/// lookup miss means "that plugin isn't loaded right now", never "that script
/// doesn't exist".
#[derive(Resource, Default)]
pub struct ScriptRegistry {
    scripts: HashMap<(String, String), RegisteredScript>,
    level_hooks: HashMap<String, LevelDataHooks>,
}

impl ScriptRegistry {
    /// Register one script. Call from a plugin's `setup`. Re-registering the
    /// same `(plugin, name)` replaces the previous entry.
    pub fn register_script(
        &mut self,
        plugin: &str,
        schema: ScriptSchema,
        hydrate: ScriptHydrateFn,
    ) {
        self.scripts.insert(
            (plugin.to_owned(), schema.name.to_owned()),
            RegisteredScript {
                plugin: plugin.to_owned(),
                schema,
                hydrate,
            },
        );
    }

    /// Register this plugin's level-data save/load hooks.
    pub fn register_level_hooks(&mut self, plugin: &str, hooks: LevelDataHooks) {
        self.level_hooks.insert(plugin.to_owned(), hooks);
    }

    pub fn get(&self, plugin: &str, script: &str) -> Option<&RegisteredScript> {
        // Tuple keys can't be looked up from (&str, &str) without allocating,
        // and this runs only for newly-spawned entities, so a scan is fine and
        // avoids two allocations per lookup.
        self.scripts
            .values()
            .find(|s| s.plugin == plugin && s.schema.name == script)
    }

    pub fn level_hooks(&self, plugin: &str) -> Option<LevelDataHooks> {
        self.level_hooks.get(plugin).copied()
    }

    /// Every registered script, for the editor's script picker.
    pub fn scripts(&self) -> impl Iterator<Item = &RegisteredScript> {
        self.scripts.values()
    }

    /// Drop everything belonging to `plugin`. Called by the host just before a
    /// plugin is unloaded or replaced.
    pub fn remove_plugin(&mut self, plugin: &str) {
        self.scripts.retain(|(p, _), _| p != plugin);
        self.level_hooks.remove(plugin);
    }

    pub fn is_empty(&self) -> bool {
        self.scripts.is_empty() && self.level_hooks.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    static PROPS: &[PropDef] = &[
        PropDef::new("capacity", PropDefault::Int(4)),
        PropDef::new("label", PropDefault::Str("brew")),
    ];

    fn noop(_: &mut EntityWorldMut, _: &ScriptBinding) {}

    #[test]
    fn schema_seeds_default_props() {
        let schema = ScriptSchema::new("coffee_machine")
            .with_props(PROPS)
            .with_default_mesh("cafe-sim/machine.gltf#Scene0");

        let props = schema.default_props();
        assert_eq!(props.len(), 2);
        assert_eq!(props[0].key, "capacity");
        assert_eq!(props[0].value, PropValue::Int(4));
        assert_eq!(props[1].value, PropValue::Str("brew".to_owned()));
        assert_eq!(schema.default_mesh, Some("cafe-sim/machine.gltf#Scene0"));
    }

    /// Editor metadata is optional presentation: titles fall back to the
    /// machine names, and the builders are `const` so a decorated schema can
    /// still live in a `static` (which is where plugins keep theirs).
    #[test]
    fn display_metadata_is_const_constructible_and_falls_back() {
        static DECORATED: &[PropDef] = &[PropDef::new("speed", PropDefault::Float(1.0))
            .with_label("Speed")
            .with_doc("Radians per second.")
            .with_range(-25.0, 25.0)
            .with_speed(0.05)];
        static SCHEMA: ScriptSchema = ScriptSchema::new("spinner")
            .with_display_name("Spinner")
            .with_props(DECORATED);

        assert_eq!(SCHEMA.title(), "Spinner");
        assert_eq!(DECORATED[0].title(), "Speed");
        assert_eq!(DECORATED[0].range, Some((-25.0, 25.0)));
        assert_eq!(DECORATED[0].speed, Some(0.05));
        assert_eq!(DECORATED[0].doc, Some("Radians per second."));

        let bare = ScriptSchema::new("spinner");
        assert_eq!(bare.title(), "spinner");
        assert_eq!(
            PropDef::new("speed", PropDefault::Float(1.0)).title(),
            "speed"
        );
    }

    #[test]
    fn typed_accessors_fall_back_on_missing_or_mistyped() {
        let binding = ScriptBinding::new("cafe-sim", "coffee_machine").with_props(vec![
            ScriptProp::new("capacity", PropValue::Int(7)),
            ScriptProp::new("wrong", PropValue::Str("nope".into())),
        ]);

        assert_eq!(binding.int_or("capacity", 1), 7);
        // Missing key falls back.
        assert_eq!(binding.int_or("absent", 1), 1);
        // Present but wrong variant also falls back rather than panicking.
        assert_eq!(binding.int_or("wrong", 1), 1);
        assert_eq!(binding.str_or("wrong", "fallback"), "nope");
        assert!(binding.entity_ref("capacity").is_none());
    }

    #[test]
    fn remove_plugin_drops_only_that_plugins_entries() {
        let mut reg = ScriptRegistry::default();
        reg.register_script("cafe-sim", ScriptSchema::new("a"), noop);
        reg.register_script("cafe-sim", ScriptSchema::new("b"), noop);
        reg.register_script("example", ScriptSchema::new("a"), noop);
        reg.register_level_hooks(
            "cafe-sim",
            LevelDataHooks {
                save: |_| None,
                load: |_, _| {},
            },
        );

        assert!(reg.get("cafe-sim", "a").is_some());
        assert!(reg.get("example", "a").is_some());

        reg.remove_plugin("cafe-sim");

        assert!(reg.get("cafe-sim", "a").is_none());
        assert!(reg.get("cafe-sim", "b").is_none());
        assert!(reg.level_hooks("cafe-sim").is_none());
        // The same script name under another plugin survives.
        assert!(reg.get("example", "a").is_some());
    }

    #[test]
    fn re_registering_replaces_rather_than_duplicates() {
        let mut reg = ScriptRegistry::default();
        reg.register_script("example", ScriptSchema::new("a"), noop);
        reg.register_script(
            "example",
            ScriptSchema::new("a").with_default_mesh("mesh.gltf#Scene0"),
            noop,
        );
        assert_eq!(reg.scripts().count(), 1);
        assert_eq!(
            reg.get("example", "a").unwrap().schema.default_mesh,
            Some("mesh.gltf#Scene0")
        );
    }
}
