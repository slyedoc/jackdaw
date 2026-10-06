//! The held graph or state machine as the canvas edits it.
//!
//! The held document's entities are the source of truth. [`track_graph_document`] rebuilds the
//! graph ([`graph_from_world`] / [`fsm_from_world`]) whenever one of them changes and puts it in
//! the asset store under the file's path, so every character playing it follows the edit. Every
//! canvas gesture is an undoable component change on those entities. Links and transitions name
//! nodes by entity, so a rename needs no follow-up.

use std::any::TypeId;
use std::collections::HashMap;
use std::sync::Arc;

use bevy::{
    ecs::{change_detection::Tick, entity_disabling::Disabled},
    prelude::*,
    reflect::std_traits::ReflectDefault,
};
use bevy_animation_graph::core::{
    animation_graph::{
        AnimationGraph, SourcePin, TargetPin,
        bsn::{AnimGraph, Link, LinkFrom, LinkTo, Links, NodePosition, graph_from_world, node_id},
    },
    state_machine::high_level::{
        StateMachine,
        bsn::{AnimFsm, AnimState, AnimTransition, fsm_from_world, state_id},
    },
};
use uuid::Uuid;

use super::super::held::{self, HeldGraph, HeldGraphNode};
use super::{CanvasView, seed_fsm_layout, seed_layout};
use crate::commands::{CommandGroup, CommandHistory, EditorCommand};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DocKind {
    Graph,
    Fsm,
}

/// The held document, as the canvas sees it.
pub(super) struct GraphDocument {
    pub kind: DocKind,
    /// The root entity: `AnimGraph` / `AnimFsm`, and on a graph the outputs' `Links`.
    pub root: Entity,
    /// Node (or state) id to its entity and name.
    pub nodes: HashMap<Uuid, (Entity, String)>,
    graph: Option<Handle<AnimationGraph>>,
    fsm: Option<Handle<StateMachine>>,
}

#[derive(Resource, Default)]
pub(super) struct ActiveGraphDoc(pub Option<GraphDocument>);

fn doc_kind(world: &World, root: Entity) -> Option<DocKind> {
    if world.get::<AnimGraph>(root).is_some() {
        Some(DocKind::Graph)
    } else if world.get::<AnimFsm>(root).is_some() {
        Some(DocKind::Fsm)
    } else {
        None
    }
}

/// The held document's entities in use: the root and its children (an undone node is detached).
fn document_entities(world: &World, root: Entity) -> Vec<Entity> {
    let mut entities = vec![root];
    if let Some(children) = world.get::<Children>(root) {
        entities.extend(children.iter());
    }
    entities
}

/// Whether any of `entities` changed a component (or lost one) since `since`.
fn changed_since(world: &World, entities: &[Entity], since: Tick, now: Tick) -> bool {
    entities.iter().any(|&entity| {
        let Ok(entity) = world.get_entity(entity) else {
            return true;
        };
        entity.archetype().components().iter().any(|&id| {
            entity
                .get_change_ticks_by_id(id)
                .is_some_and(|ticks| ticks.is_changed(since, now))
        })
    })
}

