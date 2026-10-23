use bevy::prelude::*;
use std::collections::HashSet;

/// Re-export so the `add!` macro can reference bevy via `$crate::bevy`.
pub use bevy;

pub mod script;
pub mod toon_material;
pub use script::{
    LevelDataHooks, PropDef, PropDefault, PropValue, RegisteredScript, ScriptBinding,
    ScriptBindings, ScriptHydrateFn, ScriptProp, ScriptRegistry, ScriptSchema,
};
pub use toon_material::{DefaultToonLightmap, ToonMaterial, create_default_toon_lightmap};

// ---------------------------------------------------------------------------
// Dynamic plugin interface
// ---------------------------------------------------------------------------

/// ABI version. Bump on any breaking change to [`DynamicPlugin`] or the
/// generated FFI entry points.
///
/// - 3: added the [`script`] module — [`ScriptRegistry`], [`ScriptBindings`],
///   and the level-data hooks.
pub const ENGINE_SCRIPTS_ABI_VERSION: u32 = 3;

/// A plugin exported from a dynamically loaded script dylib.
///
/// Lifecycle: the host calls [`setup`](Self::setup) on load and
/// [`teardown`](Self::teardown) immediately before the dylib is replaced on
/// hot-reload. Setup populates the dedicated `ScriptUpdate` schedule and may
/// spawn entities/insert resources. Teardown is for any custom cleanup —
/// entities tagged with [`ScriptOwned`] are despawned automatically by the
/// generated FFI shim *after* `teardown` runs, so plugin-private components
/// (defined in the dylib) get their drop fns called while the old code pages
/// are still mapped.
///
/// Implementors are constructed via [`Default::default`] each time the host
/// invokes a lifecycle method, so don't store state on `self` — use Bevy
/// resources instead.
pub trait DynamicPlugin {
    fn setup(&self, world: &mut bevy::prelude::World, schedule: &mut bevy::ecs::schedule::Schedule);
    fn teardown(&self, _world: &mut bevy::prelude::World) {}
}

/// Marker for entities owned by a specific dynamic plugin.
///
/// Tag any entity spawned by a plugin with `ScriptOwned { plugin: PLUGIN_NAME }`
/// (the [`add!`] macro generates `PLUGIN_NAME`). On hot-reload, the host's
/// teardown shim despawns every entity carrying a matching `ScriptOwned`,
/// which calls the dylib's component drop fns while its code is still mapped
/// and prevents the new dylib from inheriting orphaned entities whose
/// `ComponentId`s belong to the old load.
#[derive(Component, Clone, Debug)]
pub struct ScriptOwned {
    pub plugin: &'static str,
}

impl ScriptOwned {
    pub const fn new(plugin: &'static str) -> Self {
        Self { plugin }
    }
}

/// Tag a `SceneRoot` (or any entity that will have a mesh hierarchy) to have
/// the host attach a static trimesh collider once. The host removes the marker
/// after fulfilling it, so the component is a one-shot request.
#[derive(Component, Clone, Copy, Debug, Default)]
pub struct AutoCollider;

