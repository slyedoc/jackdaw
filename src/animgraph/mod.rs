//! Animation: characters (a rig playing a bevy_animation_graph graph) and the graph editor. A
//! graph is edited beside the scene in the Animation Graph window, and the scene's characters
//! playing it are its preview.

mod character;
mod editor;
pub mod held;

pub(crate) use character::inject_animation_card;
pub(crate) use editor::pin_name as pin_label;

pub use editor::{AnimGraphRoot, document, graph_window_content};

use bevy::prelude::*;
use jackdaw_api::prelude::*;
use jackdaw_feathers::icons::Icon;

pub(crate) const GRAPH_WINDOW_ID: &str = "jackdaw.animation_graph";

pub(crate) fn plugin(app: &mut App) {
    app.init_resource::<held::HeldGraph>();
    editor::plugin(app);
    character::plugin(app);
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
        ctx.register_operator::<AnimgraphSaveOp>()
            .register_operator::<character::AnimgraphTogglePlaybackOp>()
            .register_operator::<character::AnimgraphEditGraphOp>();
        // The Animation tab: a character's rig and its Playback card.
        ctx.register_inspector_category(jackdaw_api::inspector::InspectorCategory {
            id: "animation".into(),
            label: "Animation".into(),
            icon: Icon::PersonStanding,
            order: 35,
        });
        ctx.register_component_category::<jackdaw_animation_runtime::AnimationRig>("animation");
        ctx.entity_mut().world_scope(|world| {
            if let Some(mut registry) =
                world.get_resource_mut::<jackdaw_api::inspector::InspectorRegistry>()
            {
                registry.set_component_category_prefix("animation_card::", "animation");
            }
        });
        ctx.register_operator::<AnimgraphAddNodeOp>()
            .register_operator::<AnimgraphAddStateOp>();
        ctx.register_operator::<AnimgraphNewOp>()
            .register_operator::<AnimgraphNewGraphOp>()
            .register_operator::<AnimgraphNewFsmOp>()
            .register_operator::<AnimgraphNewCharacterOp>();
        ctx.register_menu_entry::<AnimgraphNewCharacterOp>(TopLevelMenu::Add)
            .register_menu_entry::<AnimgraphNewGraphOp>(TopLevelMenu::Add)
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
    match shown_folder(world) {
        Some(folder) => create_and_open(world, &folder, fsm),
        None => warn!("animgraph: no project folder to make it in"),
    }
}

/// The folder the Project window is showing, when it is inside the project's assets; else the
/// assets folder itself.
fn shown_folder(world: &World) -> Option<std::path::PathBuf> {
    let assets = world.resource::<AnimGraphRoot>().0.clone()?;
    let shown = world
        .get_resource::<crate::project_window::ProjectWindowState>()
        .map(|state| state.current_directory.clone())
        .filter(|dir| dir.is_dir() && dir.starts_with(&assets));
    Some(shown.unwrap_or(assets))
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
    open_graph(world, &file);
}

/// Open a graph or state machine for editing: held beside the scene, its canvas brought forward.
pub fn open_graph(world: &mut World, path: &std::path::Path) {
    held::hold(world, path);
    focus_graph_window(world);
}

/// Whether `path` names a graph or state machine document.
pub(crate) fn is_graph_path(path: &std::path::Path) -> bool {
    let name = path.to_string_lossy();
    name.ends_with(".animgraph.bsn") || name.ends_with(".fsm.bsn")
}

/// Bring the Animation Graph window's tab forward, if the dock has it.
fn focus_graph_window(world: &mut World) {
    use jackdaw_panels::tree::{DockNode, DockTree};
    let Some(mut tree) = world.get_resource_mut::<DockTree>() else {
        return;
    };
    let Some(leaf_id) = tree.find_leaf_with_window(GRAPH_WINDOW_ID) else {
        return;
    };
    let Some(leaf) = tree.get(leaf_id).and_then(DockNode::as_leaf) else {
        return;
    };
    let Some(tab) = leaf
        .tabs()
        .find_map(|(window, tab)| (window == GRAPH_WINDOW_ID).then_some(tab))
    else {
        return;
    };
    if leaf.active != Some(tab) {
        tree.set_active(leaf_id, tab);
    }
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

/// Add > Character: a character prefab (`character_N.bsn`) holding the project's rig and an
/// `AnimationRig`, with a starter graph beside it, in the folder the Project window shows; then
/// open it. Place it in a scene like any prefab; open the file to change the character.
#[operator(
    id = "animgraph.new_character",
    label = "Character",
    description = "Make a character: a prefab of the project's rig playing a starter animation graph.",
    allows_undo = false
)]
pub fn animgraph_new_character(_: In<OperatorParameters>, mut commands: Commands) -> OperatorResult {
    commands.queue(create_character);
    OperatorResult::Finished
}

