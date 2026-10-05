//! A `.animgraph.bsn` opens as a document: the viewport shows it as a node canvas, and canvas
//! edits are document edits that undo and save like any other.

use crate::util;

use bevy::prelude::*;
use bevy_animation_graph::builtin_nodes::speed_node::SpeedNode;
use bevy_animation_graph::core::animation_graph::{
    AnimationGraph, SourcePin, TargetPin, bsn::node_id,
};
use jackdaw::animgraph::document;
use jackdaw::migrate_dialog::request_open_with_conversion;
use jackdaw::viewport_host::{ViewportMode, ViewportModeIntent};

const GRAPH: &str = r#"#walk
AnimGraph { outputs: [Time, Data("pose", Pose)] }
Links([Link(Data("pose"), Node("loop", "pose"))])
Children [
    #clip
    ClipNode { }
    NodePosition(40.0, 40.0)
    --
    #loop
    LoopNode { interpolation_period: 0.1 }
    NodePosition(330.0, 40.0)
]
"#;

fn open(app: &mut App, path: &std::path::Path) {
    request_open_with_conversion(app.world_mut(), path);
    for _ in 0..3 {
        app.update();
    }
}

fn graph(app: &App) -> AnimationGraph {
    let handle = document::active_graph(app.world()).expect("a graph document is open");
    app.world()
        .resource::<Assets<AnimationGraph>>()
        .get(&handle)
        .expect("the document's graph is built")
        .clone()
}

fn edit(app: &mut App, f: impl FnOnce(&mut World)) {
    f(app.world_mut());
    app.update();
}

#[test]
fn a_graph_document_opens_on_the_canvas_and_edits_undo_and_save() {
    let mut app = util::editor_test_app();
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("walk.animgraph.bsn");
    std::fs::write(&path, GRAPH).expect("write graph");
    open(&mut app, &path);

    assert_eq!(
        app.world().resource::<ViewportModeIntent>().mode,
        ViewportMode::Graph
    );
    let built = graph(&app);
    assert_eq!(built.nodes.len(), 2);
    assert_eq!(built.edges_inverted.len(), 1);

    let (clip, looped) = (node_id("clip"), node_id("loop"));
    let wire = TargetPin::NodeData(looped, "pose".into());
    edit(&mut app, |world| {
        document::connect(world, SourcePin::NodeData(clip, "pose".into()), wire.clone());
    });
    assert_eq!(
        graph(&app).edges_inverted.get(&wire),
        Some(&SourcePin::NodeData(clip, "pose".into()))
    );

    edit(&mut app, |world| {
        world.resource_scope(|world, mut history: Mut<jackdaw::commands::CommandHistory>| {
            history.undo(world);
        });
    });
    assert!(graph(&app).edges_inverted.get(&wire).is_none(), "undo cuts the wire");

    edit(&mut app, |world| {
        document::add_node(
            world,
            std::any::TypeId::of::<SpeedNode>(),
            "SpeedNode",
            Vec2::new(10.0, 20.0),
        );
    });
    edit(&mut app, |world| {
        document::commit_node_position(world, clip.uuid(), Vec2::new(5.0, 6.0));
    });
    let built = graph(&app);
    assert_eq!(built.nodes.len(), 3);
    assert_eq!(
        built.editor_metadata.node_positions.get(&clip),
        Some(&Vec2::new(5.0, 6.0))
    );

    assert!(jackdaw::scene_io::save_scene(app.world_mut()), "the document saves");
    let saved = std::fs::read_to_string(&path).expect("read back");
    assert!(saved.contains("SpeedNode"), "{saved}");
    assert!(saved.contains("NodePosition(5.0, 6.0)"), "{saved}");
    // A moved node keeps one position, whether the file spelled the type short or in full.
    assert_eq!(saved.matches("NodePosition(").count(), 3, "{saved}");

    // The saved file is still a graph the runtime loader builds.
    let ast = jackdaw_bsn::parse_bsn(&saved).expect("saved text parses");
    let registry = app.world().resource::<AppTypeRegistry>().read();
    let reloaded = bevy_animation_graph::core::animation_graph::bsn::graph_from_document(
        &ast,
        ast.roots[0],
        &registry,
        None,
    )
    .expect("the saved document builds");
    assert_eq!(reloaded.nodes.len(), 3);
}