/// Exports a type implementing [`DynamicPlugin`] from a script dylib.
///
/// Generates `plugin_setup`, `plugin_teardown`, `plugin_name`, and
/// `plugin_abi_version` FFI entry points, plus a `pub const PLUGIN_NAME`
/// usable inside the plugin (e.g. for `ScriptOwned { plugin: PLUGIN_NAME }`).
///
/// All FFI bodies are wrapped in [`std::panic::catch_unwind`] so a plugin
/// panic becomes a non-zero return code instead of unwinding across the FFI
/// boundary (UB on Windows, crashes with STATUS_FATAL_USER_CALLBACK_EXCEPTION).
///
/// # Example
/// ```ignore
/// use bevy::prelude::*;
/// use engine_api::{add, DynamicPlugin, ScriptOwned, PLUGIN_NAME};
///
/// #[derive(Default)]
/// pub struct ExamplePlugin;
///
/// impl DynamicPlugin for ExamplePlugin {
///     fn setup(&self, world: &mut World, schedule: &mut Schedule) {
///         world.spawn((MyComponent::default(), ScriptOwned::new(PLUGIN_NAME)));
///         schedule.add_systems(hello);
///     }
/// }
///
/// add!(ExamplePlugin, "example");
/// ```
#[macro_export]
macro_rules! add {
    ($plugin_type:ty, $plugin_name:literal) => {
        /// Stable name for this plugin. Use with `ScriptOwned::new(PLUGIN_NAME)`.
        pub const PLUGIN_NAME: &str = $plugin_name;

        const __PLUGIN_NAME_C: &[u8] = ::std::concat!($plugin_name, "\0").as_bytes();

        #[unsafe(no_mangle)]
        pub extern "C" fn plugin_name() -> *const ::std::os::raw::c_char {
            __PLUGIN_NAME_C.as_ptr() as *const ::std::os::raw::c_char
        }

        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn plugin_setup(
            world_ptr: *mut ::std::ffi::c_void,
            schedule_ptr: *mut ::std::ffi::c_void,
        ) -> u32 {
            let result = ::std::panic::catch_unwind(::std::panic::AssertUnwindSafe(|| {
                let world = unsafe {
                    &mut *(world_ptr as *mut $crate::bevy::prelude::World)
                };

                // ABI sanity check: confirm Time's TypeId resolves through the
                // host world's registry. Catches the case where bevy_dylib.dll
                // was rebuilt without restarting the host, which would silently
                // give us wrong ComponentIds.
                let dll_time_tid = ::std::any::TypeId::of::<$crate::bevy::time::Time>();
                let cid = world.register_resource::<$crate::bevy::time::Time>();
                let has_resource = world.contains_resource::<$crate::bevy::time::Time>();
                ::std::eprintln!(
                    "[scripts] DLL Time TypeId={dll_time_tid:?} ComponentId={cid:?} resource_exists={has_resource}"
                );
                if !has_resource {
                    ::std::eprintln!(
                        "[scripts] TypeId mismatch: DLL's Time resource entity not found. \
                         The DLL was likely compiled against different bevy metadata. \
                         Rebuild the full workspace and restart."
                    );
                    return 2;
                }

                let schedule = unsafe {
                    &mut *(schedule_ptr as *mut $crate::bevy::ecs::schedule::Schedule)
                };
                let plugin = <$plugin_type as ::std::default::Default>::default();
                <$plugin_type as $crate::DynamicPlugin>::setup(&plugin, world, schedule);
                0
            }));
            match result {
                Ok(code) => code,
                Err(_) => 1,
            }
        }

        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn plugin_teardown(
            world_ptr: *mut ::std::ffi::c_void,
        ) -> u32 {
            let result = ::std::panic::catch_unwind(::std::panic::AssertUnwindSafe(|| {
                let world = unsafe {
                    &mut *(world_ptr as *mut $crate::bevy::prelude::World)
                };

                // User-defined cleanup runs first (custom resource/observer teardown).
                let plugin = <$plugin_type as ::std::default::Default>::default();
                <$plugin_type as $crate::DynamicPlugin>::teardown(&plugin, world);

                // Auto-despawn entities carrying ScriptOwned for this plugin.
                // Compare by string content (different dylibs may have
                // different static-string addresses for the same literal).
                let mut to_despawn: ::std::vec::Vec<$crate::bevy::prelude::Entity> =
                    ::std::vec::Vec::new();
                let mut query = world.query::<(
                    $crate::bevy::prelude::Entity,
                    &$crate::ScriptOwned,
                )>();
                for (entity, owned) in query.iter(world) {
                    if owned.plugin == PLUGIN_NAME {
                        to_despawn.push(entity);
                    }
                }
                for entity in to_despawn {
                    world.despawn(entity);
                }
                0
            }));
            match result {
                Ok(code) => code,
                Err(_) => 1,
            }
        }

        #[unsafe(no_mangle)]
        pub extern "C" fn plugin_abi_version() -> u32 {
            $crate::ENGINE_SCRIPTS_ABI_VERSION
        }
    };
}