/// Follow the held document: rebuild its graph whenever it changes.
pub(super) fn track_graph_document(world: &mut World, mut last: Local<Option<(Entity, Tick)>>) {
    let now = world.change_tick();
    let Some(held) = world.resource::<HeldGraph>().0.as_ref().map(|doc| (doc.root, doc.asset_path.clone())) else {
        *last = None;
        if world.resource_mut::<ActiveGraphDoc>().0.take().is_some() {
            let mut view = world.resource_mut::<CanvasView>();
            view.graph = None;
            view.fsm = None;
            view.dirty = true;
        }
        return;
    };
    let (root, asset_path) = held;
    let entities = document_entities(world, root);
    let fresh = last.is_none_or(|(held_root, _)| held_root != root);
    if !fresh {
        let (_, since) = last.expect("checked above");
        if !changed_since(world, &entities, since, now) {
            return;
        }
    }
    *last = Some((root, now));
    let Some(kind) = doc_kind(world, root) else {
        return;
    };

    let mut nodes = HashMap::new();
    for &child in &entities[1..] {
        let Some(name) = world.get::<Name>(child) else {
            continue;
        };
        let id = match kind {
            DocKind::Graph => node_id(name.as_str()).uuid(),
            DocKind::Fsm => state_id(name.as_str()).uuid(),
        };
        nodes.insert(id, (child, name.as_str().to_string()));
    }

    let registry = world.resource::<AppTypeRegistry>().clone();
    let server = world.resource::<AssetServer>().clone();
    let previous = world.resource_mut::<ActiveGraphDoc>().0.take();
    let mut next = GraphDocument {
        kind,
        root,
        nodes,
        graph: previous.as_ref().and_then(|d| d.graph.clone()),
        fsm: previous.as_ref().and_then(|d| d.fsm.clone()),
    };
    match kind {
        DocKind::Graph => match graph_from_world(world, root, &registry.read()) {
            Ok(graph) => {
                // The asset characters load by this path: replacing it in place is what makes
                // every character playing the graph follow the edit.
                let handle = server.load::<AnimationGraph>(asset_path);
                let _ = world
                    .resource_mut::<Assets<AnimationGraph>>()
                    .insert(handle.id(), graph.clone());
                let mut view = world.resource_mut::<CanvasView>();
                if fresh {
                    seed_layout(&mut view, &graph);
                } else {
                    refresh_positions(&mut view, &graph);
                }
                view.graph = Some(handle.clone());
                view.fsm = None;
                next.graph = Some(handle);
            }
            Err(err) => warn!("animation graph document: {err}"),
        },
        DocKind::Fsm => match fsm_from_world(world, root) {
            Ok(fsm) => {
                let handle = server.load::<StateMachine>(asset_path);
                let _ = world
                    .resource_mut::<Assets<StateMachine>>()
                    .insert(handle.id(), fsm.clone());
                let mut view = world.resource_mut::<CanvasView>();
                if fresh {
                    seed_fsm_layout(&mut view, &fsm);
                } else {
                    for (id, pos) in &fsm.editor_metadata.states {
                        view.positions.insert(id.uuid(), *pos);
                    }
                }
                view.fsm = Some(handle.clone());
                view.graph = None;
                next.fsm = Some(handle);
            }
            Err(err) => warn!("state machine document: {err}"),
        },
    }
    let mut view = world.resource_mut::<CanvasView>();
    view.clip = None;
    view.ragdoll = None;
    view.path = None;
    view.dirty = true;
    world.resource_mut::<ActiveGraphDoc>().0 = Some(next);
}

/// Take the document's positions without moving the view.
fn refresh_positions(view: &mut CanvasView, graph: &AnimationGraph) {
    view.positions.clear();
    for (id, pos) in &graph.editor_metadata.node_positions {
        view.positions.insert(id.uuid(), *pos);
    }
    view.input_pos = graph.editor_metadata.input_position;
    view.output_pos = graph.editor_metadata.output_position;
}

// ------------------------------------------------------------------ edits

/// Undoable replace (or insert, or remove) of one component on a held entity.
struct SetHeldComponent<T: Component + Clone> {
    entity: Entity,
    before: Option<T>,
    after: Option<T>,
    label: String,
}

impl<T: Component + Clone> SetHeldComponent<T> {
    fn apply(&self, world: &mut World, value: &Option<T>) {
        let Ok(mut entity) = world.get_entity_mut(self.entity) else {
            return;
        };
        match value {
            Some(value) => {
                entity.insert(value.clone());
            }
            None => {
                entity.remove::<T>();
            }
        }
        entity.insert(crate::inspector::InspectorDirty);
        held::mark_dirty(world);
    }
}

impl<T: Component + Clone> EditorCommand for SetHeldComponent<T> {
    fn execute(&mut self, world: &mut World) {
        let after = self.after.clone();
        self.apply(world, &after);
    }

    fn undo(&mut self, world: &mut World) {
        let before = self.before.clone();
        self.apply(world, &before);
    }

    fn description(&self) -> &str {
        &self.label
    }
}

/// A node put into, or taken out of, the document. Taking it out detaches and disables it rather
/// than despawning it, so links and undo entries naming it stay valid.
struct PlaceHeldNode {
    entity: Entity,
    root: Entity,
    present: bool,
    label: String,
}

