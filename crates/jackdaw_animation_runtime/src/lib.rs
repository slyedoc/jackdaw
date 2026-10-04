//! Skeletal animation for authored scenes: an [`AnimationRig`] plays a bevy_animation_graph
//! graph on the rig it sits on, and clip events fire as playback reaches them.
//!
//! Clips are baked `.animclip`s (aurora_files' `animlib_import`) addressing bones by their
//! name path from the rig root, so a clip baked against one armature drives any armature whose
//! bones carry the same names. The graph, its state machines and its clips are
//! bevy_animation_graph assets (`.animgraph.ron`, `.fsm.ron`, `.anim.ron`, `.skn.ron`), edited
//! in the editor's animation graph windows.

#![deny(missing_docs)]

use bevy::{
    animation::{AnimatedBy, AnimationTargetId},
    prelude::*,
};
use bevy_animation_graph::core::{
    animation_graph::AnimationGraph, animation_graph_player::AnimationGraphPlayer,
    skeleton::Skeleton,
};

pub mod events;

pub use events::{AnimationEvent, ClipEvent, ClipPass, ClipPlayhead, fire_clip_events};

/// Plays `graph` on the rig under this entity (typically a scene instance of the rig's
/// `.bsn`). Once the rig has spawned, every named node under it is bound by its name path from
/// this entity (the entity's own name is not part of the path) and an `AnimationGraphPlayer`
/// takes over. Changing either handle re-arms the player.
#[derive(Component, Reflect, Default, Clone, Debug)]
#[reflect(Component, Default, Clone)]
pub struct AnimationRig {
    /// The graph to play (`.animgraph.ron`).
    pub graph: Handle<AnimationGraph>,
    /// The skeleton the graph's clips were baked against (`.skn.ron`).
    pub skeleton: Handle<Skeleton>,
}

/// The rig's bones are bound; the player is armed for the handles it was armed with.
#[derive(Component)]
struct RigArmed {
    graph: AssetId<AnimationGraph>,
    skeleton: AssetId<Skeleton>,
}

/// Binds a spawned rig's bones by name path and (re)arms its player.
fn arm_rigs(
    mut commands: Commands,
    rigs: Query<(Entity, &AnimationRig, Option<&RigArmed>)>,
    children: Query<&Children>,
    names: Query<&Name>,
) {
    for (root, rig, armed) in &rigs {
        let current = (rig.graph.id(), rig.skeleton.id());
        if armed.is_some_and(|a| (a.graph, a.skeleton) == current) {
            continue;
        }
        let Ok(top) = children.get(root) else {
            continue; // the rig has not spawned yet
        };
        if armed.is_none() {
            let mut stack: Vec<(Entity, Vec<&str>)> = top.iter().map(|e| (e, Vec::new())).collect();
            while let Some((entity, mut path)) = stack.pop() {
                let Ok(name) = names.get(entity) else {
                    continue;
                };
                path.push(name.as_str());
                commands.entity(entity).insert((
                    AnimationTargetId::from_iter(path.iter().copied()),
                    AnimatedBy(root),
                ));
                if let Ok(kids) = children.get(entity) {
                    stack.extend(kids.iter().map(|k| (k, path.clone())));
                }
            }
        }
        commands.entity(root).insert((
            AnimationGraphPlayer::new(rig.skeleton.clone()).with_graph(rig.graph.clone()),
            RigArmed {
                graph: current.0,
                skeleton: current.1,
            },
        ));
    }
}

/// Plays [`AnimationRig`]s and fires clip events. bevy_animation_graph's own plugin comes with
/// aurora's default plugins.
pub struct AnimationRuntimePlugin;

impl Plugin for AnimationRuntimePlugin {
    fn build(&self, app: &mut App) {
        register_animation_types(app);
        app.add_message::<AnimationEvent>()
            .add_systems(Update, (arm_rigs, fire_clip_events));
    }
}

/// Registers the authored animation types for reflection. [`AnimationRuntimePlugin`] calls
/// this; call it directly only in an app that authors documents without playing them.
pub fn register_animation_types(app: &mut App) {
    app.register_type::<AnimationRig>()
        .register_type::<ClipEvent>();
}

#[cfg(test)]
mod tests {
    use bevy::asset::AssetPlugin;

    use super::*;

    #[test]
    fn a_spawned_rig_is_bound_by_name_path_and_armed() {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, AssetPlugin::default()))
            .add_systems(Update, arm_rigs);
        let rig = app
            .world_mut()
            .spawn((Name::new("Player"), AnimationRig::default()))
            .id();
        let armature = app
            .world_mut()
            .spawn((Name::new("Armature"), ChildOf(rig)))
            .id();
        let hips = app
            .world_mut()
            .spawn((Name::new("Hips"), ChildOf(armature)))
            .id();
        app.update();

        let world = app.world();
        assert!(world.get::<AnimationGraphPlayer>(rig).is_some());
        // Paths start below the rig: its own name is not part of them.
        assert_eq!(
            world.get::<AnimationTargetId>(hips),
            Some(&AnimationTargetId::from_iter(["Armature", "Hips"]))
        );
        assert_eq!(world.get::<AnimatedBy>(hips).map(|a| a.0), Some(rig));
    }

    #[test]
    fn a_rig_waits_for_its_scene() {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, AssetPlugin::default()))
            .add_systems(Update, arm_rigs);
        let rig = app.world_mut().spawn(AnimationRig::default()).id();
        app.update();
        assert!(app.world().get::<AnimationGraphPlayer>(rig).is_none());
    }
}