// ---------------------------------------------------------------------------
// App states
// ---------------------------------------------------------------------------

#[derive(Default, Clone, Copy, PartialEq, Eq, Hash, Debug, States)]
pub enum AppState {
    MainMenu,
    InGame,
    #[default]
    Starting,
}

#[derive(Default, Clone, Copy, PartialEq, Eq, Hash, Debug, States)]
pub enum SimulationState {
    #[default]
    Running,
    Paused,
}

// ---------------------------------------------------------------------------
// Core components
// ---------------------------------------------------------------------------

/// Marker for the player entity.
#[derive(Component)]
pub struct Player;

/// Marker for the player's collider child entity.
#[derive(Component)]
pub struct PlayerCollider;

/// Marker for all in-game entities (despawned on exit).
#[derive(Component)]
pub struct GameEntity;

/// Player physics state.
#[derive(Component, Debug)]
pub struct CharacterState {
    pub velocity: Vec3,
    pub grounded: bool,
    pub ground_normal: Vec3,
}

impl Default for CharacterState {
    fn default() -> Self {
        Self {
            velocity: Vec3::ZERO,
            grounded: false,
            ground_normal: Vec3::Y,
        }
    }
}

/// Camera orientation (attached to the camera entity).
#[derive(Component, Debug, Default)]
pub struct PlayerCamera {
    pub yaw: f32,
    pub pitch: f32,
}

// ---------------------------------------------------------------------------
// Core resources
// ---------------------------------------------------------------------------

#[derive(Resource, Debug)]
pub struct SimulationConfig {
    pub speed: f32,
}

impl Default for SimulationConfig {
    fn default() -> Self {
        Self { speed: 1.0 }
    }
}

/// Stackable freeze on the player's input. While at least one holder is
/// registered, the engine suppresses mouse-look and movement input. Cursor
/// visibility/grab is a separate concern — manipulate `CursorOptions` directly.
///
/// Plugins should lock with their own `PLUGIN_NAME` and unlock with the same
/// key, so multiple holders compose cleanly (e.g. shop UI + cutscene).
#[derive(Resource, Default, Debug)]
pub struct InputLock {
    holders: HashSet<&'static str>,
}

impl InputLock {
    pub fn lock(&mut self, holder: &'static str) {
        self.holders.insert(holder);
    }

    pub fn unlock(&mut self, holder: &'static str) {
        self.holders.remove(holder);
    }

    pub fn is_locked(&self) -> bool {
        !self.holders.is_empty()
    }
}

// ---------------------------------------------------------------------------
// Settings
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CameraView {
    FirstPerson,
    ThirdPerson,
}

impl Default for CameraView {
    fn default() -> Self {
        CameraView::ThirdPerson
    }
}

impl std::fmt::Display for CameraView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CameraView::FirstPerson => write!(f, "First Person"),
            CameraView::ThirdPerson => write!(f, "Third Person"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MovementMode {
    FreeCam,
    FloorBased,
}

impl Default for MovementMode {
    fn default() -> Self {
        MovementMode::FloorBased
    }
}

impl std::fmt::Display for MovementMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MovementMode::FreeCam => write!(f, "FreeCam"),
            MovementMode::FloorBased => write!(f, "Floor-Based"),
        }
    }
}

#[derive(Resource, Debug)]
pub struct UserSettings {
    pub crosshair_visible: bool,
    pub movement_mode: MovementMode,
    pub mouse_sensitivity: f32,
    pub movement_speed: f32,
    pub edit_mode: bool,
    pub outline_visible: bool,
    pub camera_view: CameraView,
    pub third_person_distance: f32,
    pub edge_detection_enabled: bool,
    pub edge_depth_sensitivity: f32,
    pub edge_normal_sensitivity: f32,
    pub terrain_edit_mode: bool,
    pub brush_radius: f32,
    pub physics_debug: bool,
    pub use_lightmap: bool,
}

