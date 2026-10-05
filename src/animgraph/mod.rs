//! The animation graph editor: bevy_animation_graph's graphs, state machines, clips and
//! ragdolls, edited in two windows (the graph canvas and a live preview of the rig).

mod editor;

pub use editor::{AnimGraphRoot, build_graph_presentation, document};

use bevy::prelude::*;
use jackdaw_api::prelude::*;
use jackdaw_feathers::icons::Icon;

pub(crate) const GRAPH_WINDOW_ID: &str = "jackdaw.animation_graph";
pub(crate) const PREVIEW_WINDOW_ID: &str = "jackdaw.animation_preview";

pub(crate) fn plugin(app: &mut App) {
    editor::plugin(app);
    app.add_systems(Update, follow_the_project);
}

/// The editor browses, and saves under, the open project's assets.
fn follow_the_project(
    project: Option<Res<crate::project::ProjectRoot>>,
    mut root: ResMut<AnimGraphRoot>,
) {
    let assets = project
        .as_deref()
        .map(crate::project::ProjectRoot::assets_dir);
    if root.0 != assets {
        root.0 = assets;
    }
}

/// The animation graph editor's windows.
#[derive(Default)]
pub struct AnimationGraphExtension;

impl JackdawExtension for AnimationGraphExtension {
    fn id(&self) -> String {
        GRAPH_WINDOW_ID.to_string()
    }

    fn label(&self) -> String {
        "Animation Graph".to_string()
    }

    fn kind(&self) -> ExtensionKind {
        ExtensionKind::Builtin
    }

    fn register(&self, ctx: &mut ExtensionContext) {
        ctx.register_operator::<AnimgraphAddNodeOp>()
            .register_operator::<AnimgraphAddStateOp>();
        ctx.register_operator::<AnimgraphNewOp>()
            .register_operator::<AnimgraphNewGraphOp>()
            .register_operator::<AnimgraphNewFsmOp>();
        ctx.register_menu_entry::<AnimgraphNewGraphOp>(TopLevelMenu::Add)
            .register_menu_entry::<AnimgraphNewFsmOp>(TopLevelMenu::Add);
        ctx.register_window(
            WindowDescriptor::new(GRAPH_WINDOW_ID)
                .with_name("Animation Graph")
                .with_icon(Icon::Workflow.unicode())
                .with_default_area(DefaultArea::BottomDock)
                .with_priority(2)
                .with_build(|window| {
                    window.spawn(editor::graph_window_content());
                }),
        );
        ctx.register_window(
            WindowDescriptor::new(PREVIEW_WINDOW_ID)
                .with_name("Animation Preview")
                .with_icon(Icon::PersonStanding.unicode())
                .with_default_area(DefaultArea::BottomDock)
                .with_priority(3)
                .with_build(|window| {
                    window.spawn(editor::preview_window_content());
                }),
        );
    }
}

/// What a new graph starts as: the graph's pose and time outputs, and nothing feeding them.
const NEW_GRAPH: &str = "#{name}
AnimGraph {
    outputs: [Time, Data(\"pose\", Pose)],
    input_position: Vec2 { x: -240.0, y: 40.0 },
    output_position: Vec2 { x: 620.0, y: 40.0 },
}
";

/// What a new state machine starts as: one state, which a machine needs to start in.
const NEW_FSM: &str = "#{name}
AnimFsm { start: \"idle\", outputs: [Time, Data(\"pose\", Pose)] }
Children [
    #idle
    AnimState { }
    NodePosition(0.0, 0.0)
]
";

/// Make a starter animation graph or state machine in a folder, and open it.
#[operator(
    id = "animgraph.new",
    label = "New Animation Graph",
    description = "Make a starter animation graph (or state machine) in a folder and open it.",
    allows_undo = false,
    params(
        path(String, doc = "The folder to make it in."),
        kind(String, default = "graph", doc = "`graph` or `fsm`.")
    )
)]
pub fn animgraph_new(params: In<OperatorParameters>, mut commands: Commands) -> OperatorResult {
    let Some(path) = params.as_str("path").map(std::path::PathBuf::from) else {
        warn!("animgraph.new: no path given");
        return OperatorResult::Cancelled;
    };
    let fsm = params.as_str("kind") == Some("fsm");
    commands.queue(move |world: &mut World| create_and_open(world, &path, fsm));
    OperatorResult::Finished
}

/// Add > Animation Graph: a starter graph in the folder the Project window is showing.
#[operator(
    id = "animgraph.new_graph",
    label = "Animation Graph",
    description = "Make a starter animation graph in the folder the Project window shows, and open it.",
    allows_undo = false
)]
pub fn animgraph_new_graph(_: In<OperatorParameters>, mut commands: Commands) -> OperatorResult {
    commands.queue(|world: &mut World| create_in_shown_folder(world, false));
    OperatorResult::Finished
}

