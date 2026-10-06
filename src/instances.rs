//! Instances: an entity that inherits a `.bsn` file (`:"characters/priest.bsn"` in the file it
//! sits in), spawned by bevy's loader with the base's own entities under it.
//!
//! The base file is where its contents are edited; in a scene the instance root carries only
//! what differs (a Transform, say), which is all the writer saves. The base's own descendants are
//! read-only here.

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
pub type AuthoredUiSceneRoot =
    (With<UiSceneRoot>, Without<ChildOf>, Without<SceneBase>, With<SceneEntity>);

/// A UI scene root an instance brought into this document.
pub type ImportedUiSceneRoot =
    (With<UiSceneRoot>, Without<ChildOf>, With<SceneBase>, With<SceneEntity>);

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
    world.get_resource_or_init::<PendingInstances>().0.push(PendingInstance {
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
        .add_systems(Update, place_pending_instances)
        .add_observer(hold_base);
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
pub fn save_as_instance(world: &mut World, roots: &[Entity], file: &Path) -> Result<Entity, String> {
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
    std::fs::write(file, crate::scene_io::stamp::with_stamp(&text)).map_err(|err| err.to_string())?;
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
    let placed = world.get::<Transform>(instance).copied().unwrap_or_default();
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
    let placed = world.get::<Transform>(instance).copied().unwrap_or_default();
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
