//! Instances: an entity that inherits a `.bsn` file (`:"characters/priest.bsn"` in the file it
//! sits in), spawned by bevy's loader with the base's own entities under it.
//!
//! The base file is where its contents are edited; in a scene an instance is placement only (its
//! Transform, Name and Visibility). Nothing inside an instance can be selected: a pick or a row
//! inside one selects the instance, and Edit Source (double-click, or the inspector's button)
//! opens the base in its own tab with the picked entity selected. Saving the base reloads it,
//! and every tab's instances of it respawn the next time that tab is in front.

use std::path::Path;

use bevy::{
    bsn::{BsnDocument, BsnNodeKind},
    bsn_asset::DynamicScene,
    prelude::*,
    scene::{SceneBase, ScenePatch, WorldSceneExt},
};
use jackdaw_scene_types::UiSceneRoot;

use crate::scene_io::SceneEntity;

/// A UI scene root this document authored itself, rather than one an instance brought in.
pub type AuthoredUiSceneRoot = (
    With<UiSceneRoot>,
    Without<ChildOf>,
    Without<SceneBase>,
    With<SceneEntity>,
);

/// A UI scene root an instance brought into this document.
pub type ImportedUiSceneRoot = (
    With<UiSceneRoot>,
    Without<ChildOf>,
    With<SceneBase>,
    With<SceneEntity>,
);

/// The instance root `entity` sits inside, when it belongs to a base's contents: read-only here.
pub fn inherited_by(world: &World, entity: Entity) -> Option<Entity> {
    let mut current = world.get::<ChildOf>(entity).map(ChildOf::parent);
    while let Some(ancestor) = current {
        if world.get::<SceneBase>(ancestor).is_some() {
            return Some(ancestor);
        }
        current = world.get::<ChildOf>(ancestor).map(ChildOf::parent);
    }
    None
}

/// Whether `entity` is part of an instance's inherited contents.
pub fn is_inherited(world: &World, entity: Entity) -> bool {
    inherited_by(world, entity).is_some()
}

/// The file an instance root inherits.
pub fn base_of(world: &World, entity: Entity) -> Option<String> {
    world.get::<SceneBase>(entity).map(|base| base.0.clone())
}

/// Spawn an instance of `base` (an asset path) at `transform`, under `parent` when given.
pub fn place_instance(
    world: &mut World,
    base: &str,
    transform: Transform,
    parent: Option<Entity>,
) -> Result<Entity, String> {
    let mut document = BsnDocument::new();
    let root = document.push_node(BsnNodeKind::Entity {
        name: None,
        name_span: None,
        base: Some(base.to_string()),
        base_span: None,
        patches: Vec::new(),
        relations: Vec::new(),
    });
    document.push_root(root);
    let registry = world.resource::<AppTypeRegistry>().clone();
    let _server = world.resource::<AssetServer>().clone();
    let mut handles = jackdaw_runtime::handle_provider(world);
    let scene = DynamicScene::from_document_with_handles(&document, base, &registry, &mut handles)
        .map_err(|err| format!("{err}"))?;
    let entity = world
        .spawn_scene(scene)
        .map_err(|err| format!("{base}: {err}"))?
        .insert(transform)
        .id();
    if let Some(parent) = parent {
        world.entity_mut(entity).insert(ChildOf(parent));
    }
    Ok(entity)
}

/// The asset path a file sits at under `assets`, with `/` separators.
pub fn asset_path_of(assets: &Path, file: &Path) -> String {
    file.strip_prefix(assets)
        .unwrap_or(file)
        .to_string_lossy()
        .replace('\\', "/")
}

/// Instances waiting for their base to load.
#[derive(Resource, Default)]
pub struct PendingInstances(Vec<PendingInstance>);

pub struct PendingInstance {
    base: Handle<bevy::scene::ScenePatch>,
    path: String,
    transform: Transform,
    parent: Option<Entity>,
    frames: u32,
    /// Run with the placed entity (selecting it, recording an undo entry), or why none was.
    then: Box<dyn FnOnce(&mut World, Result<Entity, String>) + Send + Sync>,
}

/// Place an instance of `base` once the base (and what it depends on) has loaded.
pub fn queue_instance(
    world: &mut World,
    base: &str,
    transform: Transform,
    parent: Option<Entity>,
    then: impl FnOnce(&mut World, Result<Entity, String>) + Send + Sync + 'static,
) {
    let handle = world.resource::<AssetServer>().load(base.to_string());
    world
        .get_resource_or_init::<PendingInstances>()
        .0
        .push(PendingInstance {
            base: handle,
            path: base.to_string(),
            transform,
            parent,
            frames: 0,
            then: Box::new(then),
        });
}

