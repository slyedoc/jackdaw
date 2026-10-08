use avian3d::environment::PhysicsEnvironment;
use bevy::{
    ecs::reflect::AppTypeRegistry, platform::collections::HashSet, prelude::*,
    reflect::std_traits::ReflectDefault, scene::AuthoredComponents,
};
use bevy_aurora::environment::Environment;

use crate::scenes::Scenes;

/// Part of the open scene: written on save, cleared with the scene. Everything a scene file
/// spawned carries it, and so does every entity the editor authors into the scene.
#[derive(Component, Default, Clone, Copy, Debug)]
pub struct SceneEntity;

/// A top-level entity of the tab whose environment this points at.
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq)]
#[relationship(relationship_target = SceneRoots)]
pub struct SceneRootOf(pub Entity);

/// A tab environment's top-level entities, in the order the outliner shows and a save writes them.
#[derive(Component, Default, Debug)]
#[relationship_target(relationship = SceneRootOf, linked_spawn)]
pub struct SceneRoots(Vec<Entity>);

impl SceneRoots {
    pub fn entities(&self) -> &[Entity] {
        &self.0
    }
}

/// A tab's environment: its sky, exposure and physics settings, and the root a save writes them
/// on. Editor-owned (scene operations pass it by), yet the inspector edits it.
#[derive(Component, Clone, Copy, Debug, Default)]
pub struct SceneEnvironment;

/// A UI root's own visibility while its tab is behind another.
#[derive(Component, Clone, Copy, Debug)]
struct ParkedVisibility(Visibility);

/// The active tab's environment, if a tab is open.
pub fn scene_environment(world: &World) -> Option<Entity> {
    let scenes = world.get_resource::<Scenes>()?;
    scenes.tabs.get(scenes.active)?.environment
}

/// The active tab's environment, spawned on first use.
pub fn ensure_scene_environment(world: &mut World) -> Option<Entity> {
    let scenes = world.get_resource::<Scenes>()?;
    let active = scenes.active;
    let tab = scenes.tabs.get(active)?;
    if let Some(entity) = tab.environment
        && world.get_entity(entity).is_ok()
    {
        return Some(entity);
    }
    // Saved as the file's root once anything on it is authored (its sky, exposure, gravity).
    let entity = world
        .spawn((
            Name::new("Environment"),
            Environment::default(),
            SceneEnvironment,
            Transform::IDENTITY,
            AuthoredComponents(HashSet::default()),
            crate::EditorEntity,
            crate::EditorHidden,
        ))
        .id();
    world.resource_mut::<Scenes>().tabs[active].environment = Some(entity);
    Some(entity)
}

/// A file's environment root onto the active tab's environment: the file's authored settings
/// replace the ones the tab had, and settings the file leaves out go back to their defaults. The
/// tab keeps its entity, so its render bit and everything parented to it stay put.
pub fn adopt_environment(world: &mut World, source: Entity) {
    let Some(target) = ensure_scene_environment(world) else {
        return;
    };
    let authored = world
        .get::<AuthoredComponents>(source)
        .map(|a| a.0.clone())
        .unwrap_or_default();
    let previous = world
        .get::<AuthoredComponents>(target)
        .map(|a| a.0.clone())
        .unwrap_or_default();
    let registry = world.resource::<AppTypeRegistry>().clone();
    let registry = registry.read();
    for type_id in previous.difference(&authored) {
        let Some(registration) = registry.get(*type_id) else {
            continue;
        };
        let (Some(component), Some(default)) = (
            registration.data::<ReflectComponent>(),
            registration.data::<ReflectDefault>(),
        ) else {
            continue;
        };
        let value = default.default();
        component.insert(
            &mut world.entity_mut(target),
            value.as_partial_reflect(),
            &registry,
        );
    }
    for type_id in &authored {
        let Some(component) = registry
            .get(*type_id)
            .and_then(|r| r.data::<ReflectComponent>())
        else {
            continue;
        };
        let Some(value) = component
            .reflect(world.entity(source))
            .and_then(|v| v.to_dynamic().ok())
        else {
            continue;
        };
        component.insert(&mut world.entity_mut(target), &*value, &registry);
    }
    drop(registry);
    world
        .entity_mut(target)
        .insert(AuthoredComponents(authored));
}

/// A bevy_ui root: lays out only without a parent, so it joins its tab unparented.
pub fn is_ui_root(world: &World, entity: Entity) -> bool {
    world.get::<Node>(entity).is_some()
}

