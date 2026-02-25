use std::time::Duration;

use bevy::{gltf::GltfAssetLabel, prelude::*};

use crate::settings::{MovementMode, UserSettings};

use super::physics::CharacterState;
use super::simulation::Player;

const RUN_SPEED_THRESHOLD: f32 = 0.5;
const TRANSITION_DURATION: Duration = Duration::from_millis(200);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnimState {
    Idle,
    Running,
    Jumping,
}

#[derive(Component)]
pub struct CharacterAnimator {
    graph_handle: Handle<AnimationGraph>,
    idle_node: AnimationNodeIndex,
    run_node: AnimationNodeIndex,
    jump_node: AnimationNodeIndex,
    current_state: AnimState,
    /// The descendant entity that holds the `AnimationPlayer`. None until the scene is ready.
    anim_player_entity: Option<Entity>,
}

/// Runs once on entering InGame (after `setup`). Builds the animation graph and
/// attaches a `CharacterAnimator` to the player entity.
pub fn setup_character_animations(
    mut commands: Commands,
    asset_server: Res<AssetServer>,
    mut graphs: ResMut<Assets<AnimationGraph>>,
    player_query: Query<Entity, With<Player>>,
) {
    let Ok(player_entity) = player_query.single() else {
        return;
    };

    // Animation indices in robotv2.gltf: 0=Idle, 1=Jump, 2=Run
    let idle_clip = asset_server
        .load(GltfAssetLabel::Animation(0).from_asset("meshes/character/robotv2.gltf"));
    let jump_clip = asset_server
        .load(GltfAssetLabel::Animation(1).from_asset("meshes/character/robotv2.gltf"));
    let run_clip = asset_server
        .load(GltfAssetLabel::Animation(2).from_asset("meshes/character/robotv2.gltf"));

    let mut graph = AnimationGraph::new();
    let idle_node = graph.add_clip(idle_clip, 1.0, graph.root);
    let jump_node = graph.add_clip(jump_clip, 1.0, graph.root);
    let run_node = graph.add_clip(run_clip, 1.0, graph.root);
    let graph_handle = graphs.add(graph);

    commands.entity(player_entity).insert(CharacterAnimator {
        graph_handle,
        idle_node,
        run_node,
        jump_node,
        current_state: AnimState::Idle,
        anim_player_entity: None,
    });
}

/// Polls each frame until the scene is ready and an `AnimationPlayer` descendant
/// is found. Once found, attaches the graph and starts the idle animation.
pub fn init_animation_player(
    mut commands: Commands,
    mut player_query: Query<(Entity, &mut CharacterAnimator), With<Player>>,
    children: Query<&Children>,
    mut anim_players: Query<&mut AnimationPlayer>,
) {
    let Ok((player_entity, mut animator)) = player_query.single_mut() else {
        return;
    };

    if animator.anim_player_entity.is_some() {
        return;
    }

    for child in children.iter_descendants(player_entity) {
        if let Ok(mut player) = anim_players.get_mut(child) {
            let mut transitions = AnimationTransitions::new();
            transitions
                .play(&mut player, animator.idle_node, Duration::ZERO)
                .repeat();

            commands
                .entity(child)
                .insert(AnimationGraphHandle(animator.graph_handle.clone()))
                .insert(transitions);

            animator.anim_player_entity = Some(child);
            break;
        }
    }
}

pub fn pause_animations(mut anim_players: Query<&mut AnimationPlayer>) {
    for mut player in &mut anim_players {
        player.pause_all();
    }
}

pub fn resume_animations(mut anim_players: Query<&mut AnimationPlayer>) {
    for mut player in &mut anim_players {
        player.resume_all();
    }
}

/// Each frame: derives the desired animation state from physics and input, then
/// transitions the animation player if the state has changed.
pub fn update_character_animation(
    settings: Res<UserSettings>,
    mut player_query: Query<(&CharacterState, &mut CharacterAnimator), With<Player>>,
    mut anim_players: Query<(&mut AnimationPlayer, &mut AnimationTransitions)>,
) {
    let Ok((char_state, mut animator)) = player_query.single_mut() else {
        return;
    };
    let Some(anim_entity) = animator.anim_player_entity else {
        return;
    };
    let Ok((mut player, mut transitions)) = anim_players.get_mut(anim_entity) else {
        return;
    };

    let desired = if settings.movement_mode == MovementMode::FreeCam {
        // In freecam, physics doesn't run so velocity/grounded are unreliable — just idle.
        AnimState::Idle
    } else {
        let h_speed = Vec2::new(char_state.velocity.x, char_state.velocity.z).length();
        if !char_state.grounded {
            AnimState::Jumping
        } else if h_speed > RUN_SPEED_THRESHOLD {
            AnimState::Running
        } else {
            AnimState::Idle
        }
    };

    if desired == animator.current_state {
        return;
    }

    let new_node = match desired {
        AnimState::Idle => animator.idle_node,
        AnimState::Running => animator.run_node,
        AnimState::Jumping => animator.jump_node,
    };

    let active = transitions.play(&mut player, new_node, TRANSITION_DURATION);
    match desired {
        AnimState::Idle | AnimState::Running => { active.repeat(); }
        AnimState::Jumping => {} // one-shot by default
    }

    animator.current_state = desired;
}