fn place_pending_instances(world: &mut World) {
    let Some(mut pending) = world.get_resource_mut::<PendingInstances>() else {
        return;
    };
    if pending.0.is_empty() {
        return;
    }
    let waiting = std::mem::take(&mut pending.0);
    let server = world.resource::<AssetServer>().clone();
    let mut still = Vec::new();
    for mut instance in waiting {
        instance.frames += 1;
        if server.load_state(&instance.base).is_failed() || instance.frames > 600 {
            (instance.then)(world, Err(format!("{} did not load", instance.path)));
            continue;
        }
        if !server.is_loaded_with_dependencies(&instance.base) {
            still.push(instance);
            continue;
        }
        match place_instance(world, &instance.path, instance.transform, instance.parent) {
            Err(err) if crate::scene_io::is_unresolved(&err) => still.push(instance),
            placed => (instance.then)(world, placed),
        }
    }
    world.resource_mut::<PendingInstances>().0.extend(still);
}

pub(crate) fn plugin(app: &mut App) {
    app.init_resource::<PendingInstances>()
        .init_resource::<HeldBases>()
        .init_resource::<BaseGenerations>()
        .init_resource::<PendingSourceSelect>()
        .add_systems(
            Update,
            (
                place_pending_instances,
                select_instances_not_their_insides,
                track_reloads,
                refresh_changed_instances,
                select_in_source,
            ),
        )
        .add_observer(hold_base);
}

/// The components an instance keeps of its own: where it is, what it is called, whether it shows.
pub const PLACEMENT: [&str; 3] = [
    "bevy_transform::components::transform::Transform",
    "bevy_ecs::name::Name",
    "bevy_camera::visibility::Visibility",
];

/// The instance `entity` belongs to in the open document: the outermost instance root among
/// itself and its ancestors. An instance nested in a base is part of that base.
pub fn document_instance_of(world: &World, entity: Entity) -> Option<Entity> {
    let mut found = None;
    let mut current = Some(entity);
    while let Some(at) = current {
        if world.get::<SceneBase>(at).is_some() {
            found = Some(at);
        }
        current = world.get::<ChildOf>(at).map(ChildOf::parent);
    }
    found
}

/// Whether `entity` is an instance placed in the open document (not one inside a base).
pub fn is_document_instance(world: &World, entity: Entity) -> bool {
    world.get::<SceneBase>(entity).is_some() && !is_inherited(world, entity)
}

/// The names from `instance` (exclusive) down to `entity`, which is how its base names it.
fn path_below(world: &World, instance: Entity, entity: Entity) -> Vec<String> {
    let mut names = Vec::new();
    let mut current = entity;
    while current != instance {
        names.push(
            world
                .get::<Name>(current)
                .map(|n| n.as_str().to_string())
                .unwrap_or_default(),
        );
        match world.get::<ChildOf>(current) {
            Some(parent) => current = parent.parent(),
            None => return Vec::new(),
        }
    }
    names.reverse();
    names
}

/// A selection reaching inside an instance becomes the instance: its insides are its base's.
fn select_instances_not_their_insides(world: &mut World) {
    let selected = world.resource::<crate::selection::Selection>().entities.clone();
    if !selected.iter().any(|&e| is_inherited(world, e)) {
        return;
    }
    let mut lifted: Vec<Entity> = Vec::new();
    for entity in selected {
        let target = if is_inherited(world, entity) {
            document_instance_of(world, entity).unwrap_or(entity)
        } else {
            entity
        };
        if !lifted.contains(&target) {
            lifted.push(target);
        }
    }
    crate::selection::select_many(world, &lifted);
}

/// Edit Source on `entity`: open its instance's base in a tab and select what was picked there.
pub fn edit_source(world: &mut World, entity: Entity) {
    let Some(instance) = document_instance_of(world, entity) else {
        return;
    };
    let path = path_below(world, instance, entity);
    let Some(base) = base_of(world, instance) else {
        return;
    };
    open_base(world, instance);
    world.resource_mut::<PendingSourceSelect>().0 = Some((base, path, 0));
}

/// An entity to select in a source tab once it has spawned: the base, the names down to it, and
/// frames waited.
#[derive(Resource, Default)]
struct PendingSourceSelect(Option<(String, Vec<String>, u32)>);