const MACHINE: &str = r#"#moves
AnimFsm { start: "idle", outputs: [Time, Data("pose", Pose)] }
Children [
    #idle
    AnimState { }
    NodePosition(0.0, 0.0)
    --
    #run
    AnimState { }
    NodePosition(330.0, 0.0)
]
"#;

#[test]
fn a_state_machine_document_wires_transitions_and_adds_states() {
    use bevy_animation_graph::core::state_machine::high_level::{StateMachine, bsn::state_id};

    let mut app = util::editor_test_app();
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("moves.fsm.bsn");
    std::fs::write(&path, MACHINE).expect("write machine");
    open(&mut app, &path);

    let fsm = |app: &App| {
        let handle = document::active_fsm(app.world()).expect("a state machine document is open");
        app.world()
            .resource::<Assets<StateMachine>>()
            .get(&handle)
            .expect("built")
            .clone()
    };
    assert_eq!(fsm(&app).states.len(), 2);

    edit(&mut app, |world| {
        document::add_transition(world, state_id("idle").uuid(), state_id("run").uuid());
    });
    edit(&mut app, |world| document::add_state(world, Vec2::new(0.0, 200.0)));
    let built = fsm(&app);
    assert_eq!(built.states.len(), 3);
    let transition = built.transitions.values().next().expect("one transition");
    assert_eq!(
        (transition.source, transition.target),
        (state_id("idle"), state_id("run"))
    );
}

/// Links name nodes, so renaming a node carries every link that names it, and undo puts both
/// back in one step.
#[test]
fn renaming_a_node_keeps_its_links() {
    use jackdaw::commands::{CommandHistory, SetBsnField};

    let mut app = util::editor_test_app();
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("walk.animgraph.bsn");
    std::fs::write(&path, GRAPH).expect("write graph");
    open(&mut app, &path);

    let looped = document::node_entity(app.world(), node_id("loop").uuid()).expect("loop node");
    edit(&mut app, |world| {
        world.resource_scope(|world, mut history: Mut<CommandHistory>| {
            history.execute(
                Box::new(SetBsnField {
                    entity: looped,
                    type_path: "bevy_ecs::name::Name".into(),
                    field_path: String::new(),
                    old_value: Some(jackdaw_bsn::BsnValue::String("loop".into())),
                    new_value: jackdaw_bsn::BsnValue::String("cycle".into()),
                    was_derived: false,
                }),
                world,
            );
        });
    });
    app.update();
    let output = TargetPin::OutputData("pose".into());
    assert_eq!(
        graph(&app).edges_inverted.get(&output),
        Some(&SourcePin::NodeData(node_id("cycle"), "pose".into())),
        "the graph output follows the renamed node"
    );

    edit(&mut app, |world| {
        world.resource_scope(|world, mut history: Mut<CommandHistory>| history.undo(world));
    });
    app.update();
    assert_eq!(
        graph(&app).edges_inverted.get(&output),
        Some(&SourcePin::NodeData(node_id("loop"), "pose".into())),
        "one undo puts the name and the link back"
    );
}

/// New Animation Graph / New State Machine write a starter file into the folder and open it.
#[test]
fn a_new_graph_and_a_new_state_machine_start_from_the_project_window() {
    use jackdaw_api::prelude::*;

    let mut app = util::editor_test_app();
    let tmp = tempfile::tempdir().expect("tempdir");
    for kind in ["graph", "fsm"] {
        // The way the Project window's menu calls it: through Commands.
        app.world_mut()
            .commands()
            .operator("animgraph.new")
            .param("path", tmp.path().to_string_lossy().into_owned())
            .param("kind", kind.to_string())
            .call();
        app.world_mut().flush();
        for _ in 0..3 {
            app.update();
        }
        if kind == "graph" {
            assert!(tmp.path().join("graph_1.animgraph.bsn").exists());
            assert!(graph(&app).nodes.is_empty(), "a new graph starts empty");
        } else {
            assert!(tmp.path().join("state_machine_1.fsm.bsn").exists());
            assert!(document::active_fsm(app.world()).is_some(), "it opened and built");
        }
        assert_eq!(
            app.world().resource::<ViewportModeIntent>().mode,
            ViewportMode::Graph
        );
    }
}

