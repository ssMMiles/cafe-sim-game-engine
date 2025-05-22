use bevy::{log, prelude::*};

use crate::AppState;

mod freecam;
use freecam::{setup_camera, handle_movement};

pub struct SimulationPlugin;

#[derive(Default, Clone, Copy, PartialEq, Eq, Hash, Debug, States)]
pub enum SimulationState {
    #[default]
    Running,
    Paused,
}

#[derive(Resource, Debug)]
pub struct SimulationConfig {
    pub speed: f32,
}

impl Default for SimulationConfig {
    fn default() -> Self {
        SimulationConfig { speed: 1.0 }
    }
}

impl Plugin for SimulationPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(OnEnter(AppState::InGame), setup);
        app.add_systems(OnExit(AppState::InGame), exit);

        app.add_systems(
            FixedUpdate,
            (update).run_if(in_state(SimulationState::Running).and(in_state(AppState::InGame))),
        );

        app.add_systems(Update, handle_movement.run_if(in_state(AppState::InGame)));

        app.init_state::<SimulationState>();
        app.init_resource::<SimulationConfig>();
    }
}


fn setup(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    log::info!("Setting up the simulation environment");

    setup_camera(&mut commands);
    commands.insert_resource(SimulationConfig::default());

    let sphere = meshes.add(Sphere::default());

    // spawn a sphere at 0, 0, 0
    commands.spawn((
        Mesh3d(sphere),
        MeshMaterial3d(materials.add(StandardMaterial {
            base_color: Color::srgb(1.0, 0.1, 0.1),
            ..Default::default()
        })),
        Transform::from_translation(Vec3::new(0.0, 1.0, 0.0)),
        GlobalTransform::default(),
    ));

    // spawn a plane at 0, 0, 0
    commands.spawn((
        Mesh3d(meshes.add(Plane3d {
            normal: Dir3::Y,
            half_size: Vec2 { x: 100.0, y: 100.0 },
            ..Default::default()
        })),
        MeshMaterial3d(materials.add(StandardMaterial {
            base_color: Color::srgb(0.1, 0.1, 0.1),
            ..Default::default()
        })),
        Transform::from_translation(Vec3::new(0.0, 0.0, 0.0)),
        GlobalTransform::default(),
    ));
}


fn update(_commands: Commands, _simulation_state: ResMut<SimulationConfig>, _time: Res<Time>) {
    // Update the simulation state based on the elapsed time
    // simulation_state.update(time.delta_seconds());
}

fn exit(mut commands: Commands) {
    // Cleanup the simulation environment
    commands.remove_resource::<SimulationConfig>();
}