fn select_in_source(world: &mut World) {
    let Some((base, path, frames)) = world.resource::<PendingSourceSelect>().0.clone() else {
        return;
    };
    let active = world
        .get_resource::<crate::scenes::Scenes>()
        .and_then(|s| s.tabs.get(s.active))
        .and_then(|t| t.path.clone());
    let assets = world
        .get_resource::<crate::project::ProjectRoot>()
        .map(crate::project::ProjectRoot::assets_dir);
    let in_front = active
        .zip(assets)
        .is_some_and(|(tab, assets)| asset_path_of(&assets, &tab) == base);
    if frames > 120 {
        world.resource_mut::<PendingSourceSelect>().0 = None;
        return;
    }
    let found = in_front.then(|| find_in_document(world, &path)).flatten();
    match found {
        Some(entity) => {
            crate::selection::select_only(world, entity);
            world.resource_mut::<PendingSourceSelect>().0 = None;
        }
        None => world.resource_mut::<PendingSourceSelect>().0 = Some((base, path, frames + 1)),
    }
}

/// The entity at `path` in the open document: under its one root (a root-entity file), or from
/// its top-level entities (a scene).
fn find_in_document(world: &mut World, path: &[String]) -> Option<Entity> {
    let roots = crate::scene_io::scene_roots(world);
    let descend = |world: &World, from: Vec<Entity>, names: &[String]| {
        let mut level = from;
        let mut found = None;
        for name in names {
            let next = level
                .iter()
                .copied()
                .find(|&e| world.get::<Name>(e).is_some_and(|n| n.as_str() == name))?;
            found = Some(next);
            level = world
                .get::<Children>(next)
                .map(|c| c.iter().collect())
                .unwrap_or_default();
        }
        found
    };
    if path.is_empty() {
        return (roots.len() == 1).then(|| roots[0]);
    }
    if let [root] = roots[..] {
        let below: Vec<Entity> = world
            .get::<Children>(root)
            .map(|c| c.iter().collect())
            .unwrap_or_default();
        if let Some(found) = descend(world, below, path) {
            return Some(found);
        }
    }
    descend(world, roots, path)
}

/// How many times each base was saved this session, and how many of those saves its reload has
/// brought in: an instance behind the reloaded count respawns.
#[derive(Resource, Default)]
struct BaseGenerations {
    saved: bevy::platform::collections::HashMap<String, u32>,
    reloaded: bevy::platform::collections::HashMap<String, u32>,
}

/// A held base was reloaded: its instances may catch up to the saves it carries. A reload
/// announces itself as `Modified`; a `LoadedWithDependencies` right after a save is the old asset.
fn track_reloads(
    mut events: MessageReader<AssetEvent<ScenePatch>>,
    held: Res<HeldBases>,
    mut generations: ResMut<BaseGenerations>,
) {
    for event in events.read() {
        let AssetEvent::Modified { id } = event else {
            continue;
        };
        let Some(base) = held.0.iter().find(|(_, h)| h.id() == *id).map(|(b, _)| b.clone())
        else {
            continue;
        };
        if let Some(&saved) = generations.saved.get(&base) {
            generations.reloaded.insert(base, saved);
        }
    }
}

/// The generation of its base an instance was spawned from.
#[derive(Component, Clone, Copy)]
struct SeenGeneration(u32);

/// A file was saved: reload it as a base, and let the instances of it catch up.
pub fn note_saved(world: &mut World, file: &Path) {
    let Some(assets) = world
        .get_resource::<crate::project::ProjectRoot>()
        .map(crate::project::ProjectRoot::assets_dir)
    else {
        return;
    };
    let base = asset_path_of(&assets, file);
    if !world.resource::<HeldBases>().0.contains_key(&base) {
        return; // nothing inherits it
    }
    world.resource::<AssetServer>().reload(base.clone());
    *world
        .resource_mut::<BaseGenerations>()
        .saved
        .entry(base)
        .or_default() += 1;
}

/// Respawn the open document's instances whose base was saved since they spawned, once the
/// reloaded base is in.
fn refresh_changed_instances(world: &mut World) {
    if world.resource::<BaseGenerations>().saved.is_empty() {
        return;
    }
    let mut instances = world.query_filtered::<(Entity, &SceneBase, Option<&SeenGeneration>), With<SceneEntity>>();
    let mut stale = Vec::new();
    let mut unseen = Vec::new();
    for (entity, base, seen) in instances.iter(world) {
        let current = world
            .resource::<BaseGenerations>()
            .reloaded
            .get(&base.0)
            .copied()
            .unwrap_or(0);
        match seen {
            Some(seen) if seen.0 < current => stale.push((entity, base.0.clone(), current)),
            Some(_) => {}
            None => unseen.push((entity, current)),
        }
    }
    for (entity, generation) in unseen {
        world.entity_mut(entity).insert(SeenGeneration(generation));
    }
    for (instance, base, generation) in stale {
        if is_inherited(world, instance) {
            continue;
        }
        match revert_instance(world, instance) {
            Ok(fresh) => {
                world.entity_mut(fresh).insert(SeenGeneration(generation));
            }
            Err(err) => warn!("refresh {base}: {err}"),
        }
    }
}