impl PlaceHeldNode {
    fn set(&self, world: &mut World, present: bool) {
        let Ok(mut entity) = world.get_entity_mut(self.entity) else {
            return;
        };
        if present {
            entity.remove::<Disabled>().insert(ChildOf(self.root));
        } else {
            entity.remove::<ChildOf>().insert(Disabled);
        }
        held::mark_dirty(world);
    }
}

impl EditorCommand for PlaceHeldNode {
    fn execute(&mut self, world: &mut World) {
        self.set(world, self.present);
    }

    fn undo(&mut self, world: &mut World) {
        self.set(world, !self.present);
    }

    fn description(&self) -> &str {
        &self.label
    }
}

/// Run an edit on the held document, as one undo entry on the scene tab's history.
fn run(world: &mut World, command: Box<dyn EditorCommand>) {
    if world.resource::<HeldGraph>().0.is_none() {
        return;
    }
    world.resource_scope(|world, mut history: Mut<CommandHistory>| {
        history.execute(command, world);
    });
}

fn set_component<T: Component + Clone>(
    world: &mut World,
    entity: Entity,
    after: Option<T>,
    label: impl Into<String>,
) {
    run(world, set_command(world, entity, after, label));
}

fn set_command<T: Component + Clone>(
    world: &World,
    entity: Entity,
    after: Option<T>,
    label: impl Into<String>,
) -> Box<dyn EditorCommand> {
    Box::new(SetHeldComponent {
        entity,
        before: world.get::<T>(entity).cloned(),
        after,
        label: label.into(),
    })
}

/// A field edit from the inspector on a held node, as an undo entry. `false` when `entity` is
/// not a held node.
pub fn commit_field(
    world: &mut World,
    entity: Entity,
    type_path: &str,
    field_path: &str,
    new_json: &serde_json::Value,
) -> bool {
    if !held::is_held(world, entity) {
        return false;
    }
    let old_json = field_json(world, entity, type_path, field_path);
    run(
        world,
        Box::new(SetHeldField {
            entity,
            type_path: type_path.to_string(),
            field_path: field_path.to_string(),
            before: old_json,
            after: new_json.clone(),
        }),
    );
    true
}

fn field_json(
    world: &World,
    entity: Entity,
    type_path: &str,
    field_path: &str,
) -> Option<serde_json::Value> {
    use bevy::reflect::GetPath;
    let registry = world.resource::<AppTypeRegistry>().read();
    let registration = registry.get_with_type_path(type_path)?;
    let component = registration
        .data::<ReflectComponent>()?
        .reflect(world.get_entity(entity).ok()?)?;
    let field = if field_path.is_empty() {
        component.as_partial_reflect()
    } else {
        component.reflect_path(field_path).ok()?
    };
    crate::inspector::reflect_fields::reflect_to_json(field, &registry)
}

/// One field of one component on a held entity, set from JSON as the inspector writes it.
struct SetHeldField {
    entity: Entity,
    type_path: String,
    field_path: String,
    before: Option<serde_json::Value>,
    after: serde_json::Value,
}

impl SetHeldField {
    fn apply(&self, world: &mut World, value: &serde_json::Value) {
        if world.get_entity(self.entity).is_err() {
            return;
        }
        crate::commands::apply_json_field_to_ecs(
            world,
            self.entity,
            &self.type_path,
            &self.field_path,
            value,
        );
        world
            .entity_mut(self.entity)
            .insert(crate::inspector::InspectorDirty);
        held::mark_dirty(world);
    }
}

impl EditorCommand for SetHeldField {
    fn execute(&mut self, world: &mut World) {
        let after = self.after.clone();
        self.apply(world, &after);
    }

    fn undo(&mut self, world: &mut World) {
        if let Some(before) = self.before.clone() {
            self.apply(world, &before);
        }
    }

    fn description(&self) -> &str {
        "Set field"
    }
}

/// What the held document is, when one is open.
pub fn active_kind(world: &World) -> Option<DocKind> {
    world
        .get_resource::<ActiveGraphDoc>()
        .and_then(|doc| doc.0.as_ref().map(|d| d.kind))
}

