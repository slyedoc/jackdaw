//! Graph and state machine documents: a `.animgraph.bsn` / `.fsm.bsn` open as a scene tab.
//!
//! The document's entities are the source of truth. The canvas draws the graph that
//! [`graph_from_document`] / [`fsm_from_document`] build from it, rebuilt whenever the document
//! changes, and every canvas gesture is an undoable document edit: the inspector, undo, the
//! dirty marker and Save are the scene machinery's.

use std::any::TypeId;
use std::collections::HashMap;
use std::path::PathBuf;

use bevy::{
    prelude::*,
    reflect::{TypePath, std_traits::ReflectDefault},
};
use bevy_animation_graph::core::{
    animation_graph::{
        AnimationGraph, SourcePin, TargetPin,
        bsn::{
            AnimGraph, Link, LinkFrom, LinkTo, Links, NodePosition, graph_from_document, node_id,
        },
    },
    state_machine::high_level::{
        StateMachine,
        bsn::{AnimFsm, AnimState, AnimTransition, fsm_from_document, state_id},
    },
};
use jackdaw_bsn::{BsnApplyAssets, SceneBsnAst};
use uuid::Uuid;

use super::{ArmGraph, CanvasView, seed_fsm_layout, seed_layout};
use crate::commands::{CommandHistory, EditorCommand, SpawnEntity, sync_component_to_ast};
use crate::viewport_host::{ViewportMode, ViewportModeIntent, focus_viewport, set_viewport_mode};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DocKind {
    Graph,
    Fsm,
}

/// The active document, when it is a graph or a state machine.
pub(super) struct GraphDocument {
    pub kind: DocKind,
    /// The root entity: `AnimGraph` / `AnimFsm`, and on a graph the outputs' `Links`.
    pub root: Entity,
    /// Node (or state) id to its entity and name.
    pub nodes: HashMap<Uuid, (Entity, String)>,
    path: Option<PathBuf>,
    graph: Option<Handle<AnimationGraph>>,
    fsm: Option<Handle<StateMachine>>,
}

#[derive(Resource, Default)]
pub(super) struct ActiveGraphDoc(pub Option<GraphDocument>);

fn doc_kind(ast: &SceneBsnAst, root: Entity) -> Option<DocKind> {
    ast.component_type_paths(root).iter().find_map(|path| {
        match path.rsplit("::").next().unwrap_or(path) {
            "AnimGraph" => Some(DocKind::Graph),
            "AnimFsm" => Some(DocKind::Fsm),
            _ => None,
        }
    })
}