/// The bases instances inherit, kept loaded: a save diffs each instance against its base.
#[derive(Resource, Default)]
struct HeldBases(bevy::platform::collections::HashMap<String, Handle<ScenePatch>>);

fn hold_base(
    insert: On<Insert<SceneBase>>,
    bases: Query<&SceneBase>,
    server: Res<AssetServer>,
    mut held: ResMut<HeldBases>,
) {
    let Ok(base) = bases.get(insert.entity) else {
        return;
    };
    if !held.0.contains_key(&base.0) {
        held.0.insert(base.0.clone(), server.load(base.0.clone()));
    }
}

/// Write `roots` to `file` (one root as itself, several under a wrapper) and put an instance of
/// it where the first root was. The roots' own placement stays on the instance.
pub fn save_as_instance(
    world: &mut World,
    roots: &[Entity],
    file: &Path,
) -> Result<Entity, String> {
    let &[first, ..] = roots else {
        return Err("nothing to save".into());
    };
    let location = crate::commands::HierarchyLocation::from_world(world, first);
    let placed = world.get::<Transform>(first).copied().unwrap_or_default();
    let settings = crate::scene_io::write_settings(world);
    let text = if let [root] = roots {
        world.entity_mut(*root).insert(Transform::IDENTITY);
        let text = bevy::bsn_asset::write_scene_text(world, *root, &settings);
        world.entity_mut(*root).insert(placed);
        text
    } else {
        bevy::bsn_asset::write_scene_roots_text(world, roots, &settings)
    }
    .map_err(|err| err.to_string())?;
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir).map_err(|err| err.to_string())?;
    }
    std::fs::write(file, crate::scene_io::stamp::with_stamp(&text))
        .map_err(|err| err.to_string())?;
    let base = crate::scene_io::asset_path_of(world, file);
    crate::commands::deselect_entities(world, roots);
    for &root in roots {
        crate::commands::despawn_scene_entity(world, root);
    }
    let instance = place_instance(world, &base, placed, location.parent)?;
    crate::scene_io::adopt_entity(world, instance);
    if location.parent.is_none() {
        crate::scene_io::place_root(world, instance, location.index);
    }
    Ok(instance)
}

/// Write an instance as a file of its own: the same base, with this instance's changes.
pub fn save_as_variant(world: &mut World, instance: Entity, file: &Path) -> Result<(), String> {
    let settings = crate::scene_io::write_settings(world);
    let placed = world
        .get::<Transform>(instance)
        .copied()
        .unwrap_or_default();
    world.entity_mut(instance).insert(Transform::IDENTITY);
    let text = bevy::bsn_asset::write_scene_text(world, instance, &settings);
    world.entity_mut(instance).insert(placed);
    let text = text.map_err(|err| err.to_string())?;
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir).map_err(|err| err.to_string())?;
    }
    std::fs::write(file, crate::scene_io::stamp::with_stamp(&text)).map_err(|err| err.to_string())
}

/// Throw away an instance's changes: spawn its base afresh in its place.
pub fn revert_instance(world: &mut World, instance: Entity) -> Result<Entity, String> {
    let base = base_of(world, instance).ok_or("not an instance")?;
    let location = crate::commands::HierarchyLocation::from_world(world, instance);
    let placed = world
        .get::<Transform>(instance)
        .copied()
        .unwrap_or_default();
    let name = world.get::<Name>(instance).cloned();
    crate::commands::deselect_entities(world, &[instance]);
    crate::commands::despawn_scene_entity(world, instance);
    let fresh = place_instance(world, &base, placed, location.parent)?;
    if let Some(name) = name {
        world.entity_mut(fresh).insert(name);
    }
    crate::scene_io::adopt_entity(world, fresh);
    if location.parent.is_none() {
        crate::scene_io::place_root(world, fresh, location.index);
    }
    Ok(fresh)
}

/// Make an instance's contents the scene's own: it no longer inherits its base.
pub fn unbundle_instance(world: &mut World, instance: Entity) {
    world.entity_mut(instance).remove::<SceneBase>();
}

/// Open the file an instance inherits in a tab of its own.
pub fn open_base(world: &mut World, instance: Entity) {
    let Some(base) = base_of(world, instance) else {
        return;
    };
    let Some(assets) = world
        .get_resource::<crate::project::ProjectRoot>()
        .map(crate::project::ProjectRoot::assets_dir)
    else {
        return;
    };
    crate::scenes::operators::scene_open_system(world, &assets.join(base));
}