/// The graph the held graph document builds, when one is open.
pub fn active_graph(world: &World) -> Option<Handle<AnimationGraph>> {
    with_doc(world, |doc| doc.graph.clone()).flatten()
}

/// The state machine the held state machine document builds, when one is open.
pub fn active_fsm(world: &World) -> Option<Handle<StateMachine>> {
    with_doc(world, |doc| doc.fsm.clone()).flatten()
}

fn with_doc<R>(world: &World, f: impl FnOnce(&GraphDocument) -> R) -> Option<R> {
    world.get_resource::<ActiveGraphDoc>()?.0.as_ref().map(f)
}

/// The entity a canvas box stands for.
pub fn node_entity(world: &World, id: Uuid) -> Option<Entity> {
    with_doc(world, |doc| doc.nodes.get(&id).map(|(e, _)| *e)).flatten()
}

/// Select the document's root: the graph (or state machine) itself, its pins and settings.
pub fn select_root(world: &mut World) {
    if let Some(root) = with_doc(world, |doc| doc.root) {
        crate::selection::select_only(world, root);
    }
}

/// The clip a clip node plays, if the node is one and names a clip.
pub fn clip_of_node(
    world: &World,
    id: Uuid,
) -> Option<Handle<bevy_animation_graph::core::animation_clip::GraphClip>> {
    use bevy::reflect::structs::GetField;
    use bevy_animation_graph::builtin_nodes::clip_node::ClipNode;
    let node = world.get::<ClipNode>(node_entity(world, id)?)?;
    node.get_field::<Handle<bevy_animation_graph::core::animation_clip::GraphClip>>("clip")
        .filter(|handle| handle.path().is_some())
        .cloned()
}

/// The document a node points at, as an asset path: a state machine node's machine, a graph
/// node's graph, a state's graph.
pub fn document_of_node(world: &World, id: Uuid) -> Option<String> {
    use bevy::reflect::structs::GetField;
    use bevy_animation_graph::builtin_nodes::{fsm_node::FsmNode, graph_node::GraphNode};
    let entity = node_entity(world, id)?;
    if let Some(node) = world.get::<FsmNode>(entity) {
        return node.fsm.path().map(|p| p.path().to_string_lossy().into_owned());
    }
    if let Some(node) = world.get::<GraphNode>(entity) {
        return node
            .get_field::<Handle<AnimationGraph>>("graph")?
            .path()
            .map(|p| p.path().to_string_lossy().into_owned());
    }
    let state = world.get::<AnimState>(entity)?;
    state.graph.path().map(|p| p.path().to_string_lossy().into_owned())
}

pub fn select_node(world: &mut World, id: Uuid) {
    if let Some(entity) = node_entity(world, id) {
        crate::selection::select_only(world, entity);
    }
}

pub fn commit_node_position(world: &mut World, id: Uuid, at: Vec2) {
    let Some(entity) = node_entity(world, id) else {
        return;
    };
    if world.get::<NodePosition>(entity) == Some(&NodePosition(at.x, at.y)) {
        return;
    }
    set_component(world, entity, Some(NodePosition(at.x, at.y)), "Move node");
}

/// Move the graph's inputs (`inputs`) or outputs box.
pub fn commit_rail_position(world: &mut World, inputs: bool, at: Vec2) {
    let Some(root) = with_doc(world, |doc| doc.root) else {
        return;
    };
    let Some(mut graph) = world.get::<AnimGraph>(root).cloned() else {
        return;
    };
    let slot = if inputs {
        &mut graph.input_position
    } else {
        &mut graph.output_position
    };
    if *slot == at {
        return;
    }
    *slot = at;
    set_component(world, root, Some(graph), "Move graph pins");
}

fn link_from(doc: &GraphDocument, source: &SourcePin) -> Option<LinkFrom> {
    let entity =
        |id: &bevy_animation_graph::core::animation_graph::NodeId| Some(doc.nodes.get(&id.uuid())?.0);
    Some(match source {
        SourcePin::NodeData(id, pin) => LinkFrom::Node(entity(id)?, pin.clone()),
        SourcePin::NodeTime(id) => LinkFrom::NodeTime(entity(id)?),
        SourcePin::InputData(pin) => LinkFrom::Input(pin.clone()),
        SourcePin::InputTime(pin) => LinkFrom::InputTime(pin.clone()),
    })
}

