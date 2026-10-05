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
/// `.bsn`). Once the rig has spawned, its bones are bound by name path from the skeleton's root
/// bone, found anywhere below this entity, and an `AnimationGraphPlayer` takes over; without a
/// skeleton, paths run from this entity (its own name not part of them). Changing either handle
/// re-arms the player.
#[derive(Component, Reflect, Default, Clone, Debug)]
#[reflect(Component, Default, Clone)]
pub struct AnimationRig {
    /// The graph to play (`.animgraph.bsn`).
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
    skeletons: Res<Assets<Skeleton>>,
) {
    for (root, rig, armed) in &rigs {
        let current = (rig.graph.id(), rig.skeleton.id());
        if armed.is_some_and(|a| (a.graph, a.skeleton) == current) {
            continue;
        }
        let Ok(top) = children.get(root) else {
            continue; // the rig has not spawned yet
        };
        // The skeleton's root bone, by name: its paths start there, wherever it sits below.
        let bone_root = match skeletons.get(&rig.skeleton) {
            Some(skeleton) => skeleton
                .id_to_path(skeleton.root())
                .and_then(|path| path.parts.first().map(|name| name.as_str().to_string())),
            None if rig.skeleton.path().is_some() => continue, // still loading
            None => None,
        };
        let start: Vec<Entity> = match &bone_root {
            Some(bone) => {
                let mut found = None;
                let mut queue: std::collections::VecDeque<Entity> = top.iter().collect();
                while let Some(e) = queue.pop_front() {
                    if names.get(e).is_ok_and(|n| n.as_str() == bone) {
                        found = Some(e);
                        break;
                    }
                    if let Ok(kids) = children.get(e) {
                        queue.extend(kids.iter());
                    }
                }
                let Some(found) = found else {
                    continue; // the rig has not spawned its bones yet
                };
                vec![found]
            }
            None => top.iter().collect(),
        };
        if armed.is_none() {
            let mut stack: Vec<(Entity, Vec<&str>)> = start.into_iter().map(|e| (e, Vec::new())).collect();
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
            .init_asset::<Skeleton>()
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

    /// A rig wrapped deeper than the bone tree (character > rig instance > Armature) binds from
    /// the skeleton's root bone, so clip paths starting at `Armature` still land.
    #[test]
    fn bones_bind_from_the_skeletons_root_bone_wherever_it_sits() {
        use bevy_animation_graph::core::animation_clip::EntityPath;

        let mut app = App::new();
        app.add_plugins((MinimalPlugins, AssetPlugin::default()))
            .init_asset::<Skeleton>()
            .add_systems(Update, arm_rigs);
        let mut skeleton = Skeleton::default();
        let root = EntityPath::default().child("Armature");
        skeleton.add_bone(root.clone(), Transform::IDENTITY, Transform::IDENTITY);
        skeleton.set_root(root.id());
        let skeleton = app.world_mut().resource_mut::<Assets<Skeleton>>().add(skeleton);

        let character = app
            .world_mut()
            .spawn((
                Name::new("Character"),
                AnimationRig {
                    skeleton,
                    ..default()
                },
            ))
            .id();
        let instance = app
            .world_mut()
            .spawn((Name::new("Mannequin"), ChildOf(character)))
            .id();
        let armature = app
            .world_mut()
            .spawn((Name::new("Armature"), ChildOf(instance)))
            .id();
        let hips = app
            .world_mut()
            .spawn((Name::new("Hips"), ChildOf(armature)))
            .id();
        app.update();

        let world = app.world();
        assert_eq!(
            world.get::<AnimationTargetId>(hips),
            Some(&AnimationTargetId::from_iter(["Armature", "Hips"]))
        );
        assert!(world.get::<AnimationTargetId>(instance).is_none());
        assert!(world.get::<AnimationGraphPlayer>(character).is_some());
    }

    #[test]
    fn a_rig_waits_for_its_scene() {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, AssetPlugin::default()))
            .init_asset::<Skeleton>()
            .add_systems(Update, arm_rigs);
        let rig = app.world_mut().spawn(AnimationRig::default()).id();
        app.update();
        assert!(app.world().get::<AnimationGraphPlayer>(rig).is_none());
    }
}