/// The Project window's menu row, activated, writes the graph (the path a click takes).
#[test]
fn the_project_windows_menu_row_makes_a_graph() {
    use bevy::ecs::world::CommandQueue;
    use bevy::feathers::controls::FeathersMenuPopup;
    use bevy::ui_widgets::Activate;

    let mut app = util::editor_test_app();
    let tmp = tempfile::tempdir().expect("tempdir");
    app.world_mut().insert_resource(jackdaw::project_window::MenuTarget {
        path: tmp.path().to_path_buf(),
    });
    let mut queue = CommandQueue::default();
    let mut commands = Commands::new(&mut queue, app.world());
    jackdaw_feathers::context_menu::spawn_context_menu(
        &mut commands,
        Vec2::ZERO,
        None,
        &[("project.new_anim_graph", "New Animation Graph")],
    );
    queue.apply(app.world_mut());
    app.update();
    let menu = app
        .world_mut()
        .query_filtered::<Entity, With<FeathersMenuPopup>>()
        .single(app.world())
        .expect("the menu");
    let row = app.world().get::<Children>(menu).expect("rows")[0];
    app.world_mut().trigger(Activate { entity: row });
    for _ in 0..3 {
        app.update();
    }
    assert!(tmp.path().join("graph_1.animgraph.bsn").exists());
}

/// Add > Animation Graph makes the graph in the folder the Project window is showing.
#[test]
fn the_add_menu_makes_a_graph_in_the_shown_folder() {
    use jackdaw_api::prelude::*;

    let mut app = util::editor_test_app();
    let tmp = tempfile::tempdir().expect("tempdir");
    app.world_mut()
        .resource_mut::<jackdaw::project_window::ProjectWindowState>()
        .current_directory = tmp.path().to_path_buf();
    let _ = app
        .world_mut()
        .operator("animgraph.new_graph")
        .call()
        .expect("the Add entry's operator resolves");
    for _ in 0..3 {
        app.update();
    }
    assert!(tmp.path().join("graph_1.animgraph.bsn").exists());
    assert!(document::active_graph(app.world()).is_some(), "and it opened");
}

/// Ctrl+A and the Add menu offer node types while a graph is open, and picking one adds it.
#[test]
fn the_add_picker_offers_nodes_in_a_graph_and_states_in_a_state_machine() {
    let mut app = util::editor_test_app();
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("walk.animgraph.bsn");
    std::fs::write(&path, GRAPH).expect("write graph");
    open(&mut app, &path);

    let items = jackdaw::add_entity_picker::collect_add_menu_items(app.world_mut());
    let clip = items
        .iter()
        .find(|item| item.label == "Clip")
        .expect("the clip node is offered");
    assert!(
        items.iter().all(|item| item.action.starts_with("op:animgraph.")),
        "a graph offers nodes, not entities"
    );
    app.world_mut().trigger(jackdaw_widgets::menu_bar::MenuAction {
        action: clip.action.clone(),
    });
    for _ in 0..3 {
        app.update();
    }
    assert_eq!(graph(&app).nodes.len(), 3, "the picked node was added");

    let fsm = tmp.path().join("moves.fsm.bsn");
    std::fs::write(&fsm, MACHINE).expect("write machine");
    open(&mut app, &fsm);
    let items = jackdaw::add_entity_picker::collect_add_menu_items(app.world_mut());
    assert_eq!(
        items.iter().map(|item| item.label.as_str()).collect::<Vec<_>>(),
        ["State"]
    );
}



/// A clip node names the clip a double-click opens on the timeline.
#[test]
fn a_clip_node_names_its_clip() {
    let mut app = util::editor_test_app();
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("walk.animgraph.bsn");
    std::fs::write(
        &path,
        GRAPH.replace("ClipNode { }", r#"ClipNode { clip: "anim/Walk.anim.ron" }"#),
    )
    .expect("write graph");
    open(&mut app, &path);
    let clip = document::clip_of_node(app.world(), node_id("clip").uuid()).expect("a clip");
    assert_eq!(
        clip.path().map(|p| p.path().to_string_lossy().into_owned()),
        Some("anim/Walk.anim.ron".to_string())
    );
    assert!(document::clip_of_node(app.world(), node_id("loop").uuid()).is_none());
}