/// The entity holding a target pin's `Links`, and the pin as a link names it.
fn link_to(doc: &GraphDocument, target: &TargetPin) -> Option<(Entity, LinkTo)> {
    Some(match target {
        TargetPin::NodeData(id, pin) => (doc.nodes.get(&id.uuid())?.0, LinkTo::Data(pin.clone())),
        TargetPin::NodeTime(id, pin) => (doc.nodes.get(&id.uuid())?.0, LinkTo::Time(pin.clone())),
        TargetPin::OutputData(pin) => (doc.root, LinkTo::Data(pin.clone())),
        TargetPin::OutputTime => (doc.root, LinkTo::Time(String::new())),
    })
}

/// The same input, whatever a root's time pin happens to be called.
fn same_input(a: &LinkTo, b: &LinkTo, on_root: bool) -> bool {
    match (a, b) {
        (LinkTo::Time(_), LinkTo::Time(_)) if on_root => true,
        _ => a == b,
    }
}

/// Wire `source` into `target`. An input takes one link, so wiring an occupied input replaces it.
pub fn connect(world: &mut World, source: SourcePin, target: TargetPin) {
    let Some(found) = with_doc(world, |doc| {
        Some((link_to(doc, &target)?, link_from(doc, &source)?, doc.root))
    })
    .flatten() else {
        return;
    };
    let ((entity, to), from, root) = found;
    let mut links = world
        .get::<Links>(entity)
        .cloned()
        .unwrap_or(Links(Vec::new()));
    links.0.retain(|link| !same_input(&link.0, &to, entity == root));
    links.0.push(Link(to, from));
    set_component(world, entity, Some(links), "Connect");
}

/// Cut the link into `target`.
pub fn cut(world: &mut World, target: TargetPin) {
    let Some(((entity, to), root)) =
        with_doc(world, |doc| Some((link_to(doc, &target)?, doc.root))).flatten()
    else {
        return;
    };
    let Some(mut links) = world.get::<Links>(entity).cloned() else {
        return;
    };
    let before = links.0.len();
    links.0.retain(|link| !same_input(&link.0, &to, entity == root));
    if links.0.len() == before {
        return;
    }
    let after = (!links.0.is_empty()).then_some(links);
    set_component(world, entity, after, "Disconnect");
}

/// A name no other node of the document has: `Blend`, then `Blend 2`, ...
fn unique_name(doc: &GraphDocument, base: &str) -> String {
    let taken = |name: &str| doc.nodes.values().any(|(_, n)| n == name);
    if !taken(base) {
        return base.to_string();
    }
    (2..)
        .map(|n| format!("{base} {n}"))
        .find(|name| !taken(name))
        .unwrap_or_else(|| base.to_string())
}

/// Add a node of a registered node type, default-built, at `at` (canvas coordinates).
pub fn add_node(world: &mut World, type_id: TypeId, label: &str, at: Vec2) {
    let Some((root, name)) = with_doc(world, |doc| (doc.root, unique_name(doc, label))) else {
        return;
    };
    spawn_child(world, root, name, format!("Add {label}"), move |world, entity| {
        let registry = world.resource::<AppTypeRegistry>().clone();
        let registry = registry.read();
        let (Some(component), Some(default)) = (
            registry.get_type_data::<ReflectComponent>(type_id),
            registry.get_type_data::<ReflectDefault>(type_id),
        ) else {
            return;
        };
        let value = default.default();
        component.insert(
            &mut world.entity_mut(entity),
            value.as_partial_reflect(),
            &registry,
        );
        world
            .entity_mut(entity)
            .insert(NodePosition(at.x, at.y));
    });
}

/// Add a state to a state machine document.
pub fn add_state(world: &mut World, at: Vec2) {
    let Some((root, name)) = with_doc(world, |doc| (doc.root, unique_name(doc, "state"))) else {
        return;
    };
    spawn_child(world, root, name, "Add state", move |world, entity| {
        world
            .entity_mut(entity)
            .insert((AnimState::default(), NodePosition(at.x, at.y)));
    });
}