/// Follow the active document: rebuild its graph whenever it changes, and give the viewport
/// to the canvas while one is open.
pub(super) fn track_graph_document(
    mut commands: Commands,
    ast: Res<SceneBsnAst>,
    registry: Res<AppTypeRegistry>,
    server: Res<AssetServer>,
    scenes: Res<crate::scenes::Scenes>,
    intent: Option<Res<ViewportModeIntent>>,
    mut graphs: ResMut<Assets<AnimationGraph>>,
    mut fsms: ResMut<Assets<StateMachine>>,
    mut doc: ResMut<ActiveGraphDoc>,
    mut view: ResMut<CanvasView>,
) {
    let found = ast
        .roots
        .first()
        .and_then(|&root| Some((root, doc_kind(&ast, root)?, ast.ecs_for_ast(root)?)));
    let Some((root_ast, kind, root)) = found else {
        if doc.0.take().is_some() {
            view.graph = None;
            view.fsm = None;
            view.dirty = true;
            if intent.is_some_and(|i| i.mode == ViewportMode::Graph) {
                commands.queue(|world: &mut World| {
                    set_viewport_mode(world, ViewportMode::ThreeD, false);
                });
            }
        }
        return;
    };
    let path = scenes.tabs.get(scenes.active).and_then(|tab| tab.path.clone());
    let fresh = doc
        .0
        .as_ref()
        .is_none_or(|d| d.path != path || d.kind != kind || d.root != root);

    let mut nodes = HashMap::new();
    for child in ast.get_children_ast(root_ast) {
        let (Some(name), Some(entity)) = (ast.get_name(child), ast.ecs_for_ast(child)) else {
            continue;
        };
        let id = match kind {
            DocKind::Graph => node_id(name).uuid(),
            DocKind::Fsm => state_id(name).uuid(),
        };
        nodes.insert(id, (entity, name.to_string()));
    }

    // A node renamed in the inspector: carry its links (and transitions) to the new name.
    if !fresh && let Some(previous) = doc.0.as_ref() {
        let renames: Vec<(String, String)> = previous
            .nodes
            .values()
            .filter_map(|(entity, old)| {
                let new = nodes.values().find(|(e, _)| e == entity)?.1.clone();
                (new != *old).then(|| (old.clone(), new))
            })
            .collect();
        if !renames.is_empty() {
            commands.queue(move |world: &mut World| follow_renames(world, root, renames));
        }
    }

    let assets = BsnApplyAssets {
        server: &server,
        local: None,
    };
    let registry = registry.read();
    let previous = doc.0.take();
    let mut next = GraphDocument {
        kind,
        root,
        nodes,
        path,
        graph: previous.as_ref().and_then(|d| d.graph.clone()),
        fsm: previous.as_ref().and_then(|d| d.fsm.clone()),
    };
    if fresh {
        next.graph = None;
        next.fsm = None;
    }
    match kind {
        DocKind::Graph => match graph_from_document(&ast, root_ast, &registry, Some(&assets)) {
            Ok(graph) => {
                let handle = match next.graph.clone() {
                    Some(handle) => {
                        let _ = graphs.insert(handle.id(), graph.clone());
                        handle
                    }
                    None => graphs.add(graph.clone()),
                };
                if fresh {
                    seed_layout(&mut view, &graph);
                    commands.insert_resource(ArmGraph(handle.clone()));
                } else {
                    refresh_positions(&mut view, &graph);
                }
                view.graph = Some(handle.clone());
                view.fsm = None;
                next.graph = Some(handle);
            }
            Err(err) => warn!("animation graph document: {err}"),
        },
        DocKind::Fsm => match fsm_from_document(&ast, root_ast, &registry, Some(&assets)) {
            Ok(fsm) => {
                let handle = match next.fsm.clone() {
                    Some(handle) => {
                        let _ = fsms.insert(handle.id(), fsm.clone());
                        handle
                    }
                    None => fsms.add(fsm.clone()),
                };
                if fresh {
                    seed_fsm_layout(&mut view, &fsm);
                    let wrapper = graphs.add(fsm_preview_graph(handle.clone(), &fsm));
                    commands.insert_resource(ArmGraph(wrapper));
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
    view.clip = None;
    view.ragdoll = None;
    view.path = None;
    view.dirty = true;
    if fresh {
        commands.queue(|world: &mut World| focus_viewport(world, ViewportMode::Graph));
    }
    doc.0 = Some(next);
}

/// A graph that plays a state machine, so the preview can: its data inputs become the graph's
/// (number inputs default to zero, which is what gives them a slider), its pose the graph's.
fn fsm_preview_graph(handle: Handle<StateMachine>, fsm: &StateMachine) -> AnimationGraph {
    use bevy_animation_graph::builtin_nodes::fsm_node::FsmNode;
    use bevy_animation_graph::core::{
        animation_graph::GraphInputPin,
        animation_node::AnimationNode,
        context::spec_context::NodeInput,
        edge_data::{DataSpec, DataValue},
    };
    let mut graph = AnimationGraph::new();
    let node = AnimationNode::new("fsm", FsmNode::new(handle));
    let id = node.id;
    graph.add_node(node);
    for input in fsm.node_spec.sorted_inputs() {
        if let NodeInput::Data(pin, spec) = input {
            let input = GraphInputPin::Passthrough(pin.clone());
            graph.io_spec.add_input_data(input.clone(), spec);
            if spec == DataSpec::F32 {
                graph.set_default_data(input.clone(), DataValue::F32(0.0));
            }
            graph.add_edge(SourcePin::InputData(input), TargetPin::NodeData(id, pin));
        }
    }
    graph.io_spec.add_output_time();
    graph.io_spec.add_output_data("pose".into(), DataSpec::Pose);
    graph.add_edge(SourcePin::NodeData(id, "pose".into()), TargetPin::OutputData("pose".into()));
    graph.add_edge(SourcePin::NodeTime(id), TargetPin::OutputTime);
    graph
}

/// A graph that loops one clip, so the preview can play it. Root motion is extracted, so a
/// clip that travels plays in place.
pub(super) fn clip_preview_graph(
    clip: Handle<bevy_animation_graph::core::animation_clip::GraphClip>,
) -> AnimationGraph {
    use bevy::reflect::structs::GetField;
    use bevy_animation_graph::builtin_nodes::{clip_node::ClipNode, loop_node::LoopNode};
    use bevy_animation_graph::core::{
        animation_node::AnimationNode, edge_data::DataSpec, pose::RootMotionMode,
    };
    let mut clip_node = ClipNode::new(clip, None, None);
    if let Some(mode) = clip_node.get_field_mut::<RootMotionMode>("root_motion_mode") {
        *mode = RootMotionMode::GroundPlane;
    }
    let mut graph = AnimationGraph::new();
    let clip_node = AnimationNode::new("clip", clip_node);
    let looped = AnimationNode::new("loop", LoopNode::new(0.1));
    let (clip, looped_id) = (clip_node.id, looped.id);
    graph.add_node(clip_node);
    graph.add_node(looped);
    graph.io_spec.add_output_time();
    graph.io_spec.add_output_data("pose".into(), DataSpec::Pose);
    graph.add_edge(SourcePin::NodeData(clip, "pose".into()), TargetPin::NodeData(looped_id, "pose".into()));
    graph.add_edge(SourcePin::NodeTime(clip), TargetPin::NodeTime(looped_id, "time".into()));
    graph.add_edge(SourcePin::NodeData(looped_id, "pose".into()), TargetPin::OutputData("pose".into()));
    graph.add_edge(SourcePin::NodeTime(looped_id), TargetPin::OutputTime);
    graph
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

/// Undoable replace (or insert, or remove) of one component on a document entity, mirrored
/// into the document.
struct SetDocComponent<T: Component + Reflect + TypePath + Clone> {
    entity: Entity,
    before: Option<T>,
    after: Option<T>,
    label: String,
}

impl<T: Component + Reflect + TypePath + Clone> SetDocComponent<T> {
    fn apply(&self, world: &mut World, value: &Option<T>) {
        match value {
            Some(value) => {
                if let Ok(mut entity) = world.get_entity_mut(self.entity) {
                    entity.insert(value.clone());
                }
                sync_component_to_ast(world, self.entity, T::type_path(), value);
            }
            None => {
                if let Ok(mut entity) = world.get_entity_mut(self.entity) {
                    entity.remove::<T>();
                }
                if let Some(mut ast) = world.get_resource_mut::<SceneBsnAst>()
                    && let Some(node) = ast.ast_for(self.entity)
                {
                    ast.remove_component_patch(node, T::type_path());
                }
            }
        }
        if let Ok(mut entity) = world.get_entity_mut(self.entity) {
            entity.insert(crate::inspector::InspectorDirty);
        }
    }
}

impl<T: Component + Reflect + TypePath + Clone> EditorCommand for SetDocComponent<T> {
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

/// Rewrite every reference to a renamed node, folded into the rename's own undo entry so undo
/// and redo move both together.
fn follow_renames(world: &mut World, root: Entity, renames: Vec<(String, String)>) {
    let renamed = |name: &str| {
        renames
            .iter()
            .find(|(old, _)| old == name)
            .map(|(_, new)| new.clone())
    };
    let mut entities = vec![root];
    if let Some(children) = world.get::<Children>(root) {
        entities.extend(children.iter());
    }
    let mut fixes: Vec<Box<dyn EditorCommand>> = Vec::new();
    for entity in entities {
        if let Some(links) = world.get::<Links>(entity).cloned() {
            let mut after = links.clone();
            for link in &mut after.0 {
                match &mut link.1 {
                    LinkFrom::Node(node, _) | LinkFrom::NodeTime(node) => {
                        if let Some(new) = renamed(node) {
                            *node = new;
                        }
                    }
                    _ => {}
                }
            }
            if after.0 != links.0 {
                fixes.push(Box::new(SetDocComponent {
                    entity,
                    before: Some(links),
                    after: Some(after),
                    label: "Follow rename".into(),
                }));
            }
        }
        if let Some(t) = world.get::<AnimTransition>(entity).cloned() {
            let mut after = t.clone();
            if let Some(new) = renamed(&t.from) {
                after.from = new;
            }
            if let Some(new) = renamed(&t.to) {
                after.to = new;
            }
            if after.from != t.from || after.to != t.to {
                fixes.push(Box::new(SetDocComponent {
                    entity,
                    before: Some(t),
                    after: Some(after),
                    label: "Follow rename".into(),
                }));
            }
        }
        if let Some(fsm) = world.get::<AnimFsm>(entity).cloned()
            && let Some(new) = renamed(&fsm.start)
        {
            let mut after = fsm.clone();
            after.start = new;
            fixes.push(Box::new(SetDocComponent {
                entity,
                before: Some(fsm),
                after: Some(after),
                label: "Follow rename".into(),
            }));
        }
    }
    if fixes.is_empty() {
        return;
    }
    for fix in &mut fixes {
        fix.execute(world);
    }
    let mut history = world.resource_mut::<CommandHistory>();
    let mut commands: Vec<Box<dyn EditorCommand>> = history.undo_stack.pop().into_iter().collect();
    commands.extend(fixes);
    history.push_executed(Box::new(crate::commands::CommandGroup {
        commands,
        label: "Rename node".into(),
    }));
}

fn run(world: &mut World, command: Box<dyn EditorCommand>) {
    world.resource_scope(|world, mut history: Mut<CommandHistory>| {
        history.execute(command, world);
    });
}

fn set_component<T: Component + Reflect + TypePath + Clone>(
    world: &mut World,
    entity: Entity,
    after: Option<T>,
    label: impl Into<String>,
) {
    let before = world.get::<T>(entity).cloned();
    run(
        world,
        Box::new(SetDocComponent {
            entity,
            before,
            after,
            label: label.into(),
        }),
    );
}

/// What the active document is, when it is a graph or a state machine.
pub fn active_kind(world: &World) -> Option<DocKind> {
    world
        .get_resource::<ActiveGraphDoc>()
        .and_then(|doc| doc.0.as_ref().map(|d| d.kind))
}

/// The graph the open graph document builds, when one is open.
pub fn active_graph(world: &World) -> Option<Handle<AnimationGraph>> {
    with_doc(world, |doc| doc.graph.clone()).flatten()
}

/// The state machine the open state machine document builds, when one is open.
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
    let name = |id: &bevy_animation_graph::core::animation_graph::NodeId| {
        doc.nodes.get(&id.uuid()).map(|(_, name)| name.clone())
    };
    Some(match source {
        SourcePin::NodeData(id, pin) => LinkFrom::Node(name(id)?, pin.clone()),
        SourcePin::NodeTime(id) => LinkFrom::NodeTime(name(id)?),
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
    let mut links = world.get::<Links>(entity).cloned().unwrap_or_default();
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
        Some((
            doc.root,
            doc.nodes.get(&from)?.1.clone(),
            doc.nodes.get(&to)?.1.clone(),
        ))
    })
    .flatten() else {
        return;
    };
    let (root, from, to) = found;
    let name = format!("{from} -> {to}");
    spawn_child(world, root, name, "Add transition", move |world, entity| {
        world.entity_mut(entity).insert(AnimTransition {
            from: from.clone(),
            to: to.clone(),
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
    run(
        world,
        Box::new(SpawnEntity {
            spawned: None,
            label: label.into(),
            spawn_fn: Box::new(move |world: &mut World| {
                let entity = world.spawn((Name::new(name.clone()), ChildOf(root))).id();
                fill(world, entity);
                crate::scene_io::register_entity_in_ast(world, entity);
                entity
            }),
        }),
    );
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
