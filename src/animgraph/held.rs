//! A graph (or state machine) held open beside the live scene, the way an open material is.
//!
//! Holding a file spawns it, with bevy's own `.bsn` loader, into the editor's world: one entity
//! per node under the document's root, each tagged [`HeldGraphNode`] so the scene neither lists
//! nor saves them. The canvas, the inspector and undo edit those entities directly, and Save
//! writes them back with the scene writer.

use std::path::{Path, PathBuf};

use bevy::{
    asset::AssetPath,
    bsn::BsnDocument,
    bsn_asset::{DynamicScene, WriteSettings, write_scene_text},
    ecs::entity_disabling::Disabled,
    prelude::*,
    scene::WorldSceneExt,
};

/// Marks a held document's entities, which are not part of the scene.
#[derive(Component)]
pub struct HeldGraphNode;

pub struct HeldDocument {
    /// The file, absolute.
    pub path: PathBuf,
    /// The file as an asset path, which is how characters name it.
    pub asset_path: String,
    /// The document's root entity.
    pub root: Entity,
    pub dirty: bool,
}

#[derive(Resource, Default)]
pub struct HeldGraph(pub Option<HeldDocument>);

impl HeldGraph {
    pub fn root(&self) -> Option<Entity> {
        self.0.as_ref().map(|doc| doc.root)
    }
}

/// Whether `entity` is a node of the held document.
pub fn is_held(world: &World, entity: Entity) -> bool {
    world.get::<HeldGraphNode>(entity).is_some()
}

/// The held document has edits its file does not.
pub fn mark_dirty(world: &mut World) {
    if let Some(doc) = world.resource_mut::<HeldGraph>().0.as_mut() {
        doc.dirty = true;
    }
}

/// Hold `path` open beside the scene, replacing whatever was held (unsaved edits to it are
/// reported: they stay on disk as they were).
pub fn hold(world: &mut World, path: &Path) {
    let assets = world
        .resource::<super::AnimGraphRoot>()
        .0
        .clone()
        .unwrap_or_default();
    let asset_path = path
        .strip_prefix(&assets)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/");
    let root = match spawn_file(world, path, &asset_path) {
        Ok(root) => root,
        Err(err) => {
            warn!("{}: {err}", path.display());
            return;
        }
    };
    release(world);
    let mut stack = vec![root];
    while let Some(entity) = stack.pop() {
        if let Some(children) = world.get::<Children>(entity) {
            stack.extend(children.iter());
        }
        world.entity_mut(entity).insert(HeldGraphNode);
    }
    world.resource_mut::<HeldGraph>().0 = Some(HeldDocument {
        path: path.to_path_buf(),
        asset_path,
        root,
        dirty: false,
    });
}

/// Spawn a graph file as entities, its handles loaded through the asset server.
fn spawn_file(world: &mut World, path: &Path, asset_path: &str) -> Result<Entity, String> {
    let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let document = BsnDocument::parse(&text).map_err(|e| format!("{e:?}"))?;
    let registry = world.resource::<AppTypeRegistry>().clone();
    let server = world.resource::<AssetServer>().clone();
    let mut handles = |type_id, path: AssetPath<'static>| server.load_builder().load_erased(type_id, path);
    let scene = DynamicScene::from_document_with_handles(&document, asset_path, &registry, &mut handles)
        .map_err(|e| e.render(&text))?;
    Ok(world.spawn_scene(scene).map_err(|e| format!("{e:?}"))?.id())
}

/// Let the held document go: its entities despawn, including nodes an undo set aside. Unsaved
/// edits are dropped with a warning.
pub fn release(world: &mut World) {
    let Some(doc) = world.resource_mut::<HeldGraph>().0.take() else {
        return;
    };
    if doc.dirty {
        warn!("{} closed with unsaved edits", doc.path.display());
    }
    let held: Vec<Entity> = world
        .query_filtered::<Entity, (With<HeldGraphNode>, Allow<Disabled>)>()
        .iter(world)
        .collect();
    for entity in held {
        if let Ok(entity) = world.get_entity_mut(entity) {
            entity.despawn();
        }
    }
}

/// Write the held document to its file.
pub fn save(world: &mut World) -> bool {
    let Some((path, root)) = world
        .resource::<HeldGraph>()
        .0
        .as_ref()
        .map(|doc| (doc.path.clone(), doc.root))
    else {
        return false;
    };
    let text = match write_scene_text(world, root, &WriteSettings::default()) {
        Ok(text) => text,
        Err(err) => {
            warn!("{}: {err}", path.display());
            return false;
        }
    };
    match std::fs::write(&path, crate::scene_io::stamp::with_stamp(&text)) {
        Ok(()) => {
            if let Some(doc) = world.resource_mut::<HeldGraph>().0.as_mut() {
                doc.dirty = false;
            }
            info!("saved {}", path.display());
            true
        }
        Err(err) => {
            warn!("{}: {err}", path.display());
            false
        }
    }
}