/// The scene parent of `entity`: its `ChildOf` unless that is the tab environment.
pub fn scene_parent(world: &World, entity: Entity) -> Option<Entity> {
    let parent = world.get::<ChildOf>(entity)?.parent();
    world
        .get::<PhysicsEnvironment>(parent)
        .is_none()
        .then_some(parent)
}

/// Put the root `entity` at `index` among the active tab's roots, parented to its environment
/// unless it is a UI root.
pub fn place_root(world: &mut World, entity: Entity, index: usize) {
    let Some(tab_environment) = ensure_scene_environment(world) else {
        world.entity_mut(entity).remove::<ChildOf>();
        return;
    };
    let ui = is_ui_root(world, entity);
    let mut entity_mut = world.entity_mut(entity);
    entity_mut.remove::<SceneRootOf>();
    if ui {
        entity_mut.remove::<ChildOf>();
    } else if entity_mut.get::<ChildOf>().map(ChildOf::parent) != Some(tab_environment) {
        entity_mut.insert(ChildOf(tab_environment));
    }
    let len = world
        .get::<SceneRoots>(tab_environment)
        .map_or(0, |roots| roots.0.len());
    world
        .entity_mut(tab_environment)
        .insert_related::<SceneRootOf>(index.min(len), &[entity]);
}

/// Take `entity` and everything under it into the open scene. A top-level entity joins the
/// active tab's roots.
pub fn adopt_entity(world: &mut World, entity: Entity) {
    if world.get_entity(entity).is_ok()
        && scene_parent(world, entity).is_none()
        && world.get::<SceneRootOf>(entity).is_none()
        && !is_editor_owned(world, entity)
    {
        place_root(world, entity, usize::MAX);
    }
    let mut stack = vec![entity];
    while let Some(entity) = stack.pop() {
        if world.get_entity(entity).is_err() || is_editor_owned(world, entity) {
            continue;
        }
        world.entity_mut(entity).insert(SceneEntity);
        super::freeze_authored(world, entity);
        if let Some(children) = world.get::<Children>(entity) {
            stack.extend(children.iter());
        }
    }
}

fn is_editor_owned(world: &World, entity: Entity) -> bool {
    world.get::<crate::NonSerializable>(entity).is_some()
        || world.get::<crate::EditorEntity>(entity).is_some()
}

/// [`adopt_entity`] for several entities.
pub fn adopt_entities(world: &mut World, entities: &[Entity]) {
    for &entity in entities {
        adopt_entity(world, entity);
    }
}

fn tab_roots(world: &World, tab_environment: Entity) -> Vec<Entity> {
    world
        .get::<SceneRoots>(tab_environment)
        .map(|roots| roots.0.clone())
        .unwrap_or_default()
}

/// Mark or unmark a tab's whole subtree as the open scene; its UI roots show only while open.
pub fn set_tab_open(world: &mut World, tab_environment: Entity, open: bool) {
    let roots = tab_roots(world, tab_environment);
    let mut stack = roots.clone();
    while let Some(entity) = stack.pop() {
        if world.get_entity(entity).is_err() || is_editor_owned(world, entity) {
            continue;
        }
        if open {
            world.entity_mut(entity).insert(SceneEntity);
        } else {
            world.entity_mut(entity).remove::<SceneEntity>();
        }
        if let Some(children) = world.get::<Children>(entity) {
            stack.extend(children.iter());
        }
    }
    for root in roots {
        if !is_ui_root(world, root) {
            continue;
        }
        let mut root = world.entity_mut(root);
        if open {
            if let Some(ParkedVisibility(visibility)) = root.take::<ParkedVisibility>() {
                root.insert(visibility);
            }
        } else if root.get::<ParkedVisibility>().is_none() {
            let visibility = root.get::<Visibility>().copied().unwrap_or_default();
            root.insert((ParkedVisibility(visibility), Visibility::Hidden));
        }
    }
}

/// Despawn a tab's contents, then its environment.
pub fn despawn_tab_environment(world: &mut World, tab_environment: Entity) {
    for root in tab_roots(world, tab_environment) {
        if let Ok(entity) = world.get_entity_mut(root) {
            entity.despawn();
        }
    }
    if let Ok(entity) = world.get_entity_mut(tab_environment) {
        entity.despawn();
    }
}
