use bevy::prelude::*;
use engine_api::{DynamicPlugin, ScriptOwned, add};

const CAFE_WIDTH: i32 = 20;
const CAFE_DEPTH: i32 = 12;

#[derive(Default)]
pub struct CafeSimPlugin;

impl DynamicPlugin for CafeSimPlugin {
    fn setup(&self, world: &mut World, schedule: &mut Schedule) {
        let scene: Handle<Scene> = world
            .resource::<AssetServer>()
            .load("cafe-sim/Cafe.glb#Scene0");

        let entity = world
            .spawn((
                SceneRoot(scene),
                Transform::from_xyz(0.0, 0.1, 0.0),
                ScriptOwned::new(PLUGIN_NAME),
            ))
            .id();

        info!("[{PLUGIN_NAME}] spawned cafe scene {entity:?}");

        schedule.add_systems(draw_floor_grid);
    }
}

fn draw_floor_grid(mut gizmos: Gizmos) {
    let half_w = CAFE_WIDTH as f32 * 0.5;
    let half_d = CAFE_DEPTH as f32 * 0.5;
    let y = 0.11;
    let color = Color::srgba(1.0, 1.0, 1.0, 0.35);

    for i in -CAFE_WIDTH / 2..=CAFE_WIDTH / 2 {
        let x = i as f32;
        gizmos.line(Vec3::new(x, y, -half_d), Vec3::new(x, y, half_d), color);
    }
    for i in -CAFE_DEPTH / 2..=CAFE_DEPTH / 2 {
        let z = i as f32;
        gizmos.line(Vec3::new(-half_w, y, z), Vec3::new(half_w, y, z), color);
    }
}

add!(CafeSimPlugin, "cafe-sim");
