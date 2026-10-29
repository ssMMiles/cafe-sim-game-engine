use bevy::{pbr::ExtendedMaterial, prelude::*};
use engine_api::{
    DefaultToonLightmap, DynamicPlugin, ProbeVolumeImages, ScriptOwned, ToonMaterial, ToonSource,
    add,
};
use std::f32::consts::TAU;

#[derive(Default)]
pub struct CirclingCubePlugin;

#[derive(Component)]
struct CircleMover {
    center: Vec3,
    radius: f32,
    angular_speed: f32,
    phase: f32,
}

impl DynamicPlugin for CirclingCubePlugin {
    fn setup(&self, world: &mut World, schedule: &mut Schedule) {
        let mover = CircleMover {
            center: Vec3::new(100.0, 5.0, 0.0),
            radius: 25.0,
            angular_speed: TAU / 8.0,
            phase: 0.0,
        };
        let start = mover.center + Vec3::new(mover.radius, 0.0, 0.0);

        let mesh = world
            .resource_mut::<Assets<Mesh>>()
            .add(Cuboid::from_length(10.0));
        let lightmap = world.resource::<DefaultToonLightmap>().0.clone();
        let probes = world.resource::<ProbeVolumeImages>().clone();
        let material = world
            .resource_mut::<Assets<ExtendedMaterial<StandardMaterial, ToonMaterial>>>()
            .add(ExtendedMaterial {
                base: StandardMaterial {
                    base_color: Color::srgb(0.00, 1.0, 0.00),
                    ..default()
                },
                // A moving cube: lit from the probe volume where it is.
                extension: ToonMaterial::new(ToonSource::ProbeVolume, lightmap, &probes),
            });

        let entity = world
            .spawn((
                Mesh3d(mesh),
                MeshMaterial3d(material),
                Transform::from_translation(start),
                mover,
                ScriptOwned::new(PLUGIN_NAME),
            ))
            .id();
        info!("[{PLUGIN_NAME}] spawned circling cube {entity:?}");

        schedule.add_systems(orbit_cube);
    }
}

fn orbit_cube(time: Res<Time>, mut q: Query<(&CircleMover, &mut Transform)>) {
    let t = time.elapsed_secs();
    for (mover, mut transform) in &mut q {
        let angle = mover.phase + t * mover.angular_speed;
        let (sin, cos) = angle.sin_cos();
        transform.translation =
            mover.center + Vec3::new(cos * mover.radius, 0.0, sin * mover.radius);
        let tangent = Vec3::new(-sin, 0.0, cos);
        transform.look_to(tangent, Vec3::Y);
    }
}

add!(CirclingCubePlugin, "example2");
