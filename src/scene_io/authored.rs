//! What a save writes: each scene entity's [`AuthoredComponents`]. A file records its own (the
//! loader fills the set from each node's patches); the editor freezes one for an entity it
//! makes, and adds to it whatever an edit changes.
//!
//! Derived state never enters a set: a component qualifies only if a scene could build it back
//! (reflected, with a default), the skip lists leave it to the writer, and, when another component
//! merely requires it, it differs from its default.

use std::any::TypeId;

use bevy::{
    ecs::{change_detection::Tick, component::ComponentId, reflect::AppTypeRegistry},
    platform::collections::HashSet,
    prelude::*,
    reflect::std_traits::ReflectDefault,
    scene::AuthoredComponents,
};
use jackdaw_commands::{OnComponentsEdited, components_edited};

use super::SceneEntity;

pub(super) fn plugin(app: &mut App) {
    app.insert_resource(OnComponentsEdited(mark_edited))
        .add_systems(Last, mark_recorded_edits);
}

/// The net under every edit recorded without a window of its own (made first, pushed after):
/// on a frame the history moved, what changed on scene entities since the last frame.
fn mark_recorded_edits(world: &mut World, mut last: Local<Option<(u64, Tick)>>) {
    let Some(edits) = world
        .get_resource::<jackdaw_commands::CommandHistory>()
        .map(jackdaw_commands::CommandHistory::edits)
    else {
        return;
    };
    let now = world.change_tick();
    if let Some((seen, since)) = *last
        && seen != edits
    {
        components_edited(world, since);
    }
    *last = Some((edits, now));
}

/// The component types a save leaves out, from the skip lists.
fn skipped_types(world: &World) -> HashSet<TypeId> {
    let registry = world.resource::<AppTypeRegistry>().read();
    registry
        .iter()
        .filter(|registration| {
            let path = registration.type_info().type_path_table().path();
            super::should_skip_component(path)
                || bevy::bsn_asset::DERIVED_COMPONENTS.contains(&path)
        })
        .map(|registration| registration.type_id())
        .collect()
}

/// `id` on `entity`, if it is something an author sets.
fn authorable(
    world: &World,
    entity: Entity,
    id: ComponentId,
    required: &HashSet<ComponentId>,
    skipped: &HashSet<TypeId>,
) -> Option<TypeId> {
    let type_id = world.components().get_info(id)?.type_id()?;
    if type_id == TypeId::of::<AuthoredComponents>() || skipped.contains(&type_id) {
        return None;
    }
    let registry = world.resource::<AppTypeRegistry>().read();
    let registration = registry.get(type_id)?;
    let reflect_component = registration.data::<ReflectComponent>()?;
    let default = registration.data::<ReflectDefault>()?;
    if required.contains(&id) {
        let value = reflect_component.reflect(world.get_entity(entity).ok()?)?;
        if default
            .default()
            .reflect_partial_eq(value.as_partial_reflect())
            .unwrap_or(false)
        {
            return None;
        }
    }
    Some(type_id)
}

/// The components on `entity` that other components on it require.
fn required_of(world: &World, entity: Entity) -> HashSet<ComponentId> {
    let Ok(entity_ref) = world.get_entity(entity) else {
        return HashSet::default();
    };
    let components = world.components();
    entity_ref
        .archetype()
        .components()
        .iter()
        .filter_map(|&id| components.get_info(id))
        .flat_map(|info| info.required_components().iter_ids().collect::<Vec<_>>())
        .collect()
}

fn component_ids(world: &World, entity: Entity) -> Vec<ComponentId> {
    world
        .get_entity(entity)
        .map(|entity| entity.archetype().components().to_vec())
        .unwrap_or_default()
}

/// A set the editor froze rather than one a file gave: until the edit that made the entity is
/// recorded, what it is built up with afterwards joins the set too.
#[derive(Component)]
struct FreshlyFrozen;

/// Give `entity` an authored set of what it holds now, unless it has one.
pub fn freeze_authored(world: &mut World, entity: Entity) {
    if world.get::<AuthoredComponents>(entity).is_some() {
        return;
    }
    let skipped = skipped_types(world);
    let set = authored_now(world, entity, &skipped);
    if let Ok(mut entity) = world.get_entity_mut(entity) {
        entity.insert((AuthoredComponents(set), FreshlyFrozen));
    }
}

fn authored_now(world: &World, entity: Entity, skipped: &HashSet<TypeId>) -> HashSet<TypeId> {
    let required = required_of(world, entity);
    component_ids(world, entity)
        .into_iter()
        .filter_map(|id| authorable(world, entity, id, &required, skipped))
        .collect()
}

/// Mark `type_id` authored on `entity`: an edit set it.
pub fn author(world: &mut World, entity: Entity, type_id: TypeId) {
    if world.get::<AuthoredComponents>(entity).is_none() {
        freeze_authored(world, entity);
    }
    if let Some(mut authored) = world.get_mut::<AuthoredComponents>(entity) {
        authored.0.insert(type_id);
    }
}

/// After an edit: what changed on a scene entity since `since` joins its authored set. An entity
/// whose set arrived during the edit (respawned from text) already holds what its file says.
fn mark_edited(world: &mut World, since: Tick) {
    let now = world.read_change_tick();
    let entities: Vec<Entity> = world
        .query_filtered::<Entity, With<SceneEntity>>()
        .iter(world)
        .collect();
    let skipped = skipped_types(world);
    let mut marks: Vec<(Entity, HashSet<TypeId>)> = Vec::new();
    let mut unset: Vec<Entity> = Vec::new();
    let mut fresh: Vec<Entity> = Vec::new();
    for entity in entities {
        let Ok(entity_ref) = world.get_entity(entity) else {
            continue;
        };
        if entity_ref.contains::<FreshlyFrozen>() {
            fresh.push(entity);
            continue;
        }
        match entity_ref.get_change_ticks::<AuthoredComponents>() {
            None => {
                unset.push(entity);
                continue;
            }
            Some(ticks) if ticks.added.is_newer_than(since, now) => continue,
            Some(_) => {}
        }
        let changed: Vec<ComponentId> = entity_ref
            .archetype()
            .components()
            .iter()
            .copied()
            .filter(|&id| {
                entity_ref
                    .get_change_ticks_by_id(id)
                    .is_some_and(|ticks| ticks.is_changed(since, now))
            })
            .collect();
        if changed.is_empty() {
            continue;
        }
        let required = required_of(world, entity);
        let set: HashSet<TypeId> = changed
            .into_iter()
            .filter_map(|id| authorable(world, entity, id, &required, &skipped))
            .filter(|type_id| {
                world
                    .get::<AuthoredComponents>(entity)
                    .is_some_and(|authored| !authored.0.contains(type_id))
            })
            .collect();
        if !set.is_empty() {
            marks.push((entity, set));
        }
    }
    for (entity, set) in marks {
        if let Some(mut authored) = world.get_mut::<AuthoredComponents>(entity) {
            authored.0.extend(set);
        }
    }
    for entity in fresh {
        let set = authored_now(world, entity, &skipped);
        if let Ok(mut entity) = world.get_entity_mut(entity) {
            entity.remove::<FreshlyFrozen>();
            if let Some(mut authored) = entity.get_mut::<AuthoredComponents>() {
                authored.0.extend(set);
            }
        }
    }
    for entity in unset {
        freeze_authored(world, entity);
    }
}
