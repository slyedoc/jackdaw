//! A graph (or state machine) held open beside the live scene, the way an open material is.
//!
//! The held document is a second parsed document. Its nodes are spawned as live entities, tagged
//! [`HeldGraphNode`], so the canvas, the inspector and the reflection paths work on them as on
//! any entity. Every edit runs with the held document swapped into the scene document's place
//! ([`with_held`]), so the editor's own commands (spawn, register, set a component, delete) edit
//! it unchanged; [`HeldCommand`] does the same swap around undo and redo. Edits go on the scene
//! tab's history; Save writes the graph's own file.

use std::path::{Path, PathBuf};

use bevy::prelude::*;
use jackdaw_bsn::SceneBsnAst;

use crate::commands::EditorCommand;

/// Marks a held document's node entities, which are not part of the scene.
#[derive(Component)]
pub struct HeldGraphNode;

pub struct HeldDocument {
    /// The file, absolute.
    pub path: PathBuf,
    /// The file as an asset path, which is how characters name it.
    pub asset_path: String,
    /// The document, while it is not swapped in.
    ast: Option<SceneBsnAst>,
    pub dirty: bool,
}

#[derive(Resource, Default)]
pub struct HeldGraph(pub Option<HeldDocument>);

impl HeldGraph {
    /// The held document, when one is open and not swapped in.
    pub fn ast(&self) -> Option<&SceneBsnAst> {
        self.0.as_ref()?.ast.as_ref()
    }
}

/// Run `f` with the held document in the scene document's place, then put both back.
/// `None` when nothing is held.
pub fn with_held<R>(world: &mut World, f: impl FnOnce(&mut World) -> R) -> Option<R> {
    let held = world.resource_mut::<HeldGraph>().0.as_mut()?.ast.take()?;
    let live = world.remove_resource::<SceneBsnAst>();
    world.insert_resource(held);
    let result = f(world);
    let held = world
        .remove_resource::<SceneBsnAst>()
        .expect("the held document is where it was put");
    if let Some(live) = live {
        world.insert_resource(live);
    }
    if let Some(doc) = world.resource_mut::<HeldGraph>().0.as_mut() {
        doc.ast = Some(held);
    }
    Some(result)
}

/// A command on the held document: runs (and undoes) with it swapped in, and marks it unsaved.
pub struct HeldCommand {
    pub path: PathBuf,
    pub inner: Box<dyn EditorCommand>,
}

impl HeldCommand {
    fn run(&mut self, world: &mut World, undo: bool) {
        let held = world
            .resource::<HeldGraph>()
            .0
            .as_ref()
            .is_some_and(|doc| doc.path == self.path);
        if !held {
            warn!(
                "{} is no longer held open; the edit cannot be {}",
                self.path.display(),
                if undo { "undone" } else { "redone" }
            );
            return;
        }
        let inner = &mut self.inner;
        with_held(world, |world| {
            if undo {
                inner.undo(world);
            } else {
                inner.execute(world);
            }
        });
        if let Some(doc) = world.resource_mut::<HeldGraph>().0.as_mut() {
            doc.dirty = true;
        }
    }
}

impl EditorCommand for HeldCommand {
    fn execute(&mut self, world: &mut World) {
        self.run(world, false);
    }

    fn undo(&mut self, world: &mut World) {
        self.run(world, true);
    }

    fn description(&self) -> &str {
        self.inner.description()
    }
}

/// Whether `entity` is a node of the held document.
pub fn is_held(world: &World, entity: Entity) -> bool {
    world.get::<HeldGraphNode>(entity).is_some()
}

/// Hold `path` open beside the scene, replacing whatever was held (unsaved edits to it are
/// reported, not lost silently: they stay on disk as they were).
pub fn hold(world: &mut World, path: &Path) {
    let assets = world.resource::<super::AnimGraphRoot>().0.clone().unwrap_or_default();
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) => {
            warn!("{}: {err}", path.display());
            return;
        }
    };
    let ast = match jackdaw_bsn::parse_bsn(&text) {
        Ok(ast) => ast,
        Err(err) => {
            warn!("{}: {err:?}", path.display());
            return;
        }
    };
    release(world);
    let asset_path = path
        .strip_prefix(&assets)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/");
    world.resource_mut::<HeldGraph>().0 = Some(HeldDocument {
        path: path.to_path_buf(),
        asset_path,
        ast: Some(ast),
        dirty: false,
    });
    with_held(world, |world| {
        let entities = jackdaw_bsn::spawn_from_ast(world);
        jackdaw_bsn::apply_dirty_ast_patches(world);
        let mut all = Vec::new();
        let mut stack = entities;
        while let Some(entity) = stack.pop() {
            all.push(entity);
            if let Some(children) = world.get::<Children>(entity) {
                stack.extend(children.iter());
            }
        }
        for entity in all {
            world.entity_mut(entity).insert(HeldGraphNode);
        }
    });
}

/// Let the held document go: its entities despawn. Unsaved edits are dropped with a warning.
pub fn release(world: &mut World) {
    let Some(doc) = world.resource_mut::<HeldGraph>().0.take() else {
        return;
    };
    if doc.dirty {
        warn!("{} closed with unsaved edits", doc.path.display());
    }
    let held: Vec<Entity> = world
        .query_filtered::<Entity, With<HeldGraphNode>>()
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
    let Some(path) = world
        .resource::<HeldGraph>()
        .0
        .as_ref()
        .map(|doc| doc.path.clone())
    else {
        return false;
    };
    let dir = path.parent().map(Path::to_path_buf).unwrap_or_default();
    let text = with_held(world, |world| {
        crate::scene_io::save::emit_bsn_scene_for_file(world, &dir)
    });
    match text {
        Some(Ok(text)) => match std::fs::write(&path, crate::scene_io::stamp::with_stamp(&text)) {
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
        },
        Some(Err(err)) => {
            warn!("{}: {err}", path.display());
            false
        }
        None => false,
    }
}
