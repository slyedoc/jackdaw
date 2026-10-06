//! A `.animgraph.bsn` is held open beside the scene: canvas edits are edits of the held document
//! that undo on the scene's history and save to the graph's own file.

use crate::util;

use bevy::prelude::*;
use bevy_animation_graph::builtin_nodes::speed_node::SpeedNode;
use bevy_animation_graph::core::animation_graph::{
    AnimationGraph, SourcePin, TargetPin, bsn::node_id,
};
use jackdaw::animgraph::document;

const GRAPH: &str = r#"#walk
AnimGraph { outputs: [Time, Data("pose", Pose)] }
Links([Link(Data("pose"), Node(#loop, "pose"))])
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
    jackdaw::animgraph::open_graph(app.world_mut(), path);
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

    assert!(jackdaw::animgraph::held::save(app.world_mut()), "the graph saves");
    let saved = std::fs::read_to_string(&path).expect("read back");
    assert!(saved.contains("SpeedNode"), "{saved}");
    assert!(saved.contains("NodePosition(5.0, 6.0)"), "{saved}");
    // A moved node keeps one position, whether the file spelled the type short or in full.
    assert_eq!(saved.matches("NodePosition(").count(), 3, "{saved}");

    // The saved file is still a graph the runtime loader builds.
    use bevy_animation_graph::core::animation_graph::bsn::{graph_from_world, spawn_document};
    let registry = app.world().resource::<AppTypeRegistry>().clone();
    let server = app.world().resource::<AssetServer>().clone();
    let mut handles = |type_id, path| server.load_builder().load_erased(type_id, path);
    let (world, root) = spawn_document(&saved, "walk.animgraph.bsn", &registry.0, &server, &mut handles)
        .expect("the saved document spawns");
    let reloaded = graph_from_world(&world, root, &registry.read()).expect("and builds");
    assert_eq!(reloaded.nodes.len(), 3);
}

const MACHINE: &str = r#"#moves
AnimFsm { start: #idle, outputs: [Time, Data("pose", Pose)] }
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

/// Links name nodes by entity, so a renamed node keeps every link, and undo puts the name back.
#[test]
fn renaming_a_node_keeps_its_links() {
    use jackdaw::commands::CommandHistory;

    let mut app = util::editor_test_app();
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("walk.animgraph.bsn");
    std::fs::write(&path, GRAPH).expect("write graph");
    open(&mut app, &path);

    let looped = document::node_entity(app.world(), node_id("loop").uuid()).expect("loop node");
    edit(&mut app, |world| {
        document::rename(world, looped, "cycle");
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
    let project = tempfile::tempdir().expect("tempdir");
    let shown = project.path().join("assets/anim");
    std::fs::create_dir_all(&shown).unwrap();
    app.world_mut().insert_resource(jackdaw::project::ProjectRoot {
        root: project.path().to_path_buf(),
        config: default(),
    });
    app.update();
    app.world_mut()
        .resource_mut::<jackdaw::project_window::ProjectWindowState>()
        .current_directory = shown.clone();
    let _ = app
        .world_mut()
        .operator("animgraph.new_graph")
        .call()
        .expect("the Add entry's operator resolves");
    for _ in 0..3 {
        app.update();
    }
    assert!(shown.join("graph_1.animgraph.bsn").exists());
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
    app.world_mut().spawn(jackdaw::animgraph::graph_window_content());
    app.update();
    let over_the_graph_window = |app: &mut App| {
        for mut rel in app
            .world_mut()
            .query::<&mut bevy::ui::RelativeCursorPosition>()
            .iter_mut(app.world_mut())
        {
            rel.cursor_over = true;
        }
    };
    over_the_graph_window(&mut app);

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
    over_the_graph_window(&mut app);
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

/// A state machine node, and a state, name the document a double-click opens.
#[test]
fn a_machine_node_and_a_state_name_what_they_open() {
    use bevy_animation_graph::core::state_machine::high_level::bsn::state_id;
    let mut app = util::editor_test_app();
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("top.animgraph.bsn");
    std::fs::write(
        &path,
        GRAPH.replace("ClipNode { }", r#"FsmNode { fsm: "priest/priest.fsm.bsn" }"#),
    )
    .expect("write graph");
    open(&mut app, &path);
    assert_eq!(
        document::document_of_node(app.world(), node_id("clip").uuid()).as_deref(),
        Some("priest/priest.fsm.bsn")
    );
    assert!(document::document_of_node(app.world(), node_id("loop").uuid()).is_none());

    let fsm = tmp.path().join("moves.fsm.bsn");
    std::fs::write(&fsm, MACHINE.replace(
        "#run\n    AnimState { }",
        "#run\n    AnimState { graph: \"priest/run.animgraph.bsn\" }",
    ))
    .expect("write machine");
    open(&mut app, &fsm);
    assert_eq!(
        document::document_of_node(app.world(), state_id("run").uuid()).as_deref(),
        Some("priest/run.animgraph.bsn")
    );
}

/// Add > Character writes a character prefab holding the project's rig and a starter graph that
/// loops its idle clip, and opens the character.
#[test]
fn a_new_character_is_a_prefab_of_the_rig_with_a_starter_graph() {
    use jackdaw_api::prelude::*;

    let mut app = util::editor_test_app();
    app.world_mut()
        .spawn(jackdaw::layout::inspector_components_content(default()));
    let project = tempfile::tempdir().expect("tempdir");
    let assets = project.path().join("assets");
    std::fs::create_dir_all(assets.join("ual")).unwrap();
    std::fs::create_dir_all(assets.join("anim/ual")).unwrap();
    // A rig as aurora_files bakes one: bones named by a `Name` component, no root Transform.
    std::fs::write(
        assets.join("ual/Mannequin.bsn"),
        "#Mannequin\nbevy_ecs::hierarchy::Children [\n    bevy_ecs::name::Name(\"Armature\")\n    bevy_transform::components::transform::Transform\n    bevy_aurora::material::AuroraMaterial3d(\n        bevy_aurora::material::AuroraMaterial {\n            base_color: bevy_color::color::Color::LinearRgba(bevy_color::linear_rgba::LinearRgba { red: 0.8, green: 0.4, blue: 0.0, alpha: 1.0 }),\n        },\n    )\n]\n",
    )
    .unwrap();
    std::fs::write(assets.join("ual/Mannequin.skn.ron"), "()").unwrap();
    std::fs::write(assets.join("anim/ual/Crouch_Idle_Loop.anim.ron"), "()").unwrap();
    std::fs::write(assets.join("anim/ual/Idle_Loop.anim.ron"), "()").unwrap();
    app.world_mut().insert_resource(jackdaw::project::ProjectRoot {
        root: project.path().to_path_buf(),
        config: default(),
    });
    app.update();

    let _ = app
        .world_mut()
        .operator("animgraph.new_character")
        .call()
        .expect("the Add entry's operator resolves");
    for _ in 0..4 {
        app.update();
    }

    let character = std::fs::read_to_string(assets.join("character_1.bsn")).expect("written");
    assert!(character.contains(r#"skeleton: "ual/Mannequin.skn.ron""#), "{character}");
    assert!(character.contains(r#"graph: "character_1.animgraph.bsn""#), "{character}");
    assert!(character.contains(r#"IsA { source: "ual/Mannequin.bsn""#), "{character}");
    let graph = std::fs::read_to_string(assets.join("character_1.animgraph.bsn")).expect("written");
    assert!(graph.contains("anim/ual/Idle_Loop.anim.ron"), "{graph}");

    let ast = app.world().resource::<jackdaw_bsn::SceneBsnAst>();
    let root = ast.roots[0];
    assert!(
        ast.component_type_paths(root)
            .iter()
            .any(|path| path.ends_with("AnimationRig")),
        "the character opened, its root carrying the rig"
    );

    // Selected, the character shows its Playback card (in the Animation tab).
    let character = ast.ecs_for_ast(root).expect("the character is live");
    jackdaw::selection::select_only(app.world_mut(), character);
    for _ in 0..8 {
        app.update();
    }
    let titles: Vec<String> = app
        .world_mut()
        .query::<&Text>()
        .iter(app.world())
        .map(|t| t.0.clone())
        .collect();
    assert!(titles.iter().any(|t| t == "Playback"), "{titles:?}");

    // The rig is there under the character: its bones, by name, placed (a Transform all the way).
    let armature = app
        .world_mut()
        .query::<(Entity, &Name)>()
        .iter(app.world())
        .find_map(|(e, n)| (n.as_str() == "Armature").then_some(e))
        .expect("the rig's bones spawned, named");
    let instance = app.world().get::<ChildOf>(armature).expect("under the instance").parent();
    assert!(app.world().get::<Transform>(instance).is_some(), "the instance is placed");

    // A material written inline in the rig is a real material, not an empty handle.
    let material = app
        .world()
        .get::<bevy_aurora::material::AuroraMaterial3d>(armature)
        .expect("the bone carries its material")
        .0
        .clone();
    let material = app
        .world()
        .resource::<Assets<bevy_aurora::material::AuroraMaterial>>()
        .get(&material)
        .expect("the inline material was added to the store");
    assert_eq!(
        material.base_color,
        Color::LinearRgba(LinearRgba::new(0.8, 0.4, 0.0, 1.0))
    );
}

/// A graph held open is edited beside the scene: the scene's document and file never see it.
#[test]
fn a_held_graph_leaves_the_open_scene_alone() {
    let mut app = util::editor_test_app();
    let tmp = tempfile::tempdir().expect("tempdir");
    let scene = tmp.path().join("yard.bsn");
    std::fs::write(
        &scene,
        "bevy_ecs::hierarchy::Children [\n    #Crate\n    bevy_transform::components::transform::Transform\n]\n",
    )
    .expect("write scene");
    jackdaw::scenes::operators::scene_open_system(app.world_mut(), &scene);
    for _ in 0..3 {
        app.update();
    }
    let graph_path = tmp.path().join("walk.animgraph.bsn");
    std::fs::write(&graph_path, GRAPH).expect("write graph");
    open(&mut app, &graph_path);

    let (clip, looped) = (node_id("clip"), node_id("loop"));
    edit(&mut app, |world| {
        document::connect(
            world,
            SourcePin::NodeData(clip, "pose".into()),
            TargetPin::NodeData(looped, "pose".into()),
        );
    });
    assert_eq!(graph(&app).edges_inverted.len(), 2, "the held graph took the edit");

    let ast = app.world().resource::<jackdaw_bsn::SceneBsnAst>();
    let names: Vec<String> = ast
        .roots
        .iter()
        .flat_map(|&r| std::iter::once(r).chain(ast.descendants_of(r)))
        .filter_map(|n| ast.get_name(n).map(str::to_string))
        .collect();
    assert_eq!(names, ["Crate"], "the scene's document is the scene alone");

    assert!(jackdaw::scene_io::save_scene(app.world_mut()), "the scene saves");
    let saved = std::fs::read_to_string(&scene).expect("read back");
    assert!(!saved.contains("ClipNode") && !saved.contains("Links"), "{saved}");
}

/// An inspector edit on a held node lands in the held graph, and saves with it.
#[test]
fn an_inspector_edit_on_a_held_node_lands_in_the_graph() {
    use jackdaw_api::prelude::*;

    let mut app = util::editor_test_app();
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("walk.animgraph.bsn");
    std::fs::write(&path, GRAPH).expect("write graph");
    open(&mut app, &path);
    let looped = document::node_entity(app.world(), node_id("loop").uuid()).expect("loop node");
    jackdaw::selection::select_only(app.world_mut(), looped);
    let _ = app
        .world_mut()
        .operator("field.set")
        .param("entity", looped)
        .param("type_path", "bevy_animation_graph::builtin_nodes::LoopNode".to_string())
        .param("field", "interpolation_period".to_string())
        .param("value", "0.5".to_string())
        .call()
        .expect("field.set resolves");
    for _ in 0..3 {
        app.update();
    }
    assert!(jackdaw::animgraph::held::save(app.world_mut()));
    let saved = std::fs::read_to_string(&path).expect("read back");
    assert!(saved.contains("interpolation_period: 0.5"), "{saved}");
}

