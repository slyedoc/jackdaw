use avian3d::world::PhysicsWorld;
use bevy::prelude::*;

use crate::scenes::Scenes;

/// Part of the open scene: written on save, cleared with the scene. Everything a scene file
/// spawned carries it, and so does every entity the editor authors into the scene.
#[derive(Component, Default, Clone, Copy, Debug)]
pub struct SceneEntity;

/// A top-level entity of the tab whose world this points at.
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq)]
#[relationship(relationship_target = SceneRoots)]
pub struct SceneRootOf(pub Entity);

/// A tab world's top-level entities, in the order the outliner shows and a save writes them.
#[derive(Component, Default, Debug)]
#[relationship_target(relationship = SceneRootOf, linked_spawn)]
pub struct SceneRoots(Vec<Entity>);

impl SceneRoots {
    pub fn entities(&self) -> &[Entity] {
        &self.0
    }
}

/// A UI root's own visibility while its tab is behind another.
#[derive(Component, Clone, Copy, Debug)]
struct ParkedVisibility(Visibility);

/// The active tab's world, if a tab is open.
pub fn scene_world(world: &World) -> Option<Entity> {
    let scenes = world.get_resource::<Scenes>()?;
    scenes.tabs.get(scenes.active)?.world
}

/// The active tab's world, spawned on first use.
pub fn ensure_scene_world(world: &mut World) -> Option<Entity> {
    let scenes = world.get_resource::<Scenes>()?;
    let active = scenes.active;
    let tab = scenes.tabs.get(active)?;
    if let Some(entity) = tab.world
        && world.get_entity(entity).is_ok()
    {
        return Some(entity);
    }
    let name = format!("tab: {}", tab.display_name);
    let entity = world
        .spawn((
            Name::new(name),
            PhysicsWorld,
            Transform::IDENTITY,
            crate::EditorEntity,
            crate::EditorHidden,
        ))
        .id();
    world.resource_mut::<Scenes>().tabs[active].world = Some(entity);
    Some(entity)
}

/// A bevy_ui root: lays out only without a parent, so it joins its tab unparented.
pub fn is_ui_root(world: &World, entity: Entity) -> bool {
    world.get::<Node>(entity).is_some()
}

/// The scene parent of `entity`: its `ChildOf` unless that is the tab world.
pub fn scene_parent(world: &World, entity: Entity) -> Option<Entity> {
    let parent = world.get::<ChildOf>(entity)?.parent();
    world.get::<PhysicsWorld>(parent).is_none().then_some(parent)
}

/// Put the root `entity` at `index` among the active tab's roots, parented to its world
/// unless it is a UI root.
pub fn place_root(world: &mut World, entity: Entity, index: usize) {
    let Some(tab_world) = ensure_scene_world(world) else {
        world.entity_mut(entity).remove::<ChildOf>();
        return;
    };
    let ui = is_ui_root(world, entity);
    let mut entity_mut = world.entity_mut(entity);
    entity_mut.remove::<SceneRootOf>();
    if ui {
        entity_mut.remove::<ChildOf>();
    } else if entity_mut.get::<ChildOf>().map(ChildOf::parent) != Some(tab_world) {
        entity_mut.insert(ChildOf(tab_world));
    }
    let len = world
        .get::<SceneRoots>(tab_world)
        .map_or(0, |roots| roots.0.len());
    world
        .entity_mut(tab_world)
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

fn tab_roots(world: &World, tab_world: Entity) -> Vec<Entity> {
    world
        .get::<SceneRoots>(tab_world)
        .map(|roots| roots.0.clone())
        .unwrap_or_default()
}

/// Mark or unmark a tab's whole subtree as the open scene; its UI roots show only while open.
pub fn set_tab_open(world: &mut World, tab_world: Entity, open: bool) {
    let roots = tab_roots(world, tab_world);
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

/// Despawn a tab's contents, then its world.
pub fn despawn_tab_world(world: &mut World, tab_world: Entity) {
    for root in tab_roots(world, tab_world) {
        if let Ok(entity) = world.get_entity_mut(root) {
            entity.despawn();
        }
    }
    if let Ok(entity) = world.get_entity_mut(tab_world) {
        entity.despawn();
    }
}