fn create_character(world: &mut World) {
    let (Some(assets), Some(folder)) = (world.resource::<AnimGraphRoot>().0.clone(), shown_folder(world))
    else {
        warn!("animgraph: no project to make a character in");
        return;
    };
    let Some((rig, skeleton)) = find_rig(&assets) else {
        warn!("animgraph: the project has no rig (a `.skn.ron` beside its `.bsn`)");
        return;
    };
    let Some(name) = (1..1000)
        .map(|n| format!("character_{n}"))
        .find(|name| !folder.join(format!("{name}.bsn")).exists())
    else {
        return;
    };
    let relative = |path: &std::path::Path| {
        path.strip_prefix(&assets)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/")
    };
    let graph_file = folder.join(format!("{name}.animgraph.bsn"));
    let idle = find_idle_clip(&assets);
    if let Err(err) = std::fs::write(&graph_file, starter_graph(&name, idle.as_deref())) {
        warn!("animgraph: {} cannot be written: {err}", graph_file.display());
        return;
    }
    let character_file = folder.join(format!("{name}.bsn"));
    let character = format!(
        "#{name}
jackdaw::prefab::components::Prefab
bevy_transform::components::transform::Transform
jackdaw_animation_runtime::AnimationRig {{ graph: \"{graph}\", skeleton: \"{skeleton}\" }}
bevy_ecs::hierarchy::Children [
    jackdaw::prefab::components::IsA {{ source: \"{rig}\", deleted: [] }}
    jackdaw::prefab::components::PrefabEntityId(0)
    bevy_transform::components::transform::Transform
]
",
        graph = relative(&graph_file),
    );
    if let Err(err) = std::fs::write(&character_file, character) {
        warn!("animgraph: {} cannot be written: {err}", character_file.display());
        return;
    }
    if let Some(mut state) = world.get_resource_mut::<crate::project_window::ProjectWindowState>() {
        state.rebuild();
    }
    crate::scenes::operators::scene_open_system(world, &character_file);
}

/// The project's rig: a `.skn.ron` with its `.bsn` beside it (a Mannequin first), as asset paths.
fn find_rig(assets: &std::path::Path) -> Option<(String, String)> {
    let mut rigs: Vec<(String, String)> = jackdaw_bsn::walk_files_with_extensions(assets, &["skn.ron"])
        .into_iter()
        .filter_map(|skeleton| {
            let stem = skeleton.to_string_lossy().strip_suffix(".skn.ron")?.to_string();
            let rig = std::path::PathBuf::from(format!("{stem}.bsn"));
            rig.exists().then(|| {
                let asset = |p: &std::path::Path| {
                    p.strip_prefix(assets).unwrap_or(p).to_string_lossy().replace('\\', "/")
                };
                (asset(&rig), asset(&skeleton))
            })
        })
        .collect();
    rigs.sort_by_key(|(rig, _)| !rig.contains("Mannequin"));
    rigs.into_iter().next()
}

/// An idle clip to start a character on, as an asset path.
fn find_idle_clip(assets: &std::path::Path) -> Option<String> {
    let clips = jackdaw_bsn::walk_files_with_extensions(assets, &["anim.ron"]);
    let stem = |p: &std::path::PathBuf| {
        p.file_name()
            .map(|n| n.to_string_lossy().to_lowercase().trim_end_matches(".anim.ron").to_string())
            .unwrap_or_default()
    };
    // A plain idle first: "Crouch_Idle_Loop" also contains "idle_loop".
    let exact = |want: &str| clips.iter().find(|p| stem(p) == want);
    let starts = |want: &str| clips.iter().find(|p| stem(p).starts_with(want));
    let contains = |want: &str| clips.iter().find(|p| stem(p).contains(want));
    exact("idle_loop")
        .or_else(|| exact("idle"))
        .or_else(|| starts("idle"))
        .or_else(|| contains("idle"))
        .map(|p| {
        p.strip_prefix(assets).unwrap_or(p).to_string_lossy().replace('\\', "/")
    })
}

/// A graph that loops `clip` (travel extracted, so the character idles in place); empty without one.
fn starter_graph(name: &str, clip: Option<&str>) -> String {
    let Some(clip) = clip else {
        return NEW_GRAPH.replace("{name}", name);
    };
    format!(
        "#{name}
AnimGraph {{
    outputs: [Time, Data(\"pose\", Pose)],
    input_position: Vec2 {{ x: -240.0, y: 40.0 }},
    output_position: Vec2 {{ x: 620.0, y: 40.0 }},
}}
Links([
    Link(Data(\"pose\"), Node(\"loop\", \"pose\")),
    Link(Time(\"\"), NodeTime(\"loop\")),
])
Children [
    #idle
    ClipNode {{ clip: \"{clip}\", root_motion_mode: GroundPlane }}
    NodePosition(40.0, 40.0)
    --
    #loop
    LoopNode {{ interpolation_period: 0.2 }}
    Links([
        Link(Data(\"pose\"), Node(\"idle\", \"pose\")),
        Link(Time(\"time\"), NodeTime(\"idle\")),
    ])
    NodePosition(330.0, 40.0)
]
"
    )
}

/// Save the graph held open in the Animation Graph window.
#[operator(
    id = "animgraph.save",
    label = "Save Graph",
    description = "Save the animation graph held open in the Animation Graph window.",
    allows_undo = false
)]
pub fn animgraph_save(_: In<OperatorParameters>, mut commands: Commands) -> OperatorResult {
    commands.queue(|world: &mut World| {
        held::save(world);
    });
    OperatorResult::Finished
}

/// Whether Ctrl+A and the Add picker should offer nodes: a graph is held open and the pointer
/// is over its window.
pub(crate) fn adding_nodes(world: &mut World) -> bool {
    document::active_kind(world).is_some() && editor::pointer_over_graph_window(world)
}