impl Default for UserSettings {
    fn default() -> Self {
        Self {
            crosshair_visible: false,
            movement_mode: MovementMode::default(),
            mouse_sensitivity: 0.002,
            movement_speed: 0.5,
            edit_mode: false,
            outline_visible: false,
            camera_view: CameraView::FirstPerson,
            third_person_distance: 20.0,
            edge_detection_enabled: true,
            edge_depth_sensitivity: 2.0,
            edge_normal_sensitivity: 0.1,
            terrain_edit_mode: false,
            brush_radius: 5.0,
            physics_debug: false,
            use_lightmap: true,
        }
    }
}

#[derive(Debug)]
pub struct SettingInfo {
    pub name: &'static str,
    pub keybind: &'static str,
    pub current_value: String,
}

impl UserSettings {
    pub fn get_settings_info(&self) -> Vec<SettingInfo> {
        vec![
            SettingInfo {
                name: "Crosshair",
                keybind: "Shift+T",
                current_value: if self.crosshair_visible { "ON" } else { "OFF" }.to_string(),
            },
            SettingInfo {
                name: "Outline",
                keybind: "Shift+O",
                current_value: if self.outline_visible { "ON" } else { "OFF" }.to_string(),
            },
            SettingInfo {
                name: "Movement Mode",
                keybind: "Shift+M",
                current_value: self.movement_mode.to_string(),
            },
            SettingInfo {
                name: "Edit Mode",
                keybind: "Shift+E",
                current_value: if self.edit_mode { "ON" } else { "OFF" }.to_string(),
            },
            SettingInfo {
                name: "Mouse Sensitivity",
                keybind: "",
                current_value: format!("{:.3}", self.mouse_sensitivity),
            },
            SettingInfo {
                name: "Movement Speed",
                keybind: "",
                current_value: format!("{:.2}", self.movement_speed),
            },
            SettingInfo {
                name: "Camera View",
                keybind: "Shift+V",
                current_value: self.camera_view.to_string(),
            },
            SettingInfo {
                name: "Edge Detection",
                keybind: "F2",
                current_value: if self.edge_detection_enabled {
                    "ON"
                } else {
                    "OFF"
                }
                .to_string(),
            },
            SettingInfo {
                name: "Terrain Edit",
                keybind: "Shift+G",
                current_value: if self.terrain_edit_mode { "ON" } else { "OFF" }.to_string(),
            },
            SettingInfo {
                name: "Physics Debug",
                keybind: "Shift+P",
                current_value: if self.physics_debug { "ON" } else { "OFF" }.to_string(),
            },
            SettingInfo {
                name: "Lighting",
                keybind: "Shift+L",
                current_value: if self.use_lightmap { "BAKED" } else { "UNLIT" }.to_string(),
            },
        ]
    }

    pub fn toggle_crosshair(&mut self) {
        self.crosshair_visible = !self.crosshair_visible;
    }

    pub fn toggle_outline(&mut self) {
        self.outline_visible = !self.outline_visible;
    }

    pub fn toggle_camera_view(&mut self) {
        self.camera_view = match self.camera_view {
            CameraView::FirstPerson => CameraView::ThirdPerson,
            CameraView::ThirdPerson => CameraView::FirstPerson,
        };
    }

    pub fn toggle_movement_mode(&mut self) {
        self.movement_mode = match self.movement_mode {
            MovementMode::FreeCam => MovementMode::FloorBased,
            MovementMode::FloorBased => MovementMode::FreeCam,
        };
    }

    pub fn toggle_edit_mode(&mut self) {
        self.edit_mode = !self.edit_mode;
    }

    pub fn toggle_edge_detection(&mut self) {
        self.edge_detection_enabled = !self.edge_detection_enabled;
    }

    pub fn toggle_terrain_edit_mode(&mut self) {
        self.terrain_edit_mode = !self.terrain_edit_mode;
        if self.terrain_edit_mode {
            self.edit_mode = false;
        }
    }

    pub fn toggle_physics_debug(&mut self) {
        self.physics_debug = !self.physics_debug;
    }

    pub fn toggle_use_lightmap(&mut self) {
        self.use_lightmap = !self.use_lightmap;
    }
}