/// Add a direct transition between two states.
pub fn add_transition(world: &mut World, from: Uuid, to: Uuid) {
    let Some(found) = with_doc(world, |doc| {
        Some((doc.root, doc.nodes.get(&from)?.clone(), doc.nodes.get(&to)?.clone()))
    })
    .flatten() else {
        return;
    };
    let (root, (from, from_name), (to, to_name)) = found;
    let name = format!("{from_name} -> {to_name}");
    spawn_child(world, root, name, "Add transition", move |world, entity| {
        world.entity_mut(entity).insert(AnimTransition {
            from,
            to,
            data: default(),
        });
    });
}

/// Spawn a named child of the document root as one undoable step.
fn spawn_child(
    world: &mut World,
    root: Entity,
    name: String,
    label: impl Into<String>,
    fill: impl Fn(&mut World, Entity) + Send + Sync + 'static,
) {
    let fill: Arc<dyn Fn(&mut World, Entity) + Send + Sync> = Arc::new(fill);
    let entity = world
        .spawn((Name::new(name), HeldGraphNode, Disabled))
        .id();
    fill(world, entity);
    run(
        world,
        Box::new(PlaceHeldNode {
            entity,
            root,
            present: true,
            label: label.into(),
        }),
    );
}

/// Take a node out of the held document, with the links that read from it and the transitions
/// that name it, as one undo entry. The start state stays: a machine needs one.
pub fn delete_node(world: &mut World, entity: Entity) -> bool {
    let Some(root) = world.resource::<HeldGraph>().root() else {
        return false;
    };
    if entity == root || !held::is_held(world, entity) {
        return false;
    }
    if world.get::<AnimFsm>(root).is_some_and(|fsm| fsm.start == entity) {
        warn!("the start state cannot be deleted; make another state the start first");
        return true;
    }
    let mut commands: Vec<Box<dyn EditorCommand>> = Vec::new();
    for other in document_entities(world, root) {
        if other == entity {
            continue;
        }
        if let Some(links) = world.get::<Links>(other) {
            let kept: Vec<Link> = links
                .0
                .iter()
                .filter(|link| !matches!(&link.1, LinkFrom::Node(e, _) | LinkFrom::NodeTime(e) if *e == entity))
                .cloned()
                .collect();
            if kept.len() != links.0.len() {
                let after = (!kept.is_empty()).then_some(Links(kept));
                commands.push(set_command(world, other, after, "Disconnect"));
            }
        }
        if world
            .get::<AnimTransition>(other)
            .is_some_and(|t| t.from == entity || t.to == entity)
        {
            commands.push(Box::new(PlaceHeldNode {
                entity: other,
                root,
                present: false,
                label: "Delete transition".into(),
            }));
        }
    }
    commands.push(Box::new(PlaceHeldNode {
        entity,
        root,
        present: false,
        label: "Delete node".into(),
    }));
    crate::selection::clear_selection_in_world(world);
    run(
        world,
        Box::new(CommandGroup {
            commands,
            label: "Delete node".into(),
        }),
    );
    true
}

/// Rename a held node (or state): its links follow, since they name it by entity.
pub fn rename(world: &mut World, entity: Entity, name: &str) -> bool {
    if !held::is_held(world, entity) {
        return false;
    }
    set_component(world, entity, Some(Name::new(name.to_string())), "Rename node");
    true
}

/// Make `entity` the state machine's start state.
pub fn set_start(world: &mut World, entity: Entity) {
    let Some(root) = world.resource::<HeldGraph>().root() else {
        return;
    };
    let Some(mut fsm) = world.get::<AnimFsm>(root).cloned() else {
        return;
    };
    if fsm.start == entity {
        return;
    }
    fsm.start = entity;
    set_component(world, root, Some(fsm), "Set start state");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_roots_time_output_is_one_input_whatever_it_is_called() {
        let a = LinkTo::Time(String::new());
        let b = LinkTo::Time("t".into());
        assert!(same_input(&a, &b, true));
        assert!(!same_input(&a, &b, false));
        assert!(!same_input(&LinkTo::Data("pose".into()), &a, true));
    }
}