/// Add > State Machine: a starter state machine in the folder the Project window is showing.
#[operator(
    id = "animgraph.new_fsm",
    label = "State Machine",
    description = "Make a starter animation state machine in the folder the Project window shows, and open it.",
    allows_undo = false
)]
pub fn animgraph_new_fsm(_: In<OperatorParameters>, mut commands: Commands) -> OperatorResult {
    commands.queue(|world: &mut World| create_in_shown_folder(world, true));
    OperatorResult::Finished
}

fn create_in_shown_folder(world: &mut World, fsm: bool) {
    let folder = world
        .get_resource::<crate::project_window::ProjectWindowState>()
        .map(|state| state.current_directory.clone())
        .filter(|dir| dir.is_dir())
        .or_else(|| world.resource::<AnimGraphRoot>().0.clone());
    match folder {
        Some(folder) => create_and_open(world, &folder, fsm),
        None => warn!("animgraph: no project folder to make it in"),
    }
}

/// Write a starter file into `path` (a folder, or a file whose folder is meant) and open it.
fn create_and_open(world: &mut World, path: &std::path::Path, fsm: bool) {
    let folder = crate::definition_assets::resolve_project_path(world, path);
    let folder = if folder.is_dir() {
        folder
    } else {
        match folder.parent() {
            Some(parent) => parent.to_path_buf(),
            None => return,
        }
    };
    let (stem, extension, template) = if fsm {
        ("state_machine", "fsm.bsn", NEW_FSM)
    } else {
        ("graph", "animgraph.bsn", NEW_GRAPH)
    };
    let Some((name, file)) = (1..1000).find_map(|n| {
        let name = format!("{stem}_{n}");
        let file = folder.join(format!("{name}.{extension}"));
        (!file.exists()).then_some((name, file))
    }) else {
        return;
    };
    if let Err(err) = std::fs::write(&file, template.replace("{name}", &name)) {
        warn!("animgraph: {} cannot be written: {err}", file.display());
        return;
    }
    if let Some(mut state) = world.get_resource_mut::<crate::project_window::ProjectWindowState>() {
        state.rebuild();
    }
    crate::scenes::operators::scene_open_system(world, &file);
}

fn graph_open(world: &World) -> bool {
    document::active_kind(world) == Some(document::DocKind::Graph)
}

fn state_machine_open(world: &World) -> bool {
    document::active_kind(world) == Some(document::DocKind::Fsm)
}

/// Add a node to the open graph: where the canvas was right-clicked, else mid-view.
#[operator(
    id = "animgraph.add_node",
    label = "Add Node",
    description = "Add a node of a registered node type to the open animation graph.",
    allows_undo = false,
    is_available = graph_open,
    params(node(String, doc = "The node type's type path."))
)]
pub fn animgraph_add_node(params: In<OperatorParameters>, mut commands: Commands) -> OperatorResult {
    let Some(type_path) = params.as_str("node").map(str::to_string) else {
        warn!("animgraph.add_node: no node type given");
        return OperatorResult::Cancelled;
    };
    commands.queue(move |world: &mut World| {
        let registration = {
            let registry = world.resource::<AppTypeRegistry>().read();
            registry
                .get_with_type_path(&type_path)
                .map(|r| {
                    let short = r.type_info().type_path_table().short_path();
                    (r.type_id(), short.trim_end_matches("Node").to_string())
                })
        };
        let Some((type_id, label)) = registration else {
            warn!("animgraph.add_node: `{type_path}` is not a registered type");
            return;
        };
        let at = editor::take_drop_point(world);
        document::add_node(world, type_id, &label, at);
    });
    OperatorResult::Finished
}

/// Add a state to the open state machine.
#[operator(
    id = "animgraph.add_state",
    label = "State",
    description = "Add a state to the open animation state machine.",
    allows_undo = false,
    is_available = state_machine_open
)]
pub fn animgraph_add_state(_: In<OperatorParameters>, mut commands: Commands) -> OperatorResult {
    commands.queue(|world: &mut World| {
        let at = editor::take_drop_point(world);
        document::add_state(world, at);
    });
    OperatorResult::Finished
}
