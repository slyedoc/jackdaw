//! bevy_animation_graph's editor inside jackdaw.
//!
//! Graphs (`.animgraph.bsn`) and state machines (`.fsm.bsn`) are documents: they open as scene
//! tabs and the viewport shows them as a node canvas (see [`document`]). Nodes are added the way
//! entities are: Ctrl+A (at the cursor) or the Add menu, which offer node types while a graph is
//! open. Clips (event tracks), skeletons and ragdolls are
//! single-value `.ron` assets, opened from the Animation Graph window's browser and saved back
//! with Ctrl+S. The Animation Preview window plays the open graph on a rig, its inputs as
//! sliders.
//!
//! Replaces aurora_files' standalone `animgraph_editor` (its history is zero's
//! docs/animgraph_editor.md).

use bevy::{
    animation::{AnimatedBy, AnimationTargetId},
    feathers::{controls::FeathersSlider, theme::ThemedText},
    feathers_inspector::{BuildAssetInspector, BuildCustomInspector, ReflectInspectorWidget},
    image::Image,
    input::mouse::MouseScrollUnit,
    prelude::*,
    ui::{
        AlignItems, BackgroundColor, ComputedNode, Display, FlexDirection, JustifyContent,
        Overflow, PositionType, UiRect, Val,
    },
    ui_widgets::{SliderValue, ValueChange, slider_self_update},
};
use bevy_animation_graph::core::{
    animation_clip::{GraphClip, loader::GraphClipSerial},
    animation_graph::{AnimationGraph, NodeId, SourcePin, TargetPin},
    animation_graph_player::AnimationGraphPlayer,
    animation_node::AnimationNode,
    context::{
        node_states::StateKey,
        spec_context::{NodeInput, NodeOutput, NodeSpec, SpecResources},
    },
    edge_data::{DataSpec, DataValue, events::AnimationEvent},
    event_track::TrackItem,
    ragdoll::definition::{Body, BodyId, ColliderShape, JointVariant, Ragdoll},
    skeleton::Skeleton,
    state_machine::high_level::{StateId, StateMachine},
};
use bevy_aurora::prelude::*;
use bevy_aurora::ui_render::UiPolyline;
use uuid::Uuid;

use std::any::TypeId;

use std::path::PathBuf;

pub mod document;

use document::ActiveGraphDoc;

/// One browsable file under the asset root.
#[derive(Clone)]
struct Entry {
    /// Asset-server path, e.g. `anim/human/locomotion.animgraph.bsn`.
    path: String,
    kind: Kind,
}

/// Which loader owns an extension, and therefore which asset type the inspector binds to.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Graph,
    Clip,
    Skeleton,
    StateMachine,
    Ragdoll,
}

impl Kind {
    /// Longest-suffix match, because every one of these ends in `.ron`.
    fn of(name: &str) -> Option<Self> {
        if name.ends_with(".animgraph.bsn") {
            Some(Self::Graph)
        } else if name.ends_with(".anim.ron") {
            Some(Self::Clip)
        } else if name.ends_with(".skn.ron") {
            Some(Self::Skeleton)
        } else if name.ends_with(".fsm.bsn") {
            Some(Self::StateMachine)
        } else if name.ends_with(".rag.ron") {
            Some(Self::Ragdoll)
        } else {
            None
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Graph => "graph",
            Self::Clip => "clip",
            Self::Skeleton => "skeleton",
            Self::StateMachine => "fsm",
            Self::Ragdoll => "ragdoll",
        }
    }

    fn type_id(self) -> TypeId {
        match self {
            Self::Graph => TypeId::of::<AnimationGraph>(),
            Self::Clip => TypeId::of::<GraphClip>(),
            Self::Skeleton => TypeId::of::<Skeleton>(),
            Self::StateMachine => TypeId::of::<StateMachine>(),
            Self::Ragdoll => TypeId::of::<Ragdoll>(),
        }
    }
}

#[derive(Resource, Default)]
struct Library {
    entries: Vec<Entry>,
    /// The same entries as a directory tree, which is what the browser draws.
    root: Dir,
}

/// One directory in the browser tree. `BTreeMap` so children come out sorted without a pass.
#[derive(Default)]
struct Dir {
    dirs: std::collections::BTreeMap<String, Dir>,
    files: Vec<Entry>,
}

impl Dir {
    fn insert(&mut self, entry: Entry) {
        let mut cursor = self;
        let segments: Vec<&str> = entry.path.split('/').collect();
        for segment in &segments[..segments.len().saturating_sub(1)] {
            cursor = cursor.dirs.entry((*segment).to_string()).or_default();
        }
        cursor.files.push(entry);
    }
}

/// Which directories are expanded, keyed by their path from the root ("anim/human").
#[derive(Resource, Default)]
struct Expanded(std::collections::HashSet<String>);

/// Set when the tree needs respawning (a directory was toggled).
#[derive(Resource, Default)]
struct BrowserDirty(bool);

/// The pane the inspector builds into.
#[derive(Resource)]
struct InspectorPane(Entity);

/// The title strip above the inspector.
#[derive(Component)]
struct SelectionLabel;

/// Where the selected node's parameters are listed.
#[derive(Component)]
struct NodeParamsHost;

/// The canvas node the inspector is showing, and whether that list needs rebuilding.
#[derive(Resource, Default)]
struct Selected {
    node: Option<Uuid>,
    /// Which event track the selected id belongs to; timelines only.
    track: Option<String>,
    dirty: bool,
}

/// The centre pane's input-slider column.
#[derive(Component)]
struct InputsHost;

/// The browser pane, so the pre-spawned rows can be parented to it.
#[derive(Component)]
struct Browser;

/// The inspector pane's parent, so the pre-spawned body can be parented to it.
#[derive(Component)]
struct InspectorHost;

/// The rig the preview plays on, and the graph currently armed on it.
#[derive(Resource)]
struct Preview {
    /// Prefab root, so it can be despawned when the rig changes.
    root: Entity,
    armature: Option<Entity>,
    graph: Option<Handle<AnimationGraph>>,
    skeleton: Handle<Skeleton>,
}

/// One generated slider's binding: which graph input it drives.
#[derive(Component, Clone, Default)]
struct GraphInput(String);

/// A handle kept alive while its asset is open, plus what to bind once it finishes loading.
#[derive(Resource)]
struct Opening {
    handle: UntypedHandle,
    kind: Kind,
    path: String,
    bound: bool,
}

/// The project's assets directory: what the browser lists and saves write under.
#[derive(Resource, Default, Clone, PartialEq)]
pub struct AnimGraphRoot(pub Option<PathBuf>);

/// Marks the graph window's root node; its `RelativeCursorPosition` scopes the shortcuts.
#[derive(Component)]
struct GraphWindowRoot;

/// The editor's shortcuts act only with the pointer over the graph window and no text field
/// taking keys -- Delete and Ctrl+S mean something else everywhere else in the editor.
/// The pointer is over the Animation Graph window or the viewport's canvas, and no text field
/// has the keyboard.
fn graph_window_hovered(
    roots: Query<
        &bevy::ui::RelativeCursorPosition,
        Or<(With<GraphWindowRoot>, With<CanvasRoot>)>,
    >,
    focus: Option<Res<bevy::input_focus::InputFocus>>,
) -> bool {
    focus.is_none_or(|f| f.get().is_none()) && roots.iter().any(|r| r.cursor_over())
}

pub(crate) fn plugin(app: &mut App) {
    if !app.is_plugin_added::<bevy::feathers_inspector::DefaultInspectorWidgetsPlugin>() {
        app.add_plugins(bevy::feathers_inspector::DefaultInspectorWidgetsPlugin);
    }
    app.init_resource::<AnimGraphRoot>();
    app.init_resource::<Library>();
    app.init_resource::<Expanded>();
    app.init_resource::<CanvasView>();
    app.init_resource::<Selected>();
    app.init_resource::<Wiring>();
    app.init_resource::<TimelineCursor>();
    // A `Handle<A>` field otherwise recurses as the enum it is -- a Strong/Uuid picker over an
    // Arc. The inspector ships the widget but registers it for no asset type, because it has
    // no list of an app's; these four are the ones a node body can hold.
    app.register_type_data::<Handle<GraphClip>, ReflectInspectorWidget>();
    app.register_type_data::<Handle<AnimationGraph>, ReflectInspectorWidget>();
    app.register_type_data::<Handle<Skeleton>, ReflectInspectorWidget>();
    app.register_type_data::<Handle<StateMachine>, ReflectInspectorWidget>();
    app.init_resource::<NodeDropAt>();
    app.init_resource::<WireStart>();
    app.init_resource::<ActiveGraphDoc>();
    app.insert_resource(BrowserDirty(true));
    app.add_observer(attach_canvas);
    app.add_observer(attach_inspector);
    app.add_observer(attach_preview_image);
    app.add_systems(Startup, spawn_preview_stage);
    app.add_systems(
        Update,
        (
            scan_library.run_if(resource_changed::<AnimGraphRoot>),
            choose_preview_rig.run_if(resource_changed::<Library>),
            rebuild_browser,
            bind_when_loaded,
            document::track_graph_document
                .run_if(resource_exists_and_changed::<jackdaw_bsn::SceneBsnAst>),
            arm_preview,
            draw_canvas,
            highlight_selected,
            highlight_pins,
            draw_wire_preview,
            highlight_bars,
            highlight_ragdoll_rows,
            draw_ragdoll,
            show_pin_values,
            show_node_params,
            save_graph.run_if(graph_window_hovered),
            close_clip_timeline,
        ),
    );
}

/// Walk the asset root for everything the editor can open. Asset paths are root-relative with
/// `/` separators, which is what the asset server wants on every platform.
fn scan_library(
    root: Res<AnimGraphRoot>,
    mut library: ResMut<Library>,
    mut expanded: ResMut<Expanded>,
    mut view: ResMut<CanvasView>,
    mut dirty: ResMut<BrowserDirty>,
) {
    *library = Library::default();
    dirty.0 = true;
    let Some(root) = root.0.clone() else {
        return;
    };
    view.root = root.clone();
    let mut stack = vec![root.clone()];
    while let Some(dir) = stack.pop() {
        let Ok(read) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in read.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            let Some(kind) = Kind::of(name) else { continue };
            let Ok(rel) = path.strip_prefix(&root) else {
                continue;
            };
            library.entries.push(Entry {
                path: rel.to_string_lossy().replace('\\', "/"),
                kind,
            });
        }
    }
    library.entries.sort_by(|a, b| a.path.cmp(&b.path));
    let entries = library.entries.clone();
    for entry in entries {
        library.root.insert(entry);
    }
    for name in library.root.dirs.keys() {
        expanded.0.insert(name.clone());
    }
    info!(
        "library: {} assets under {}",
        library.entries.len(),
        root.display()
    );
}

/// The preview rig's world: the mannequin, a floor and a sun, filmed by a camera into an
/// image the Animation Preview window shows. A world of its own (a `PhysicsWorld`), so none
/// of it shows in, or is lit by, the scene being edited.
#[derive(Resource)]
pub struct PreviewTarget(pub Handle<Image>);

fn spawn_preview_stage(
    mut commands: Commands,
    mut meshes: ResMut<Assets<AuroraMesh>>,
    mut images: ResMut<Assets<Image>>,
    mut materials: ResMut<Assets<AuroraMaterial>>,
) {
    let stage = commands
        .spawn((
            Name::new("Animation Preview World"),
            crate::EditorEntity,
            bevy_aurora::world::PhysicsWorld,
        ))
        .id();
    let target = images.add(Image::new_target_texture(
        960,
        640,
        wgpu_types::TextureFormat::Rgba8Unorm,
        Some(wgpu_types::TextureFormat::Rgba8UnormSrgb),
    ));
    commands.insert_resource(PreviewTarget(target.clone()));
    commands.spawn((
        Name::new("Animation Preview Camera"),
        crate::EditorEntity,
        Camera3d::default(),
        Camera {
            order: -1,
            ..default()
        },
        bevy::camera::RenderTarget::Image(target.into()),
        Transform::from_xyz(0.0, 1.15, -4.2).looking_at(Vec3::new(0.0, 0.95, 0.0), Vec3::Y),
        ChildOf(stage),
    ));
    commands.spawn((
        Name::new("Animation Preview Sun"),
        crate::EditorEntity,
        DirectionalLight {
            illuminance: 20_000.0,
            ..default()
        },
        Transform::from_xyz(3.0, 8.0, -4.0).looking_at(Vec3::ZERO, Vec3::Y),
        ChildOf(stage),
    ));
    // A floor under the rig: without one a walk cycle plays against the sky with nothing for
    // the feet to meet and no contact shadow to read the pose against.
    commands.spawn((
        Name::new("Animation Preview Floor"),
        crate::EditorEntity,
        AuroraMesh3d(meshes.add(AuroraMesh::from_shape(
            Plane3d::default().mesh().size(40.0, 40.0),
        ))),
        AuroraMaterial3d(materials.add(AuroraMaterial {
            base_color: Color::srgb(0.18, 0.19, 0.21),
            perceptual_roughness: 0.85,
            ..default()
        })),
        // Just below the origin the rig stands on, so the mesh never z-fights the feet.
        Transform::from_xyz(0.0, -0.002, 0.0),
        ChildOf(stage),
    ));
    commands.insert_resource(PreviewStage(stage));
    // The rig arrives with the project (`choose_preview_rig`).
    commands.insert_resource(Preview {
        root: Entity::PLACEHOLDER,
        armature: None,
        graph: None,
        skeleton: Handle::default(),
    });
}

/// The preview world's root, which the rig spawns under.
#[derive(Resource)]
struct PreviewStage(Entity);

/// The rig the preview plays graphs on: a skeleton in the project with its rig scene beside it
/// (`<rig>.skn.ron` next to `<rig>.bsn`, as aurora_files' `animlib_import` writes them),
/// preferring the mannequin, whose clips are an identity retarget and so show a graph as
/// authored.
fn choose_preview_rig(
    mut commands: Commands,
    library: Res<Library>,
    root: Res<AnimGraphRoot>,
    stage: Option<Res<PreviewStage>>,
    assets: Res<AssetServer>,
    mut preview: ResMut<Preview>,
    mut chosen: Local<Option<String>>,
) {
    let (Some(stage), Some(dir)) = (stage, root.0.as_ref()) else {
        return;
    };
    let rigs: Vec<(String, String)> = library
        .entries
        .iter()
        .filter(|e| e.kind == Kind::Skeleton)
        .filter_map(|e| {
            let scene = format!("{}.bsn", e.path.strip_suffix(".skn.ron")?);
            dir.join(&scene).is_file().then(|| (e.path.clone(), scene))
        })
        .collect();
    let rig = rigs
        .iter()
        .find(|(skeleton, _)| skeleton.contains("Mannequin"))
        .or(rigs.first())
        .cloned();
    if rig.as_ref().map(|(skeleton, _)| skeleton) == chosen.as_ref() {
        return;
    }
    *chosen = rig.as_ref().map(|(skeleton, _)| skeleton.clone());
    if preview.root != Entity::PLACEHOLDER {
        commands.entity(preview.root).despawn();
    }
    preview.root = Entity::PLACEHOLDER;
    preview.armature = None;
    let Some((skeleton, scene)) = rig else {
        return;
    };
    info!("animation preview rig: {scene}");
    preview.root = commands
        .spawn((
            Name::new("Animation Preview Rig"),
            crate::EditorEntity,
            bevy::scene::ScenePatchInstance(assets.load(scene)),
            Transform::from_rotation(Quat::from_rotation_y(std::f32::consts::PI)),
            Visibility::Visible,
            ChildOf(stage.0),
        ))
        .id();
    preview.skeleton = assets.load(skeleton);
    // A graph armed on the old rig re-arms on the new one.
    if let Some(graph) = preview.graph.take() {
        commands.insert_resource(ArmGraph(graph));
    }
}

/// Marks the node the preview image is shown in.
#[derive(Component)]
struct PreviewImageHost;

/// The Animation Preview window: the rig, filmed.
pub fn preview_window_content() -> impl Bundle {
    (
        Node {
            width: Val::Percent(100.0),
            height: Val::Percent(100.0),
            ..default()
        },
        BackgroundColor(Color::srgb(0.06, 0.06, 0.07)),
        children![
            (
                PreviewImageHost,
                Node {
                    width: Val::Percent(100.0),
                    height: Val::Percent(100.0),
                    ..default()
                },
            ),
        ],
    )
}

/// Double-click on a clip node: its clip's event tracks take the canvas, Esc gives it back.
fn open_clip_timeline(world: &mut World, id: Uuid) {
    let Some(handle) = document::clip_of_node(world, id) else {
        return;
    };
    let path = handle.path().map(|p| p.path().to_string_lossy().into_owned());
    let preview = world
        .resource_mut::<Assets<AnimationGraph>>()
        .add(document::clip_preview_graph(handle.clone()));
    world.insert_resource(ArmGraph(preview));
    let mut view = world.resource_mut::<CanvasView>();
    view.clip = Some(handle);
    view.path = path;
    // A timeline scrolls in time: pan from the left edge, zoom in pixels per second.
    view.pan = Vec2::new(0.0, 12.0);
    view.zoom = 1.0;
    view.dirty = true;
}

/// Esc on a clip opened from a graph: back to the graph.
fn close_clip_timeline(
    mut commands: Commands,
    keys: Res<ButtonInput<KeyCode>>,
    doc: Res<ActiveGraphDoc>,
    mut view: ResMut<CanvasView>,
) {
    if !keys.just_pressed(KeyCode::Escape) || view.clip.is_none() || doc.0.is_none() {
        return;
    }
    if let Some(graph) = view.graph.clone() {
        commands.insert_resource(ArmGraph(graph));
    }
    view.clip = None;
    view.path = None;
    view.zoom = 1.0;
    frame_layout(&mut view);
    view.dirty = true;
}

/// Marks the viewport's node-canvas column.
#[derive(Component)]
struct CanvasRoot;

/// Where the next added node goes, in canvas coordinates, when something asked for a spot.
/// `None` puts it under the cursor, or mid-view.
#[derive(Resource, Default)]
pub(crate) struct NodeDropAt(pub Option<Vec2>);

/// Where a node added now should go: a spot asked for, else under the cursor when it is over
/// the canvas, else the view's centre.
pub(crate) fn take_drop_point(world: &mut World) -> Vec2 {
    if let Some(at) = world.resource_mut::<NodeDropAt>().0.take() {
        return at;
    }
    let view = world.resource::<CanvasView>();
    let (pan, zoom) = (view.pan, view.zoom);
    let cursor = world
        .query::<&Window>()
        .iter(world)
        .find_map(Window::cursor_position);
    let mut canvases =
        world.query_filtered::<(&ComputedNode, &bevy::ui::UiGlobalTransform), With<Canvas>>();
    let Some((computed, transform)) = canvases.iter(world).next() else {
        return Vec2::ZERO;
    };
    let scale = computed.inverse_scale_factor();
    let size = computed.size * scale;
    let top_left = transform.translation * scale - size * 0.5;
    let local = match cursor.map(|c| c - top_left) {
        Some(local) if local.cmpge(Vec2::ZERO).all() && local.cmplt(size).all() => local,
        _ => size * 0.5 - Vec2::new(NODE_W * 0.5, 0.0) * zoom,
    };
    (local - pan) / zoom
}

/// The viewport's node-canvas column. Hidden until the viewport is in graph mode.
pub fn build_graph_presentation(world: &mut World, parent: Entity) -> Entity {
    world
        .spawn((
            Name::new("graph canvas"),
            CanvasRoot,
            bevy::ui::RelativeCursorPosition::default(),
            BackgroundColor(Color::srgba(0.06, 0.06, 0.07, 1.0)),
            Node {
                display: Display::None,
                width: Val::Percent(100.0),
                height: Val::Percent(100.0),
                flex_grow: 1.0,
                flex_direction: FlexDirection::Row,
                ..default()
            },
            ChildOf(parent),
            children![
                (
                    Name::new("canvas area"),
                    Node {
                        flex_grow: 1.0,
                        height: Val::Percent(100.0),
                        overflow: Overflow::clip(),
                        ..default()
                    },
                    children![(
                        Name::new("canvas"),
                        Canvas,
                        Node {
                            position_type: PositionType::Absolute,
                            left: Val::Px(0.0),
                            top: Val::Px(0.0),
                            right: Val::Px(0.0),
                            bottom: Val::Px(0.0),
                            // Node boxes are absolutely positioned and pan freely.
                            overflow: Overflow::clip(),
                            ..default()
                        },
                    )],
                ),
                // What is being edited, playing: the preview rig, and the graph's inputs.
                (
                    Name::new("preview"),
                    BackgroundColor(Color::srgba(0.05, 0.05, 0.06, 1.0)),
                    Node {
                        width: Val::Percent(32.0),
                        min_width: Val::Px(260.0),
                        height: Val::Percent(100.0),
                        flex_direction: FlexDirection::Column,
                        overflow: Overflow::scroll_y(),
                        ..default()
                    },
                    children![
                        (
                            PreviewImageHost,
                            Node {
                                width: Val::Percent(100.0),
                                ..default()
                            },
                        ),
                        (
                            Name::new("inputs"),
                            InputsHost,
                            Node {
                                flex_direction: FlexDirection::Column,
                                row_gap: Val::Px(4.0),
                                padding: UiRect::all(Val::Px(8.0)),
                                ..default()
                            },
                        ),
                    ],
                ),
            ],
        ))
        .id()
}

fn attach_preview_image(
    add: On<Add<PreviewImageHost>>,
    mut commands: Commands,
    target: Option<Res<PreviewTarget>>,
) {
    if let Some(target) = target {
        commands
            .entity(add.entity)
            .insert(ImageNode::new(target.0.clone()));
    }
}

/// Marks the inspector pane's scrolling body.
#[derive(Component)]
struct InspectorBody;

/// The Animation Graph window: the browser of the project's animation assets, and the
/// inspector for the single-value ones (clips, skeletons, ragdolls).
pub fn graph_window_content() -> impl Bundle {
    let pane = BackgroundColor(Color::srgba(0.06, 0.06, 0.07, 0.94));
    (
        Name::new("animation graph"),
        GraphWindowRoot,
        bevy::ui::RelativeCursorPosition::default(),
        Node {
            width: Val::Percent(100.0),
            height: Val::Percent(100.0),
            flex_direction: FlexDirection::Row,
            ..default()
        },
        children![
            (
                Name::new("browser"),
                Browser,
                pane,
                Node {
                    flex_grow: 1.0,
                    min_width: Val::Px(200.0),
                    height: Val::Percent(100.0),
                    flex_direction: FlexDirection::Column,
                    row_gap: Val::Px(2.0),
                    padding: UiRect::all(Val::Px(8.0)),
                    overflow: Overflow::scroll_y(),
                    ..default()
                },
            ),
            (
                Name::new("inspector"),
                InspectorHost,
                pane,
                Node {
                    flex_grow: 1.0,
                    min_width: Val::Px(220.0),
                    height: Val::Percent(100.0),
                    flex_direction: FlexDirection::Column,
                    overflow: Overflow::scroll_y(),
                    ..default()
                },
                children![
                    (
                        Name::new("selection"),
                        SelectionLabel,
                        Text::new("nothing selected"),
                        ThemedText,
                        Node {
                            padding: UiRect::all(Val::Px(8.0)),
                            ..default()
                        },
                    ),
                    // The selected node's parameters. Separate from the asset inspector below
                    // because `AnimationGraph::nodes` is `#[reflect(ignore)]`, so no reflection
                    // path reaches a node from the asset root -- see `show_node_params`.
                    (
                        Name::new("node params"),
                        NodeParamsHost,
                        Node {
                            flex_direction: FlexDirection::Column,
                            row_gap: Val::Px(2.0),
                            padding: UiRect::horizontal(Val::Px(8.0)),
                            ..default()
                        },
                    ),
                    (
                        Name::new("inspector body"),
                        InspectorBody,
                        Node {
                            flex_direction: FlexDirection::Column,
                            row_gap: Val::Px(4.0),
                            padding: UiRect::all(Val::Px(8.0)),
                            overflow: Overflow::scroll_y(),
                            flex_grow: 1.0,
                            ..default()
                        },
                    ),
                ],
            ),
        ],
    )
}

/// The dock respawns a window's content wholesale; a new inspector body becomes the pane the
/// asset inspector builds into, and the browser and canvas redraw into their new hosts.
fn attach_inspector(
    add: On<Add<InspectorBody>>,
    mut commands: Commands,
    mut dirty: ResMut<BrowserDirty>,
    mut view: ResMut<CanvasView>,
) {
    commands.insert_resource(InspectorPane(add.entity));
    dirty.0 = true;
    view.dirty = true;
}

/// Give a (re)spawned canvas its pan and zoom.
fn attach_canvas(add: On<Add<Canvas>>, mut commands: Commands) {
    commands
        .entity(add.entity)
        // MIDDLE- or right-drag the background to pan. Not the left button: that is the node
        // drag, and a left-drag on the background would then fight it — and a stray press from
        // the window manager on focus arrives as a left drag the moment the window opens.
        .observe(|drag: On<PointerDrag>, mut view: ResMut<CanvasView>| {
            if !matches!(
                drag.button,
                PointerButton::Middle | PointerButton::Secondary
            ) {
                return;
            }
            view.pan += drag.delta;
            view.dirty = true;
        })
        // Scroll to zoom, about the centre of the canvas — the pan that is already there is
        // what puts a region under the cursor, so anchoring on the pointer buys little.
        .observe(
            |scroll: On<PointerScroll>,
             mut view: ResMut<CanvasView>,
             nodes: Query<&ComputedNode>| {
                let ticks = match scroll.unit {
                    MouseScrollUnit::Line => scroll.y,
                    // A trackpad reports pixels; 40 of them is about one wheel notch.
                    MouseScrollUnit::Pixel => scroll.y / 40.0,
                };
                let old = view.zoom;
                let new = (old * 1.12f32.powf(ticks)).clamp(0.25, 2.5);
                if new == old {
                    return;
                }
                // `ComputedNode::size` is PHYSICAL pixels; `Val::Px` is logical.
                let centre = nodes
                    .get(scroll.entity)
                    .map(|n| n.size * n.inverse_scale_factor() * 0.5)
                    .unwrap_or(Vec2::splat(400.0));
                // Keep whatever sits at `centre` fixed: px = world * zoom + pan.
                view.pan = centre - (centre - view.pan) * (new / old);
                view.zoom = new;
                view.dirty = true;
            },
        );
}

/// Respawn the browser tree whenever a directory is toggled (and once at startup).
fn rebuild_browser(
    mut commands: Commands,
    mut dirty: ResMut<BrowserDirty>,
    library: Res<Library>,
    expanded: Res<Expanded>,
    browser: Single<(Entity, Option<&Children>), With<Browser>>,
) {
    if !dirty.0 {
        return;
    }
    dirty.0 = false;
    let (browser, children) = *browser;
    if let Some(children) = children {
        for child in children.iter() {
            commands.entity(child).despawn();
        }
    }
    let mut rows = Vec::new();
    spawn_dir(&mut commands, &library.root, "", 0, &expanded.0, &mut rows);
    commands.entity(browser).add_children(&rows);
}

/// One row per directory, then one per file, depth-first. Only an expanded directory
/// recurses, so a collapsed subtree costs nothing.
fn spawn_dir(
    commands: &mut Commands,
    dir: &Dir,
    path: &str,
    depth: usize,
    expanded: &std::collections::HashSet<String>,
    rows: &mut Vec<Entity>,
) {
    let indent = Val::Px(6.0 + depth as f32 * 14.0);
    for (name, child) in &dir.dirs {
        let child_path = if path.is_empty() {
            name.clone()
        } else {
            format!("{path}/{name}")
        };
        let open = expanded.contains(&child_path);
        // ASCII markers: this shell inherits whatever font feathers ships, and a missing
        // glyph reads as tofu rather than as a disclosure arrow.
        let label = format!("{} {}/", if open { "-" } else { "+" }, name);
        let toggle_path = child_path.clone();
        let row = commands
            .spawn((
                Node {
                    padding: UiRect::new(indent, Val::Px(6.0), Val::Px(3.0), Val::Px(3.0)),
                    ..default()
                },
                Children::spawn(Spawn((
                    Text::new(label),
                    ThemedText,
                    TextLayout {
                        linebreak: bevy::text::LineBreak::NoWrap,
                        ..default()
                    },
                ))),
            ))
            .observe(
                move |_: On<PointerClick>,
                      mut expanded: ResMut<Expanded>,
                      mut dirty: ResMut<BrowserDirty>| {
                    if !expanded.0.remove(&toggle_path) {
                        expanded.0.insert(toggle_path.clone());
                    }
                    dirty.0 = true;
                },
            )
            .id();
        rows.push(row);
        if open {
            spawn_dir(commands, child, &child_path, depth + 1, expanded, rows);
        }
    }
    for entry in &dir.files {
        let entry = entry.clone();
        let leaf = entry
            .path
            .rsplit('/')
            .next()
            .unwrap_or(&entry.path)
            .to_string();
        let label = format!("  {}   [{}]", leaf, entry.kind.label());
        let row = commands
            .spawn((
                Node {
                    padding: UiRect::new(indent, Val::Px(6.0), Val::Px(3.0), Val::Px(3.0)),
                    ..default()
                },
                Children::spawn(Spawn((
                    Text::new(label),
                    ThemedText,
                    TextLayout {
                        linebreak: bevy::text::LineBreak::NoWrap,
                        ..default()
                    },
                ))),
            ))
            .observe(
                move |_: On<PointerClick>,
                      assets: Res<AssetServer>,
                      root: Res<AnimGraphRoot>,
                      mut commands: Commands| {
                    // Graphs and state machines are documents: they open as a tab.
                    if matches!(entry.kind, Kind::Graph | Kind::StateMachine) {
                        let Some(file) = root.0.as_ref().map(|r| r.join(&entry.path)) else {
                            return;
                        };
                        commands.queue(move |world: &mut World| {
                            crate::scenes::operators::scene_open_system(world, &file);
                        });
                        return;
                    }
                    // Typed load per kind so the right loader runs; the binding itself is
                    // untyped, keyed on the asset id.
                    let handle: UntypedHandle = match entry.kind {
                        Kind::Clip => assets.load::<GraphClip>(&entry.path).untyped(),
                        Kind::Skeleton => assets.load::<Skeleton>(&entry.path).untyped(),
                        Kind::Ragdoll => assets.load::<Ragdoll>(&entry.path).untyped(),
                        Kind::Graph | Kind::StateMachine => return,
                    };
                    commands.insert_resource(Opening {
                        handle,
                        kind: entry.kind,
                        path: entry.path.clone(),
                        bound: false,
                    });
                },
            )
            .id();
        rows.push(row);
    }
}

/// An asset is only inspectable once it has finished loading, so binding waits for it.
fn bind_when_loaded(
    mut commands: Commands,
    opening: Option<ResMut<Opening>>,
    assets: Res<AssetServer>,
    pane: If<Res<InspectorPane>>,
    mut view: ResMut<CanvasView>,
    mut label: Single<&mut Text, With<SelectionLabel>>,
) {
    let Some(mut opening) = opening else { return };
    if opening.bound {
        return;
    }
    if !assets.is_loaded(&opening.handle) {
        return;
    }
    opening.bound = true;
    label.0 = opening.path.clone();
    commands.queue(BuildAssetInspector {
        asset_id: opening.handle.id(),
        type_id: opening.kind.type_id(),
        panel: pane.0.0,
    });
    if opening.kind == Kind::Clip {
        let handle = opening.handle.clone().typed::<GraphClip>();
        view.graph = None;
        view.fsm = None;
        view.ragdoll = None;
        view.clip = Some(handle);
        view.path = Some(opening.path.clone());
        // A timeline scrolls in TIME, so pan starts at the left edge rather than inset, and
        // zoom is pixels-per-second rather than a canvas scale.
        view.pan = Vec2::new(0.0, 12.0);
        view.zoom = 1.0;
        view.dirty = true;
    }
    if opening.kind == Kind::Ragdoll {
        view.graph = None;
        view.fsm = None;
        view.clip = None;
        view.ragdoll = Some(opening.handle.clone().typed::<Ragdoll>());
        view.path = Some(opening.path.clone());
        view.dirty = true;
    }
    commands.queue(|world: &mut World| {
        crate::viewport_host::focus_viewport(world, crate::viewport_host::ViewportMode::Graph);
    });
    info!("opened {}", opening.path);
}

/// `GraphInputPin`'s `Debug` is `Passthrough("speed")`; the pin NAME is what belongs on a
/// slider. No public accessor for it, so unwrap the one shape it prints.
fn pin_name(pin: &impl std::fmt::Debug) -> String {
    let text = format!("{pin:?}");
    text.split_once('"')
        .and_then(|(_, rest)| rest.rsplit_once('"'))
        .map(|(name, _)| name.to_string())
        .unwrap_or(text)
}

/// The canvas pane, which the node boxes and links are spawned into.
#[derive(Component)]
struct Canvas;

/// A node box on the canvas. Dragging one moves it; the id is what the drag writes back to.
#[derive(Component)]
struct CanvasNode(Uuid);

/// Node box geometry, in CANVAS coordinates — screen pixels are `canvas * zoom + pan`. Boxes
/// are laid out arithmetically rather than measured, so a link endpoint is known the moment
/// the box is spawned instead of a frame later.
const NODE_W: f32 = 210.0;
const HEADER_H: f32 = 22.0;
const PIN_H: f32 = 15.0;
const BOX_PAD: f32 = 5.0;
const LINK_THICKNESS: f32 = 2.0;

/// Canvas view state. The node layout lives HERE rather than in the asset so a drag does not
/// write `Assets<AnimationGraph>` every frame — which would fire `AssetEvent::Modified` at the
/// running player sixty times a second. It reaches the asset only on save.
#[derive(Resource)]
struct CanvasView {
    graph: Option<Handle<AnimationGraph>>,
    /// The state machine open instead, if this is an FSM rather than a graph.
    fsm: Option<Handle<StateMachine>>,
    /// The clip open instead, if this is an event-track timeline.
    clip: Option<Handle<GraphClip>>,
    /// The ragdoll open instead. This one has no canvas — it is drawn as 3D gizmos on the
    /// preview rig, because a body offset and a collider shape only mean anything in space.
    ragdoll: Option<Handle<Ragdoll>>,
    /// Asset-relative path of the open asset, which is where a save writes.
    path: Option<String>,
    /// The project's assets directory that `path` is relative to.
    root: PathBuf,
    /// Keyed by the raw `Uuid` that both `NodeId` and `StateId` wrap, so one canvas serves a
    /// graph's nodes and a state machine's states without a second layout map.
    positions: std::collections::HashMap<Uuid, Vec2>,
    input_pos: Vec2,
    output_pos: Vec2,
    pan: Vec2,
    zoom: f32,
    dirty: bool,
    /// Only the links need redrawing: a box is being dragged, and respawning it would end the
    /// drag.
    links_dirty: bool,
}

impl Default for CanvasView {
    fn default() -> Self {
        Self {
            graph: None,
            fsm: None,
            clip: None,
            ragdoll: None,
            path: None,
            root: PathBuf::new(),
            positions: std::collections::HashMap::new(),
            input_pos: Vec2::ZERO,
            output_pos: Vec2::ZERO,
            pan: Vec2::splat(24.0),
            zoom: 1.0,
            dirty: false,
            links_dirty: false,
        }
    }
}

/// Take the graph's authored layout, or compute one.
///
/// The loader fills in a position for EVERY node whether the file carried one or not, so an
/// unlaid-out graph arrives as a pile at the origin rather than as `None` — hence the
/// all-zero test rather than a per-node one. The fallback is a longest-path layering: a node's
/// column is one past its deepest predecessor, which for a locomotion graph reads left to
/// right in evaluation order.
fn seed_layout(view: &mut CanvasView, graph: &AnimationGraph) {
    view.positions.clear();
    view.pan = Vec2::splat(24.0);
    view.zoom = 1.0;

    let authored = graph
        .editor_metadata
        .node_positions
        .values()
        .any(|p| *p != Vec2::ZERO);
    // A stable order so the fallback does not reshuffle between runs.
    let mut ids: Vec<NodeId> = graph.nodes.keys().copied().collect();
    ids.sort_by_key(|id| format!("{id:?}"));

    if authored {
        for id in &ids {
            let pos = graph
                .editor_metadata
                .node_positions
                .get(id)
                .copied()
                .unwrap_or(Vec2::ZERO);
            view.positions.insert(id.uuid(), pos);
        }
        view.input_pos = graph.editor_metadata.input_position;
        view.output_pos = graph.editor_metadata.output_position;
        frame_layout(view);
        return;
    }

    let mut preds: std::collections::HashMap<NodeId, Vec<NodeId>> =
        ids.iter().map(|id| (*id, Vec::new())).collect();
    for (target, source) in graph.edges_inverted.iter() {
        let (Some(to), Some(from)) = (target_node(target), source_node(source)) else {
            continue;
        };
        if let Some(list) = preds.get_mut(&to) {
            list.push(from);
        }
    }

    // Iterative relaxation rather than a DFS: bounded by the node count, and a cycle just
    // stops improving instead of recursing forever.
    let mut depth: std::collections::HashMap<NodeId, usize> =
        ids.iter().map(|id| (*id, 0usize)).collect();
    for _ in 0..ids.len() {
        let mut changed = false;
        for id in &ids {
            let want = preds[id]
                .iter()
                .filter_map(|p| depth.get(p).copied())
                .max()
                .map_or(0, |d| d + 1);
            if want > depth[id] {
                depth.insert(*id, want);
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }

    let mut column_y: std::collections::HashMap<usize, f32> = std::collections::HashMap::new();
    for id in &ids {
        let column = depth[id];
        let y = column_y.entry(column).or_insert(0.0);
        view.positions
            .insert(id.uuid(), Vec2::new(column as f32 * (NODE_W + 80.0), *y));
        *y += 130.0;
    }
    let last = depth.values().copied().max().unwrap_or(0);
    view.input_pos = Vec2::new(-(NODE_W + 80.0), 0.0);
    view.output_pos = Vec2::new((last + 1) as f32 * (NODE_W + 80.0), 0.0);
    frame_layout(view);
}

/// Take the state machine's authored layout, or lay it out in a column.
///
/// An FSM has no evaluation order to lay out ALONG — a state machine is a cycle by nature —
/// so the fallback is a plain column rather than the graph's longest-path layering. Dragging
/// and saving is how a real arrangement gets made.
fn seed_fsm_layout(view: &mut CanvasView, fsm: &StateMachine) {
    view.positions.clear();
    view.pan = Vec2::splat(24.0);
    view.zoom = 1.0;
    let authored = fsm
        .editor_metadata
        .states
        .values()
        .any(|p| *p != Vec2::ZERO);
    let mut ids: Vec<StateId> = fsm.states.keys().copied().collect();
    ids.sort_by_key(|id| format!("{id:?}"));
    for (i, id) in ids.iter().enumerate() {
        let pos = if authored {
            fsm.editor_metadata
                .states
                .get(id)
                .copied()
                .unwrap_or(Vec2::ZERO)
        } else {
            Vec2::new((i % 3) as f32 * (NODE_W + 90.0), (i / 3) as f32 * 130.0)
        };
        view.positions.insert(id.uuid(), pos);
    }
    frame_layout(view);
}

/// Pan so the top-left of the laid-out graph sits just inside the canvas. The input rail is at
/// a negative x by construction, so without this a fresh graph opens with its left edge off
/// screen.
fn frame_layout(view: &mut CanvasView) {
    let min = view
        .positions
        .values()
        .chain([&view.input_pos, &view.output_pos])
        .fold(Vec2::splat(f32::MAX), |acc, p| acc.min(*p));
    if min.x < f32::MAX {
        view.pan = Vec2::splat(24.0) - min * view.zoom;
    }
}

/// Upstream's node names are emoji-prefixed (`∑ Blend`, `⌚ Speed`). This shell inherits
/// whatever font feathers ships, which has none of them, so a prefix renders as tofu.
fn ascii(text: &str) -> String {
    text.chars()
        .filter(|c| c.is_ascii_graphic() || *c == ' ')
        .collect::<String>()
        .trim()
        .to_string()
}

/// The node a target pin belongs to, or `None` for a graph-level output.
fn target_node(target: &TargetPin) -> Option<NodeId> {
    match target {
        TargetPin::NodeData(id, _) | TargetPin::NodeTime(id, _) => Some(*id),
        TargetPin::OutputData(_) | TargetPin::OutputTime => None,
    }
}

/// The node a source pin belongs to, or `None` for a graph-level input.
fn source_node(source: &SourcePin) -> Option<NodeId> {
    match source {
        SourcePin::NodeData(id, _) | SourcePin::NodeTime(id) => Some(*id),
        SourcePin::InputData(_) | SourcePin::InputTime(_) => None,
    }
}

/// A graph waiting to be put on the preview rig.
#[derive(Resource)]
struct ArmGraph(Handle<AnimationGraph>);

/// Find the rig's armature (once it streams in), swap in an `AnimationGraphPlayer` for the
/// requested graph, and rebuild the input sliders from the graph's own `io_spec`.
fn arm_preview(
    mut commands: Commands,
    arm: Option<Res<ArmGraph>>,
    mut preview: ResMut<Preview>,
    graphs: Res<Assets<AnimationGraph>>,
    names: Query<&Name>,
    children: Query<&Children>,
    host: Single<(Entity, Option<&Children>), With<InputsHost>>,
) {
    let Some(arm) = arm else { return };
    let Some(graph) = graphs.get(&arm.0) else {
        return;
    };
    // Hydrate the name-path components a text `.bsn` cannot carry, exactly as zero's
    // locomotion module does, then bind the player.
    let Some(armature) = children.get(preview.root).ok().and_then(|kids| {
        kids.iter()
            .find(|&k| names.get(k).is_ok_and(|n| n.as_str() == "Armature"))
    }) else {
        return;
    };
    let Ok(bones) = children.get(armature) else {
        return;
    };
    let root_name = Name::new("Armature");
    commands.entity(armature).insert((
        AnimationTargetId::from_names([root_name.clone()].iter()),
        AnimatedBy(preview.root),
    ));
    let mut stack: Vec<(Entity, Vec<Name>)> = bones
        .iter()
        .map(|bone| (bone, vec![root_name.clone()]))
        .collect();
    while let Some((bone, path)) = stack.pop() {
        let Ok(name) = names.get(bone) else { continue };
        let mut path = path;
        path.push(name.clone());
        commands.entity(bone).insert((
            AnimationTargetId::from_names(path.iter()),
            AnimatedBy(armature),
        ));
        if let Ok(kids) = children.get(bone) {
            stack.extend(kids.iter().map(|kid| (kid, path.clone())));
        }
    }
    commands
        .entity(armature)
        .remove::<AnimationPlayer>()
        .insert(AnimationGraphPlayer::new(preview.skeleton.clone()).with_graph(arm.0.clone()));
    preview.armature = Some(armature);
    preview.graph = Some(arm.0.clone());

    // One row per F32 input, seeded from the graph's own default. `default_data` rather than
    // `io_spec.input_data` because the spec's map has no public reader — and the defaults are
    // what a slider wants to start at anyway.
    let (host_entity, existing) = *host;
    if let Some(existing) = existing {
        for child in existing.iter() {
            commands.entity(child).despawn();
        }
    }
    let mut rows = Vec::new();
    let mut inputs: Vec<(String, f32)> = graph
        .default_data
        .iter()
        .filter_map(|(pin, value)| match value {
            DataValue::F32(v) => Some((pin_name(pin), *v)),
            _ => None,
        })
        .collect();
    inputs.sort_by(|a, b| a.0.cmp(&b.0));
    for (name, value) in inputs {
        // Range is a guess until a graph declares one: 0 ..= max(4, 2x the default) covers a
        // speed in m/s and a 0..1 factor alike without clipping either.
        let max = (value * 2.0).max(4.0);
        let pin = name.clone();
        let readout = commands
            .spawn((
                Text::new(format!("{name}  {value:.2}")),
                ThemedText,
                GraphInput(pin.clone()),
            ))
            .id();
        let observer_pin = pin.clone();
        let slider_pin = pin.clone();
        let slider = commands
            .spawn_scene(bsn! {
                @FeathersSlider { @min: 0.0, @max: {max} }
                SliderValue({value})
                GraphInput({slider_pin.clone()})
                on(slider_self_update)
            })
            .observe(
                move |change: On<ValueChange<f32>>,
                      mut players: Query<&mut AnimationGraphPlayer>,
                      mut texts: Query<(&mut Text, &GraphInput)>| {
                    let v = change.value;
                    for mut player in &mut players {
                        player.set_input_data(observer_pin.clone(), DataValue::F32(v));
                    }
                    for (mut text, input) in &mut texts {
                        if input.0 == observer_pin {
                            text.0 = format!("{observer_pin}  {v:.2}");
                        }
                    }
                },
            )
            .id();
        let row = commands
            .spawn(Node {
                flex_direction: FlexDirection::Column,
                row_gap: Val::Px(2.0),
                ..default()
            })
            .add_children(&[readout, slider])
            .id();
        rows.push(row);
    }
    commands.entity(host_entity).add_children(&rows);
    commands.remove_resource::<ArmGraph>();
    info!("preview armed, {} f32 inputs", rows.len());
}

/// What a canvas box stands for, and therefore where a drag on it writes back.
#[derive(Clone, Copy, PartialEq, Eq)]
enum BoxKind {
    /// A graph node or an FSM state — the canvas treats them the same, only the draw pass
    /// that produced the box knows which.
    Node(Uuid),
    /// The graph's own inputs — sources, so this rail has only output pins.
    Inputs,
    /// The graph's own outputs — targets, so this rail has only input pins.
    Outputs,
}

/// One pin row, and what a link dropped on it would connect. The `DataSpec` rides along so a
/// wiring drag can reject a mismatch without going back to the graph for the spec — `None` is a
/// TIME pin, which only ever connects to another time pin.
#[derive(Component, Clone, PartialEq)]
enum PinSocket {
    Source(SourcePin, Option<DataSpec>),
    Target(TargetPin, Option<DataSpec>),
    /// A state's outgoing side. A state machine has no pin TYPES — a transition is a
    /// transition — so these carry no `DataSpec` and only ever meet each other.
    StateOut(StateId),
    StateIn(StateId),
}

/// What dropping one pin on another would make.
enum Wire {
    Edge(SourcePin, TargetPin),
    /// Source state, target state.
    Transition(StateId, StateId),
}

impl PinSocket {
    /// A link runs source to target, a time pin only ever meets a time pin, and a state pin
    /// only ever meets a state pin.
    fn connects(&self, other: &Self) -> Option<Wire> {
        match (self, other) {
            (Self::StateOut(from), Self::StateIn(to))
            | (Self::StateIn(to), Self::StateOut(from)) => {
                // A self-transition is a real thing in an FSM, so this does not reject it.
                Some(Wire::Transition(*from, *to))
            }
            (Self::Source(source, a), Self::Target(target, b))
            | (Self::Target(target, b), Self::Source(source, a))
                if a == b =>
            {
                Some(Wire::Edge(source.clone(), target.clone()))
            }
            _ => None,
        }
    }
}

/// One box on the canvas: a node, or one of the two graph-level rails.
struct Boxed {
    pos: Vec2,
    title: String,
    kind: BoxKind,
    /// Left-hand pins, top to bottom: a label and what a link dropped there would connect.
    inputs: Vec<(String, PinSocket)>,
    /// Right-hand pins, top to bottom.
    outputs: Vec<(String, PinSocket)>,
}

impl Boxed {
    fn height(&self) -> f32 {
        let rows = self.inputs.len().max(self.outputs.len()).max(1) as f32;
        HEADER_H + rows * PIN_H + BOX_PAD * 2.0
    }

    /// Centre of pin row `i`, measured down from the box's top edge.
    fn pin_y(&self, i: usize) -> f32 {
        HEADER_H + BOX_PAD + (i as f32 + 0.5) * PIN_H
    }
}

/// A node's pin list, or an empty one if the spec could not be computed (a node pointing at an
/// unloaded sub-graph, say) — a box with no pins still draws, which beats dropping the node.
fn node_spec(
    node: &AnimationNode,
    graphs: &Assets<AnimationGraph>,
    fsms: &Assets<StateMachine>,
) -> NodeSpec {
    node.new_spec(SpecResources {
        graph_assets: graphs,
        fsm_assets: fsms,
    })
    .unwrap_or_default()
}

/// Draw whatever is open: an animation graph, or a state machine.
///
/// One canvas serves both. A graph node and an FSM state are the same THING here — a box with
/// pin rows, a position and a drag handler — so the split is only in what produces the boxes
/// and what resolves the links between them.
fn draw_canvas(
    mut commands: Commands,
    mut view: ResMut<CanvasView>,
    graphs: Res<Assets<AnimationGraph>>,
    fsms: Res<Assets<StateMachine>>,
    clips: Res<Assets<GraphClip>>,
    ragdolls: Res<Assets<Ragdoll>>,
    cursor: Res<TimelineCursor>,
    canvas: Single<(Entity, Option<&Children>), With<Canvas>>,
    curves: Query<(), With<LinkCurve>>,
) {
    if !view.dirty && !view.links_dirty {
        return;
    }
    let links_only = !view.dirty;
    view.links_dirty = false;
    // Endpoints come back in canvas coordinates; the draw pass below maps them to pixels.
    if view.clip.is_some() {
        draw_timeline(&mut commands, &mut view, &clips, &cursor, *canvas);
        return;
    }
    if view.ragdoll.is_some() {
        draw_ragdoll_list(&mut commands, &mut view, &ragdolls, *canvas);
        return;
    }
    let built = match (&view.graph, &view.fsm) {
        (Some(handle), _) => graphs.get(handle).map(|graph| {
            let boxes = graph_boxes(&view, graph, &graphs, &fsms);
            let links = graph_links(graph, &boxes);
            (boxes, links, graph.nodes.len())
        }),
        (_, Some(handle)) => fsms.get(handle).map(|fsm| {
            let boxes = fsm_boxes(&view, fsm);
            let links = fsm_links(fsm, &boxes);
            (boxes, links, fsm.states.len())
        }),
        _ => {
            view.dirty = false;
            return;
        }
    };
    // Still streaming: stay dirty and try again next frame.
    let Some((boxes, links, _)) = built else {
        return;
    };
    view.dirty = false;

    let (canvas_entity, existing) = *canvas;
    if let Some(existing) = existing {
        for child in existing.iter() {
            if !links_only || curves.contains(child) {
                commands.entity(child).despawn();
            }
        }
    }

    let zoom = view.zoom;
    let pan = view.pan;
    let to_px = |p: Vec2| p * zoom + pan;
    // Links first, so the boxes draw over them.
    let curves: Vec<Entity> = links
        .iter()
        .map(|(from, to, color)| curve(&mut commands, to_px(*from), to_px(*to), zoom, *color))
        .collect();
    commands.entity(canvas_entity).insert_children(0, &curves);
    if !links_only {
        let boxes: Vec<Entity> = boxes
            .iter()
            .map(|boxed| spawn_box(&mut commands, boxed, to_px(boxed.pos), zoom))
            .collect();
        commands.entity(canvas_entity).add_children(&boxes);
    }
}

/// A link's curve, so a drag can redraw the links without the boxes.
#[derive(Component)]
struct LinkCurve;

/// A graph's nodes, plus the two graph-level rails.
fn graph_boxes(
    view: &CanvasView,
    graph: &AnimationGraph,
    graphs: &Assets<AnimationGraph>,
    fsms: &Assets<StateMachine>,
) -> Vec<Boxed> {
    let mut boxes = Vec::new();
    // The graph's own inputs are SOURCES on the canvas, and its outputs are TARGETS: the rails
    // are ordinary boxes with one side empty.
    boxes.push(Boxed {
        pos: view.input_pos,
        title: "inputs".into(),
        kind: BoxKind::Inputs,
        inputs: Vec::new(),
        outputs: graph
            .io_spec
            .sorted_inputs()
            .into_iter()
            .map(|input| match input {
                NodeInput::Time(pin) => (
                    format!("t {}", pin_name(&pin)),
                    PinSocket::Source(SourcePin::InputTime(pin), None),
                ),
                NodeInput::Data(pin, spec) => (
                    pin_name(&pin),
                    PinSocket::Source(SourcePin::InputData(pin), Some(spec)),
                ),
            })
            .collect(),
    });
    boxes.push(Boxed {
        pos: view.output_pos,
        title: "outputs".into(),
        kind: BoxKind::Outputs,
        inputs: graph
            .io_spec
            .sorted_outputs()
            .into_iter()
            .map(|output| match output {
                NodeOutput::Time => (
                    "t".to_string(),
                    PinSocket::Target(TargetPin::OutputTime, None),
                ),
                NodeOutput::Data(pin, spec) => (
                    pin.clone(),
                    PinSocket::Target(TargetPin::OutputData(pin), Some(spec)),
                ),
            })
            .collect(),
        outputs: Vec::new(),
    });

    let mut ids: Vec<NodeId> = graph.nodes.keys().copied().collect();
    ids.sort_by_key(|id| format!("{id:?}"));
    for id in &ids {
        let Some(node) = graph.nodes.get(id) else {
            continue;
        };
        let spec = node_spec(node, graphs, fsms);
        boxes.push(Boxed {
            pos: view
                .positions
                .get(&id.uuid())
                .copied()
                .unwrap_or(Vec2::ZERO),
            title: if node.name.is_empty() {
                ascii(&node.inner.display_name())
            } else {
                format!("{}  ({})", node.name, ascii(&node.inner.display_name()))
            },
            kind: BoxKind::Node(id.uuid()),
            inputs: spec
                .sorted_inputs()
                .into_iter()
                .map(|input| match input {
                    NodeInput::Time(pin) => (
                        format!("t {pin}"),
                        PinSocket::Target(TargetPin::NodeTime(*id, pin), None),
                    ),
                    NodeInput::Data(pin, spec) => (
                        pin.clone(),
                        PinSocket::Target(TargetPin::NodeData(*id, pin), Some(spec)),
                    ),
                })
                .collect(),
            outputs: spec
                .sorted_outputs()
                .into_iter()
                .map(|output| match output {
                    NodeOutput::Time => (
                        "t".to_string(),
                        PinSocket::Source(SourcePin::NodeTime(*id), None),
                    ),
                    NodeOutput::Data(pin, spec) => (
                        pin.clone(),
                        PinSocket::Source(SourcePin::NodeData(*id, pin), Some(spec)),
                    ),
                })
                .collect(),
        });
    }
    boxes
}

/// One resolved link per edge: `(from, to, colour)` in canvas coordinates.
fn graph_links(graph: &AnimationGraph, boxes: &[Boxed]) -> Vec<(Vec2, Vec2, Color)> {
    let mut source_at: std::collections::HashMap<&SourcePin, Vec2> =
        std::collections::HashMap::new();
    let mut target_at: std::collections::HashMap<&TargetPin, Vec2> =
        std::collections::HashMap::new();
    for boxed in boxes {
        for (i, (_, socket)) in boxed.inputs.iter().enumerate() {
            if let PinSocket::Target(pin, _) = socket {
                target_at.insert(pin, boxed.pos + Vec2::new(0.0, boxed.pin_y(i)));
            }
        }
        for (i, (_, socket)) in boxed.outputs.iter().enumerate() {
            if let PinSocket::Source(pin, _) = socket {
                source_at.insert(pin, boxed.pos + Vec2::new(NODE_W, boxed.pin_y(i)));
            }
        }
    }
    graph
        .edges_inverted
        .iter()
        .filter_map(|(target, source)| {
            let (from, to) = (source_at.get(source)?, target_at.get(target)?);
            // Time edges carry the clock, data edges carry poses and values: colour them
            // apart, so the timing spine of a graph is visible at a glance.
            let is_time = matches!(target, TargetPin::NodeTime(..) | TargetPin::OutputTime);
            let color = if is_time {
                Color::srgba(0.85, 0.62, 0.30, 0.85)
            } else {
                Color::srgba(0.35, 0.55, 0.85, 0.85)
            };
            Some((*from, *to, color))
        })
        .collect()
}

/// A state machine's states. Each is one box with one pin a side — a transition has no type to
/// agree on, so there is nothing else to draw.
fn fsm_boxes(view: &CanvasView, fsm: &StateMachine) -> Vec<Boxed> {
    let mut ids: Vec<StateId> = fsm.states.keys().copied().collect();
    ids.sort_by_key(|id| format!("{id:?}"));
    ids.iter()
        .filter_map(|id| {
            let state = fsm.states.get(id)?;
            let start = fsm.start_state == *id;
            Some(Boxed {
                pos: view
                    .positions
                    .get(&id.uuid())
                    .copied()
                    .unwrap_or(Vec2::ZERO),
                // The start state is where the machine begins and there is exactly one, so it
                // is worth seeing without opening the inspector.
                title: if start {
                    format!("{}  (start)", state.label)
                } else {
                    state.label.clone()
                },
                kind: BoxKind::Node(id.uuid()),
                inputs: vec![("in".to_string(), PinSocket::StateIn(*id))],
                outputs: vec![("out".to_string(), PinSocket::StateOut(*id))],
            })
        })
        .collect()
}

/// One resolved link per transition.
fn fsm_links(fsm: &StateMachine, boxes: &[Boxed]) -> Vec<(Vec2, Vec2, Color)> {
    let mut out_at: std::collections::HashMap<StateId, Vec2> = std::collections::HashMap::new();
    let mut in_at: std::collections::HashMap<StateId, Vec2> = std::collections::HashMap::new();
    for boxed in boxes {
        for (i, (_, socket)) in boxed.inputs.iter().enumerate() {
            if let PinSocket::StateIn(id) = socket {
                in_at.insert(*id, boxed.pos + Vec2::new(0.0, boxed.pin_y(i)));
            }
        }
        for (i, (_, socket)) in boxed.outputs.iter().enumerate() {
            if let PinSocket::StateOut(id) = socket {
                out_at.insert(*id, boxed.pos + Vec2::new(NODE_W, boxed.pin_y(i)));
            }
        }
    }
    fsm.transitions
        .values()
        .filter_map(|transition| {
            let from = out_at.get(&transition.source)?;
            let to = in_at.get(&transition.target)?;
            Some((*from, *to, Color::srgba(0.62, 0.48, 0.85, 0.9)))
        })
        .collect()
}

/// One box: a title strip, then the input pins down the left and the output pins down the
/// right. Dragging a node box moves it and stops there, so the canvas does not also pan.
fn spawn_box(commands: &mut Commands, boxed: &Boxed, px: Vec2, zoom: f32) -> Entity {
    let font = |size: f32| TextFont {
        font_size: FontSize::Px(size * zoom),
        ..default()
    };
    // A pin row is the wiring handle: drag one onto another to make a link, right-click an
    // input to cut the link into it. Every pointer event it handles stops propagating, or the
    // box underneath would move with the drag and the canvas would pan behind that.
    let pin_row = |commands: &mut Commands, label: &str, socket: PinSocket, right: bool| {
        let color = pin_color(&socket);
        let row = commands
            .spawn((
                Node {
                    height: Val::Px(PIN_H * zoom),
                    align_items: AlignItems::Center,
                    justify_content: if right {
                        JustifyContent::FlexEnd
                    } else {
                        JustifyContent::FlexStart
                    },
                    padding: UiRect::horizontal(Val::Px(6.0 * zoom)),
                    overflow: Overflow::clip(),
                    ..default()
                },
                BackgroundColor(Color::NONE),
                socket.clone(),
                Children::spawn((
                    Spawn((
                        Text::new(label.to_string()),
                        ThemedText,
                        font(10.0),
                        TextLayout {
                            linebreak: bevy::text::LineBreak::NoWrap,
                            ..default()
                        },
                    )),
                    // Filled every frame from the running graph's own cache. Empty on an
                    // input pin, which has no value of its own — it shows whatever its edge
                    // brought, which is already on the far end of the link.
                    Spawn((
                        Text::new(String::new()),
                        PinValue(socket),
                        TextColor(Color::srgb(0.55, 0.82, 0.62)),
                        font(10.0),
                        Node {
                            margin: UiRect::left(Val::Px(6.0 * zoom)),
                            ..default()
                        },
                        TextLayout {
                            linebreak: bevy::text::LineBreak::NoWrap,
                            ..default()
                        },
                    )),
                )),
            ))
            .observe(
                |mut start: On<PointerDragStart>,
                 mut wiring: ResMut<Wiring>,
                 mut wire_start: ResMut<WireStart>,
                 sockets: Query<&PinSocket>,
                 parents: Query<&ChildOf>,
                 windows: Query<&Window>,
                 canvases: Query<(&ComputedNode, &bevy::ui::UiGlobalTransform), With<Canvas>>| {
                    if start.button != PointerButton::Primary {
                        return;
                    }
                    start.propagate(false);
                    wiring.0 = std::iter::once(start.entity)
                        .chain(parents.iter_ancestors(start.entity))
                        .find_map(|e| sockets.get(e).ok().cloned());
                    let outgoing = matches!(
                        wiring.0,
                        Some(PinSocket::Source(..) | PinSocket::StateOut(_))
                    );
                    wire_start.0 = canvas_cursor(&windows, &canvases).map(|at| (at, outgoing));
                },
            )
            .observe(|mut drag: On<PointerDrag>| drag.propagate(false))
            .observe(|mut end: On<PointerDragEnd>, mut wiring: ResMut<Wiring>, mut wire_start: ResMut<WireStart>| {
                end.propagate(false);
                wiring.0 = None;
                wire_start.0 = None;
            })
            // The drop lands on the pin under the cursor and names the pin the drag began on,
            // so both ends arrive in one event and no pending-link bookkeeping is needed.
            .observe(
                |mut drop: On<PointerDragDrop>,
                 sockets: Query<&PinSocket>,
                 parents: Query<&ChildOf>,
                 mut wiring: ResMut<Wiring>,
                 mut wire_start: ResMut<WireStart>,
                 mut commands: Commands| {
                    drop.propagate(false);
                    wire_start.0 = None;
                    // The pin the drag began on was recorded at its start: the entity the drag
                    // names is whatever was hit, usually the pin's label.
                    let from = wiring.0.take();
                    let onto = std::iter::once(drop.entity)
                        .chain(parents.iter_ancestors(drop.entity))
                        .find_map(|e| sockets.get(e).ok());
                    let (Some(from), Some(onto)) = (from, onto) else {
                        return;
                    };
                    let Some(wire) = from.connects(onto) else {
                        info!("wiring: those two pins do not connect");
                        return;
                    };
                    commands.queue(move |world: &mut World| match wire {
                        Wire::Edge(source, target) => document::connect(world, source, target),
                        Wire::Transition(from, to) => {
                            document::add_transition(world, from.uuid(), to.uuid());
                        }
                    });
                },
            )
            .observe(
                |mut click: On<PointerClick>, sockets: Query<&PinSocket>, mut commands: Commands| {
                    if click.button != PointerButton::Secondary {
                        return;
                    }
                    click.propagate(false);
                    let Ok(PinSocket::Target(target, _)) = sockets.get(click.entity) else {
                        return;
                    };
                    let target = target.clone();
                    commands.queue(move |world: &mut World| document::cut(world, target));
                },
            )
            .id();
        // The pin's dot, on the box's outer edge: where a wire starts and lands.
        let dot = commands
            .spawn((
                Node {
                    width: Val::Px(8.0 * zoom),
                    height: Val::Px(8.0 * zoom),
                    flex_shrink: 0.0,
                    margin: if right {
                        UiRect::left(Val::Px(5.0 * zoom))
                    } else {
                        UiRect::right(Val::Px(5.0 * zoom))
                    },
                    border_radius: BorderRadius::MAX,
                    ..default()
                },
                BackgroundColor(color),
            ))
            .id();
        if right {
            commands.entity(row).add_child(dot);
        } else {
            commands.entity(row).insert_children(0, &[dot]);
        }
        row
    };

    let column = |commands: &mut Commands, pins: Vec<(String, PinSocket)>, right: bool| {
        let rows: Vec<Entity> = pins
            .into_iter()
            .map(|(label, socket)| pin_row(commands, &label, socket, right))
            .collect();
        commands
            .spawn(Node {
                flex_grow: 1.0,
                flex_basis: Val::Px(0.0),
                flex_direction: FlexDirection::Column,
                ..default()
            })
            .add_children(&rows)
            .id()
    };

    let left = column(commands, boxed.inputs.clone(), false);
    let right = column(commands, boxed.outputs.clone(), true);

    let header = commands
        .spawn((
            Node {
                height: Val::Px(HEADER_H * zoom),
                align_items: AlignItems::Center,
                padding: UiRect::horizontal(Val::Px(7.0 * zoom)),
                overflow: Overflow::clip(),
                ..default()
            },
            BackgroundColor(match boxed.kind {
                BoxKind::Node(_) => Color::srgba(0.22, 0.25, 0.32, 1.0),
                // The rails are the graph's own boundary; warm them so they read as different.
                _ => Color::srgba(0.30, 0.25, 0.17, 1.0),
            }),
            Children::spawn(Spawn((
                Text::new(boxed.title.clone()),
                ThemedText,
                font(12.0),
                TextLayout {
                    linebreak: bevy::text::LineBreak::NoWrap,
                    ..default()
                },
            ))),
        ))
        .id();
    let body = commands
        .spawn(Node {
            flex_grow: 1.0,
            flex_direction: FlexDirection::Row,
            padding: UiRect::vertical(Val::Px(BOX_PAD * zoom)),
            ..default()
        })
        .add_children(&[left, right])
        .id();

    let entity = commands
        .spawn((
            Node {
                position_type: PositionType::Absolute,
                left: Val::Px(px.x),
                top: Val::Px(px.y),
                width: Val::Px(NODE_W * zoom),
                height: Val::Px(boxed.height() * zoom),
                flex_direction: FlexDirection::Column,
                overflow: Overflow::clip(),
                ..default()
            },
            BackgroundColor(BOX_IDLE),
        ))
        .add_children(&[header, body])
        .id();

    // The rails drag too — they are boxes like any other, they just write a different field.
    let kind = boxed.kind;
    commands.entity(entity).observe(
        move |mut drag: On<PointerDrag>,
              mut view: ResMut<CanvasView>,
              mut nodes: Query<&mut Node>| {
            if drag.button != PointerButton::Primary {
                return;
            }
            // Without this the drag also reaches the canvas and the whole view pans with it.
            drag.propagate(false);
            let delta = drag.delta / view.zoom;
            match kind {
                BoxKind::Node(id) => {
                    *view.positions.entry(id).or_default() += delta;
                }
                BoxKind::Inputs => view.input_pos += delta,
                BoxKind::Outputs => view.output_pos += delta,
            }
            // Move this box where it stands and redraw only the links: respawning the box
            // would end the drag after one step.
            if let Ok(mut node) = nodes.get_mut(entity) {
                if let Val::Px(left) = node.left {
                    node.left = Val::Px(left + drag.delta.x);
                }
                if let Val::Px(top) = node.top {
                    node.top = Val::Px(top + drag.delta.y);
                }
            }
            view.links_dirty = true;
        },
    );
    // The drag moved the box on screen only; letting go writes it to the document.
    commands.entity(entity).observe(
        move |end: On<PointerDragEnd>, view: Res<CanvasView>, mut commands: Commands| {
            if end.button != PointerButton::Primary {
                return;
            }
            let (pos, input_pos, output_pos) =
                (view.positions.clone(), view.input_pos, view.output_pos);
            commands.queue(move |world: &mut World| match kind {
                BoxKind::Node(id) => {
                    if let Some(at) = pos.get(&id) {
                        document::commit_node_position(world, id, *at);
                    }
                }
                BoxKind::Inputs => document::commit_rail_position(world, true, input_pos),
                BoxKind::Outputs => document::commit_rail_position(world, false, output_pos),
            });
        },
    );
    if matches!(kind, BoxKind::Inputs | BoxKind::Outputs) {
        // The rails are the graph's own pins: clicking one puts the graph in the inspector.
        commands.entity(entity).observe(move |mut click: On<PointerClick>, mut commands: Commands| {
            if click.button != PointerButton::Primary {
                return;
            }
            click.propagate(false);
            commands.queue(document::select_root);
        });
    }
    if let BoxKind::Node(id) = kind {
        commands.entity(entity).insert(CanvasNode(id)).observe(
            move |mut click: On<PointerClick>, mut commands: Commands| {
                if click.button != PointerButton::Primary {
                    return;
                }
                click.propagate(false);
                let double = click.count >= 2;
            commands.queue(move |world: &mut World| {
                document::select_node(world, id);
                if double {
                    open_clip_timeline(world, id);
                }
            });
            },
        );
    }
    entity
}

/// A pin's colour, by what flows through it: time, poses, events, numbers.
fn pin_color(socket: &PinSocket) -> Color {
    let spec = match socket {
        PinSocket::Source(_, spec) | PinSocket::Target(_, spec) => *spec,
        PinSocket::StateIn(_) | PinSocket::StateOut(_) => {
            return Color::srgb(0.62, 0.48, 0.85);
        }
    };
    match spec {
        None => Color::srgb(0.85, 0.62, 0.30),
        Some(DataSpec::Pose) => Color::srgb(0.35, 0.55, 0.85),
        Some(DataSpec::EventQueue) => Color::srgb(0.85, 0.45, 0.55),
        Some(DataSpec::F32) => Color::srgb(0.45, 0.78, 0.50),
        Some(DataSpec::Bool) => Color::srgb(0.85, 0.35, 0.35),
        Some(DataSpec::Vec2 | DataSpec::Vec3 | DataSpec::Quat) => Color::srgb(0.85, 0.80, 0.35),
        Some(_) => Color::srgb(0.65, 0.65, 0.65),
    }
}

/// Where a wiring drag began, in canvas pixels, and whether it leaves rightwards (an output).
#[derive(Resource, Default)]
struct WireStart(Option<(Vec2, bool)>);

/// The wire drawn from the pin being dragged to the cursor.
#[derive(Component)]
struct WirePreview;

/// The cursor in canvas pixels, when there is a canvas and a cursor.
fn canvas_cursor(
    windows: &Query<&Window>,
    canvases: &Query<(&ComputedNode, &bevy::ui::UiGlobalTransform), With<Canvas>>,
) -> Option<Vec2> {
    let cursor = windows.iter().find_map(Window::cursor_position)?;
    let (computed, transform) = canvases.iter().next()?;
    let scale = computed.inverse_scale_factor();
    Some(cursor - (transform.translation * scale - computed.size * scale * 0.5))
}

/// Draw the wire in flight from where the drag began to the cursor.
fn draw_wire_preview(
    mut commands: Commands,
    wire_start: Res<WireStart>,
    view: Res<CanvasView>,
    windows: Query<&Window>,
    canvases: Query<(&ComputedNode, &bevy::ui::UiGlobalTransform), With<Canvas>>,
    canvas: Query<Entity, With<Canvas>>,
    mut preview: Query<(Entity, &mut UiPolyline), With<WirePreview>>,
) {
    let (Some((from, outgoing)), Some(to)) = (wire_start.0, canvas_cursor(&windows, &canvases))
    else {
        for (entity, _) in &preview {
            commands.entity(entity).despawn();
        }
        return;
    };
    let reach = (from.distance(to) * 0.4).clamp(30.0, 160.0) * if outgoing { 1.0 } else { -1.0 };
    let points = UiPolyline::bezier(
        from,
        from + Vec2::new(reach, 0.0),
        to - Vec2::new(reach, 0.0),
        to,
        20,
    );
    if let Ok((_, mut line)) = preview.single_mut() {
        line.points = points;
        return;
    }
    let Ok(canvas) = canvas.single() else {
        return;
    };
    commands.spawn((
        WirePreview,
        Node {
            position_type: PositionType::Absolute,
            left: Val::Px(0.0),
            top: Val::Px(0.0),
            right: Val::Px(0.0),
            bottom: Val::Px(0.0),
            ..default()
        },
        UiPolyline {
            points,
            thickness: LINK_THICKNESS * view.zoom,
            color: Color::srgba(0.9, 0.9, 0.9, 0.8),
            closed: false,
        },
        Pickable::IGNORE,
        ChildOf(canvas),
    ));
}

/// The pin a wiring drag started from, while it is in flight.
#[derive(Resource, Default)]
struct Wiring(Option<PinSocket>);

/// The canvas box colours, picked apart enough that a selected node reads at a glance.
const BOX_IDLE: Color = Color::srgba(0.13, 0.14, 0.17, 0.97);
const BOX_SELECTED: Color = Color::srgba(0.20, 0.26, 0.34, 0.99);

/// The value slot on a pin row.
#[derive(Component)]
struct PinValue(PinSocket);

/// Write each output pin's CURRENT value onto its row.
///
/// This is what turns the canvas from a diagram into a debugger: drag `speed` and watch
/// `fac_walk`, `fac_jog` and `rate` move on the boxes that compute them. The graph already
/// caches every pin it evaluates — `node_caches`, keyed by `(StateKey, PinId)` — so this reads
/// what the pose was actually built from rather than recomputing anything beside it.
///
/// The cache is cleared per frame, so a pin that did not take part in THIS frame's evaluation
/// reads empty rather than stale. That is the honest answer: a node behind a zero-weight blend
/// genuinely did not run.
fn show_pin_values(
    preview: Res<Preview>,
    players: Query<&AnimationGraphPlayer>,
    mut values: Query<(&mut Text, &PinValue)>,
) {
    let Some(player) = preview.armature.and_then(|a| players.get(a).ok()) else {
        return;
    };
    let Some(arena) = player.get_context_arena() else {
        return;
    };
    let caches = &arena.get_toplevel().node_caches;
    for (mut text, slot) in &mut values {
        // Inputs and time pins carry nothing of their own.
        let PinSocket::Source(SourcePin::NodeData(node, pin), _) = &slot.0 else {
            continue;
        };
        let shown = caches
            .get_output_data(*node, StateKey::Default, pin.clone())
            .map(|value| short_value(&value))
            .unwrap_or_default();
        if text.0 != shown {
            text.0 = shown;
        }
    }
}

/// A `DataValue` in as few characters as a pin row can spare.
fn short_value(value: &DataValue) -> String {
    match value {
        DataValue::F32(v) => format!("{v:.2}"),
        DataValue::Bool(v) => (if *v { "T" } else { "F" }).to_string(),
        DataValue::Vec3(v) => format!("{:.1},{:.1},{:.1}", v.x, v.y, v.z),
        // A pose or an event queue has no short form worth the width, and the link already
        // shows that one is flowing.
        _ => String::new(),
    }
}

/// While a wire is in flight, light up every pin it could legally land on.
///
/// This is the feedback instead of a rubber band: bevy_ui cannot draw a line to the cursor
/// without a per-frame respawn, and showing where a drop WOULD take is more use than showing
/// where the cursor already is.
fn highlight_pins(wiring: Res<Wiring>, mut pins: Query<(&PinSocket, &mut BackgroundColor)>) {
    for (socket, mut background) in &mut pins {
        let want = match &wiring.0 {
            Some(from) if from.connects(socket).is_some() => Color::srgba(0.30, 0.55, 0.35, 0.8),
            _ => Color::NONE,
        };
        if background.0 != want {
            background.0 = want;
        }
    }
}

/// Tint the selected node's box. Guarded on the current value rather than run on a change
/// filter, because `draw_canvas` respawns every box and the new ones start idle.
fn highlight_selected(
    selection: Res<crate::selection::Selection>,
    doc: Res<ActiveGraphDoc>,
    mut boxes: Query<(&CanvasNode, &mut BackgroundColor)>,
) {
    let selected: Vec<Uuid> = doc
        .0
        .as_ref()
        .map(|doc| {
            doc.nodes
                .iter()
                .filter(|(_, (entity, _))| selection.entities.contains(entity))
                .map(|(id, _)| *id)
                .collect()
        })
        .unwrap_or_default();
    for (node, mut background) in &mut boxes {
        let want = if selected.contains(&node.0) {
            BOX_SELECTED
        } else {
            BOX_IDLE
        };
        if background.0 != want {
            background.0 = want;
        }
    }
}

/// Point the inspector at the selected node's parameters.
///
/// `AnimationGraph::nodes` is `#[reflect(ignore)]`, and `DynNodeLike` — the `Box<dyn NodeLike>`
/// each node body sits in — carries a hand-written `Reflect` impl reporting ZERO fields, so no
/// reflection path from the asset root reaches a node. That is what `InspectorRoot::Custom`
/// exists for: a pair of plain `fn`s that walk the world to the value themselves, after which
/// the inspector's ordinary recursion (nested structs, enum variant pickers, sliders, writeback)
/// applies below it unchanged. `NodeLike: Reflect`, so what they hand back is the CONCRETE node
/// struct.
fn show_node_params(
    mut commands: Commands,
    mut selected: ResMut<Selected>,
    view: Res<CanvasView>,
    host: Single<Entity, With<NodeParamsHost>>,
) {
    if !selected.dirty {
        return;
    }
    selected.dirty = false;
    if view.clip.is_some() {
        commands.queue(BuildCustomInspector {
            read: read_selected_item,
            write: write_selected_item,
            panel: *host,
        });
        return;
    }
    if view.ragdoll.is_some() {
        commands.queue(BuildCustomInspector {
            read: read_selected_body,
            write: write_selected_body,
            panel: *host,
        });
        return;
    }
}

/// Which track item the two resolvers below are pointed at.
///
/// `event_tracks` is a `HashMap<String, EventTrack>` and the item inside is found by id, and a
/// `ParsedPath` can spell neither — which is exactly the case `InspectorRoot::Custom` exists
/// for. The same escape hatch the node bodies needed, for a completely different reason.
fn selected_item(world: &World) -> Option<(String, Uuid, Handle<GraphClip>)> {
    let selected = world.get_resource::<Selected>()?;
    let track = selected.track.clone()?;
    let id = selected.node?;
    let handle = world.get_resource::<CanvasView>()?.clip.clone()?;
    Some((track, id, handle))
}

fn read_selected_item(world: &World, visit: &mut dyn FnMut(&dyn Reflect)) {
    let Some((track, id, handle)) = selected_item(world) else {
        return;
    };
    let Some(clips) = world.get_resource::<Assets<GraphClip>>() else {
        return;
    };
    let Some(item) = clips
        .get(&handle)
        .and_then(|clip| clip.event_tracks.get(&track))
        .and_then(|track| track.events.iter().find(|e| e.id == id))
    else {
        return;
    };
    visit(item.value.as_reflect());
}

fn write_selected_item(world: &mut World, visit: &mut dyn FnMut(&mut dyn Reflect)) {
    let Some((track, id, handle)) = selected_item(world) else {
        return;
    };
    let Some(mut clips) = world.get_resource_mut::<Assets<GraphClip>>() else {
        return;
    };
    let Some(mut clip) = clips.get_mut(&handle) else {
        return;
    };
    let Some(item) = clip
        .event_tracks
        .get_mut(&track)
        .and_then(|track| track.events.iter_mut().find(|e| e.id == id))
    else {
        return;
    };
    visit(item.value.as_reflect_mut());
    // An edited start time can reorder the track, which is kept sorted.
    if let Some(track) = clip.event_tracks.get_mut(&track) {
        track
            .events
            .sort_by(|a, b| a.value.start_time.total_cmp(&b.value.start_time));
    }
}

/// One cubic bezier per link, as an aurora [`UiPolyline`] — a single entity, where the
/// manhattan routing this replaced took three.
///
/// The curve leaves the source pin horizontally and arrives at the target the same way, which
/// is the convention every node editor uses, and it is why the control offset grows with the
/// horizontal gap: a short link stays taut, a long one bows. A BACKWARD link (the target sits
/// left of the source, a feedback edge) gets a wide offset instead, so it bulges out around the
/// boxes rather than doubling back through them.
fn curve(commands: &mut Commands, from: Vec2, to: Vec2, zoom: f32, color: Color) -> Entity {
    let gap = to.x - from.x;
    let reach = if gap > 0.0 {
        (gap * 0.5).clamp(30.0 * zoom, 180.0 * zoom)
    } else {
        (60.0 - gap * 0.35).min(320.0) * zoom
    };
    let points = UiPolyline::bezier(
        from,
        from + Vec2::new(reach, 0.0),
        to - Vec2::new(reach, 0.0),
        to,
        // Enough segments that the longest link reads as a curve, few enough that a canvas of
        // forty does not become ten thousand quads.
        ((from.distance(to) / 14.0) as usize).clamp(8, 28),
    );
    commands
        .spawn((
            // The polyline's points are in ITS node's space, so the node spans the canvas and
            // the points are the canvas pixels already computed.
            Node {
                position_type: PositionType::Absolute,
                left: Val::Px(0.0),
                top: Val::Px(0.0),
                right: Val::Px(0.0),
                bottom: Val::Px(0.0),
                ..default()
            },
            UiPolyline {
                points,
                thickness: LINK_THICKNESS * zoom,
                color,
                closed: false,
            },
            LinkCurve,
            // A full-canvas node would otherwise swallow every background drag meant to pan.
            Pickable::IGNORE,
        ))
        .id()
}

/// Ctrl+S: write the open clip or ragdoll back to its `.ron`. Graphs and state machines are
/// documents and save with the scene. A `.bak` is left the first time, because a hand-authored
/// file's inner comments do not survive a round trip through the serializer.
fn save_graph(
    keys: Res<ButtonInput<KeyCode>>,
    view: Res<CanvasView>,
    clips: Res<Assets<GraphClip>>,
    ragdolls: Res<Assets<Ragdoll>>,
) {
    if !keys.just_pressed(KeyCode::KeyS)
        || !(keys.pressed(KeyCode::ControlLeft) || keys.pressed(KeyCode::ControlRight))
    {
        return;
    }
    write_graph(&view, &clips, &ragdolls);
}

/// Serialize the open clip or ragdoll.
fn write_graph(view: &CanvasView, clips: &Assets<GraphClip>, ragdolls: &Assets<Ragdoll>) {
    if view.clip.is_some() {
        write_clip(view, clips);
        return;
    }
    if view.ragdoll.is_some() {
        write_ragdoll(view, ragdolls);
    }
}

/// A round trip through the serializer drops every comment in the file. The header block is
/// where an authored graph keeps its design record — the blend formulas, in zero's case — so
/// carry that much across; comments further in are lost, and the `.bak` is the recourse.
fn leading_comment(existing: &str) -> String {
    let header: Vec<&str> = existing
        .lines()
        .take_while(|line| line.trim_start().starts_with("//") || line.trim().is_empty())
        .collect();
    if header.iter().all(|line| line.trim().is_empty()) {
        return String::new();
    }
    format!("{}\n", header.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy_animation_graph::core::edge_data::DataSpec;

    fn node(id: u128) -> NodeId {
        Uuid::from_u128(id).into()
    }

    #[test]
    fn a_wire_runs_source_to_target_and_matches_type() {
        let pose = Some(DataSpec::Pose);
        let float = Some(DataSpec::F32);
        let out = PinSocket::Source(SourcePin::NodeData(node(1), "pose".into()), pose);
        let into = PinSocket::Target(TargetPin::NodeData(node(2), "in".into()), pose);
        let wrong_type = PinSocket::Target(TargetPin::NodeData(node(2), "f".into()), float);
        let time_in = PinSocket::Target(TargetPin::NodeTime(node(2), "t".into()), None);

        assert!(out.connects(&into).is_some());
        // Dropping the other way round is the same link, so it is allowed.
        assert!(into.connects(&out).is_some());
        // Two outputs, or two inputs, are not a link.
        assert!(out.connects(&out.clone()).is_none());
        assert!(into.connects(&wrong_type).is_none());
        // Type mismatch, and a data pin never meets a time pin.
        assert!(out.connects(&wrong_type).is_none());
        assert!(out.connects(&time_in).is_none());
    }

}

// ------------------------------------------------------------------ timeline

/// Where the scrub cursor sits, in seconds.
///
/// A clip has no player here — `AnimationSource` is Graph, Pose or None, with no clip variant —
/// so this is a cursor you place rather than a playhead that follows something. For authoring
/// event times that is the right way round anyway.
#[derive(Resource, Default)]
struct TimelineCursor(f32);

/// One event bar, so a click knows which item in which track it landed on.
#[derive(Component, Clone)]
struct TrackBar {
    track: String,
    item: Uuid,
}

/// Lane geometry, in pixels.
const RULER_H: f32 = 22.0;
const LANE_H: f32 = 28.0;
const LANE_GAP: f32 = 4.0;
/// Track names sit in a fixed gutter, so they stay readable however far the time axis scrolls.
const LABEL_W: f32 = 130.0;
/// Seconds-to-pixels at zoom 1. A 1.3 s walk cycle is then about 290 px wide.
const PX_PER_SEC: f32 = 220.0;

/// The event tracks of the open clip, as lanes of bars along a time axis.
fn draw_timeline(
    commands: &mut Commands,
    view: &mut CanvasView,
    clips: &Assets<GraphClip>,
    cursor: &TimelineCursor,
    canvas: (Entity, Option<&Children>),
) {
    let Some(clip) = view.clip.clone().and_then(|h| clips.get(&h)) else {
        // Still streaming; stay dirty.
        return;
    };
    view.dirty = false;
    let (canvas_entity, existing) = canvas;
    if let Some(existing) = existing {
        for child in existing.iter() {
            commands.entity(child).despawn();
        }
    }

    let scale = PX_PER_SEC * view.zoom;
    let x_of = |t: f32| LABEL_W + t * scale + view.pan.x;
    let mut children = Vec::new();

    // Ruler: ticks at a round interval that stays about 80 px apart whatever the zoom, so the
    // labels never collide and never thin out to uselessness.
    let step = [0.05f32, 0.1, 0.25, 0.5, 1.0, 2.0, 5.0]
        .into_iter()
        .find(|s| s * scale >= 80.0)
        .unwrap_or(10.0);
    let mut t = 0.0;
    while t <= clip.duration + step * 0.5 {
        let x = x_of(t);
        children.push(
            commands
                .spawn((
                    Node {
                        position_type: PositionType::Absolute,
                        left: Val::Px(x),
                        top: Val::Px(view.pan.y),
                        width: Val::Px(1.0),
                        height: Val::Px(RULER_H),
                        ..default()
                    },
                    BackgroundColor(Color::srgba(0.45, 0.47, 0.52, 0.9)),
                    Pickable::IGNORE,
                ))
                .id(),
        );
        children.push(
            commands
                .spawn((
                    Node {
                        position_type: PositionType::Absolute,
                        left: Val::Px(x + 3.0),
                        top: Val::Px(view.pan.y),
                        ..default()
                    },
                    Text::new(format!("{t:.2}")),
                    ThemedText,
                    TextFont {
                        font_size: FontSize::Px(10.0),
                        ..default()
                    },
                    Pickable::IGNORE,
                ))
                .id(),
        );
        t += step;
    }

    // Which clip this is, and the way back when it was opened from a graph.
    if let Some(path) = &view.path {
        children.push(
            commands
                .spawn((
                    Node {
                        position_type: PositionType::Absolute,
                        right: Val::Px(10.0),
                        top: Val::Px(6.0),
                        ..default()
                    },
                    Text::new(format!("{path}   Ctrl+S: save   Esc: back to the graph")),
                    ThemedText,
                    TextFont {
                        font_size: FontSize::Px(11.0),
                        ..default()
                    },
                    Pickable::IGNORE,
                ))
                .id(),
        );
    }

    // The name gutter is drawn first and full height: the preview renders behind the canvas,
    // and pale text straight onto a rig is unreadable.
    children.push(
        commands
            .spawn((
                Node {
                    position_type: PositionType::Absolute,
                    left: Val::Px(0.0),
                    top: Val::Px(0.0),
                    width: Val::Px(LABEL_W),
                    bottom: Val::Px(0.0),
                    ..default()
                },
                BackgroundColor(Color::srgba(0.08, 0.09, 0.11, 0.92)),
                Pickable::IGNORE,
            ))
            .id(),
    );

    // One lane per track, name-sorted so the order is stable between runs.
    let mut names: Vec<&String> = clip.event_tracks.keys().collect();
    names.sort();
    let mut bars = 0;
    for (lane, name) in names.iter().enumerate() {
        let Some(track) = clip.event_tracks.get(*name) else {
            continue;
        };
        let top = view.pan.y + RULER_H + LANE_GAP + lane as f32 * (LANE_H + LANE_GAP);
        // Lane background, full width, so an empty track still reads as a track.
        children.push(
            commands
                .spawn((
                    Node {
                        position_type: PositionType::Absolute,
                        left: Val::Px(0.0),
                        right: Val::Px(0.0),
                        top: Val::Px(top),
                        height: Val::Px(LANE_H),
                        ..default()
                    },
                    BackgroundColor(Color::srgba(0.11, 0.12, 0.14, 0.75)),
                    Pickable::IGNORE,
                ))
                .id(),
        );
        children.push(
            commands
                .spawn((
                    Node {
                        position_type: PositionType::Absolute,
                        left: Val::Px(8.0),
                        top: Val::Px(top + 6.0),
                        width: Val::Px(LABEL_W - 12.0),
                        overflow: Overflow::clip(),
                        ..default()
                    },
                    Text::new((*name).clone()),
                    ThemedText,
                    TextFont {
                        font_size: FontSize::Px(11.0),
                        ..default()
                    },
                    TextLayout {
                        linebreak: bevy::text::LineBreak::NoWrap,
                        ..default()
                    },
                    Pickable::IGNORE,
                ))
                .id(),
        );

        for item in &track.events {
            let start = x_of(item.value.start_time);
            // An instantaneous event has start == end and would otherwise be invisible.
            let width = ((item.value.end_time - item.value.start_time) * scale).max(6.0);
            children.push(spawn_track_bar(commands, name, item, start, top, width));
            bars += 1;
        }
    }

    // The cursor last, so it draws over the bars it is being placed against.
    children.push(
        commands
            .spawn((
                Node {
                    position_type: PositionType::Absolute,
                    left: Val::Px(x_of(cursor.0)),
                    top: Val::Px(view.pan.y),
                    width: Val::Px(1.0),
                    bottom: Val::Px(0.0),
                    ..default()
                },
                BackgroundColor(Color::srgba(0.95, 0.85, 0.35, 0.95)),
                Pickable::IGNORE,
            ))
            .id(),
    );

    commands.entity(canvas_entity).add_children(&children);
    info!(
        "timeline: {} tracks, {bars} events, {:.2}s",
        names.len(),
        clip.duration
    );
}

/// One event bar: click to select it, drag to move it in time.
fn spawn_track_bar(
    commands: &mut Commands,
    track: &str,
    item: &TrackItem,
    left: f32,
    top: f32,
    width: f32,
) -> Entity {
    let bar = TrackBar {
        track: track.to_string(),
        item: item.id,
    };
    let label = event_label(&item.value.event);
    let moved = bar.clone();
    let picked = bar.clone();
    commands
        .spawn((
            Node {
                position_type: PositionType::Absolute,
                left: Val::Px(left),
                top: Val::Px(top + 4.0),
                width: Val::Px(width),
                height: Val::Px(LANE_H - 8.0),
                align_items: AlignItems::Center,
                padding: UiRect::horizontal(Val::Px(4.0)),
                overflow: Overflow::clip(),
                ..default()
            },
            BackgroundColor(BAR_IDLE),
            bar,
            Children::spawn(Spawn((
                Text::new(label),
                ThemedText,
                TextFont {
                    font_size: FontSize::Px(10.0),
                    ..default()
                },
                TextLayout {
                    linebreak: bevy::text::LineBreak::NoWrap,
                    ..default()
                },
            ))),
        ))
        .observe(
            move |mut click: On<PointerClick>, mut selected: ResMut<Selected>| {
                if click.button != PointerButton::Primary {
                    return;
                }
                click.propagate(false);
                selected.node = Some(picked.item);
                selected.track = Some(picked.track.clone());
                selected.dirty = true;
            },
        )
        .observe(
            move |mut drag: On<PointerDrag>,
                  mut view: ResMut<CanvasView>,
                  mut clips: ResMut<Assets<GraphClip>>| {
                if drag.button != PointerButton::Primary {
                    return;
                }
                drag.propagate(false);
                let delta = drag.delta.x / (PX_PER_SEC * view.zoom);
                let Some(handle) = view.clip.clone() else {
                    return;
                };
                let Some(mut clip) = clips.get_mut(&handle) else {
                    return;
                };
                let duration = clip.duration;
                let Some(track) = clip.event_tracks.get_mut(&moved.track) else {
                    return;
                };
                let Some(event) = track.events.iter_mut().find(|e| e.id == moved.item) else {
                    return;
                };
                // Move the whole span and keep it on the clip: an event outside the duration
                // never fires, which looks like a broken event rather than a misplaced one.
                let span = event.value.end_time - event.value.start_time;
                let start = (event.value.start_time + delta).clamp(0.0, duration - span);
                event.value.start_time = start;
                event.value.end_time = start + span;
                // The track is kept sorted by start time; a drag past a neighbour breaks that.
                track
                    .events
                    .sort_by(|a, b| a.value.start_time.total_cmp(&b.value.start_time));
                view.dirty = true;
            },
        )
        .id()
}

/// An event as a bar caption. `StringId("footstep_l")` is the common case and its wrapper is
/// pure noise on a 13-pixel bar, so it is unwrapped to its payload.
fn event_label(event: &AnimationEvent) -> String {
    let text = format!("{event:?}");
    let inner = text
        .strip_prefix("StringId(\"")
        .and_then(|rest| rest.strip_suffix("\")"));
    ascii(inner.unwrap_or(&text))
}

const BAR_IDLE: Color = Color::srgba(0.30, 0.42, 0.58, 0.95);
const BAR_SELECTED: Color = Color::srgba(0.42, 0.62, 0.85, 1.0);

/// Tint the selected bar, the same way a selected node box is tinted.
fn highlight_bars(selected: Res<Selected>, mut bars: Query<(&TrackBar, &mut BackgroundColor)>) {
    for (bar, mut background) in &mut bars {
        let want = if selected.node == Some(bar.item) {
            BAR_SELECTED
        } else {
            BAR_IDLE
        };
        if background.0 != want {
            background.0 = want;
        }
    }
}

/// Write the open clip's event tracks back to its `.anim.ron`.
///
/// Only the tracks are editable here — `source` and `skeleton` name the baked `.animclip` and
/// its rig, and neither is the timeline's business.
fn write_clip(view: &CanvasView, clips: &Assets<GraphClip>) {
    let (Some(handle), Some(path)) = (&view.clip, &view.path) else {
        return;
    };
    let Some(clip) = clips.get(handle) else {
        return;
    };
    // Fails only when the clip has no source or no skeleton path — a clip built in memory
    // rather than loaded, which the browser cannot produce.
    let Ok(serial) = GraphClipSerial::try_from(clip) else {
        error!("save {path}: this clip has no source path to write back to");
        return;
    };
    let text = match ron::ser::to_string_pretty(&serial, ron::ser::PrettyConfig::default()) {
        Ok(text) => text,
        Err(err) => {
            error!("save {path}: {err}");
            return;
        }
    };
    let file = view.root.join(path);
    let existing = std::fs::read_to_string(&file).unwrap_or_default();
    let text = format!("{}{text}", leading_comment(&existing));
    let backup = file.with_extension("ron.bak");
    if file.exists() && !backup.exists() {
        let _ = std::fs::copy(&file, &backup);
    }
    match std::fs::write(&file, text) {
        Ok(()) => info!("saved {}", file.display()),
        Err(err) => error!("save {}: {err}", file.display()),
    }
}

// ------------------------------------------------------------------- ragdoll

/// A row in the ragdoll list, so a click knows which body it selected.
#[derive(Component, Clone, Copy)]
struct RagdollRow(BodyId);

/// Body colours: the ragdoll's own geometry, drawn over the rig it will drive.
const BODY_IDLE: Color = Color::srgb(0.35, 0.72, 0.95);
const BODY_PICKED: Color = Color::srgb(1.0, 0.78, 0.25);
const JOINT_COLOR: Color = Color::srgb(0.95, 0.45, 0.75);

/// The ragdoll's bodies and joints, as a list beside the preview.
///
/// There is no canvas here. A ragdoll is not a graph — it is geometry in the character's own
/// space, and the only honest view of a collider offset is the collider, drawn where it will
/// be. So the list is just for picking; [`draw_ragdoll`] is the actual editor.
fn draw_ragdoll_list(
    commands: &mut Commands,
    view: &mut CanvasView,
    ragdolls: &Assets<Ragdoll>,
    canvas: (Entity, Option<&Children>),
) {
    let Some(ragdoll) = view.ragdoll.clone().and_then(|h| ragdolls.get(&h)) else {
        return;
    };
    view.dirty = false;
    let (canvas_entity, existing) = canvas;
    if let Some(existing) = existing {
        for child in existing.iter() {
            commands.entity(child).despawn();
        }
    }

    let mut bodies: Vec<&Body> = ragdoll.bodies.values().collect();
    bodies.sort_by(|a, b| a.label.cmp(&b.label));
    let mut rows = Vec::new();
    for body in &bodies {
        let id = body.id;
        // Colliders are listed under their body because that is how they are POSITIONED —
        // a collider's offset is relative to the body, and means nothing without it.
        let shapes: Vec<String> = body
            .colliders
            .iter()
            .filter_map(|c| ragdoll.colliders.get(c))
            .map(|c| match &c.shape {
                ColliderShape::Sphere(s) => format!("sphere {:.2}", s.radius),
                ColliderShape::Capsule(c) => {
                    format!("capsule {:.2}x{:.2}", c.radius, c.half_length * 2.0)
                }
                ColliderShape::Cuboid(c) => format!(
                    "box {:.2},{:.2},{:.2}",
                    c.half_size.x * 2.0,
                    c.half_size.y * 2.0,
                    c.half_size.z * 2.0
                ),
            })
            .collect();
        let label = format!(
            "{}   [{}]   {}",
            body.label,
            match body.default_mode {
                bevy_animation_graph::core::ragdoll::definition::BodyMode::Dynamic => "dyn",
                _ => "kin",
            },
            shapes.join(", ")
        );
        rows.push(
            commands
                .spawn((
                    Node {
                        padding: UiRect::new(
                            Val::Px(8.0),
                            Val::Px(6.0),
                            Val::Px(3.0),
                            Val::Px(3.0),
                        ),
                        overflow: Overflow::clip(),
                        ..default()
                    },
                    BackgroundColor(Color::NONE),
                    RagdollRow(id),
                    Children::spawn(Spawn((
                        Text::new(ascii(&label)),
                        ThemedText,
                        TextFont {
                            font_size: FontSize::Px(11.0),
                            ..default()
                        },
                        TextLayout {
                            linebreak: bevy::text::LineBreak::NoWrap,
                            ..default()
                        },
                    ))),
                ))
                .observe(
                    move |mut click: On<PointerClick>, mut selected: ResMut<Selected>| {
                        if click.button != PointerButton::Primary {
                            return;
                        }
                        click.propagate(false);
                        selected.node = Some(id.uuid());
                        selected.track = None;
                        selected.dirty = true;
                    },
                )
                .id(),
        );
    }

    let panel = commands
        .spawn((
            Node {
                position_type: PositionType::Absolute,
                left: Val::Px(8.0),
                top: Val::Px(8.0),
                width: Val::Px(330.0),
                max_height: Val::Percent(70.0),
                flex_direction: FlexDirection::Column,
                row_gap: Val::Px(1.0),
                padding: UiRect::all(Val::Px(6.0)),
                overflow: Overflow::scroll_y(),
                ..default()
            },
            BackgroundColor(Color::srgba(0.08, 0.09, 0.11, 0.94)),
        ))
        .add_children(&rows)
        .id();
    commands.entity(canvas_entity).add_child(panel);
    info!(
        "ragdoll: {} bodies, {} colliders, {} joints",
        ragdoll.bodies.len(),
        ragdoll.colliders.len(),
        ragdoll.joints.len()
    );
}

/// Draw the ragdoll in the preview, every frame.
///
/// A body has an offset in the CHARACTER's space and its colliders have offsets relative to
/// that, so a collider lands at `rig * body.offset * collider.local_offset`. Composing through
/// the preview rig's transform is what puts the geometry on the body it will drive, rather than
/// floating at the world origin.
fn draw_ragdoll(
    mut gizmos: Gizmos,
    view: Res<CanvasView>,
    selected: Res<Selected>,
    ragdolls: Res<Assets<Ragdoll>>,
    preview: Res<Preview>,
    places: Query<&GlobalTransform>,
) {
    let Some(ragdoll) = view.ragdoll.as_ref().and_then(|h| ragdolls.get(h)) else {
        return;
    };
    let rig = places
        .get(preview.root)
        .map(|t| t.affine())
        .unwrap_or_default();

    for body in ragdoll.bodies.values() {
        let picked = selected.node == Some(body.id.uuid());
        let color = if picked { BODY_PICKED } else { BODY_IDLE };
        for collider in body
            .colliders
            .iter()
            .filter_map(|c| ragdoll.colliders.get(c))
        {
            let local = Transform::from_translation(body.offset)
                * Transform::from_isometry(collider.local_offset);
            let place = Transform::from_matrix((rig * local.compute_affine()).into());
            let iso = Isometry3d::new(place.translation, place.rotation);
            match &collider.shape {
                ColliderShape::Sphere(sphere) => {
                    gizmos.primitive_3d(sphere, iso, color);
                }
                ColliderShape::Capsule(capsule) => {
                    gizmos.primitive_3d(capsule, iso, color);
                }
                ColliderShape::Cuboid(cuboid) => {
                    gizmos.primitive_3d(cuboid, iso, color);
                }
            }
        }
    }

    // Joints: a cross at the anchor, and a line to each body it binds, so a joint that has
    // drifted off its bodies is visible rather than merely wrong in the file.
    let body_at = |id: &BodyId| {
        ragdoll
            .bodies
            .get(id)
            .map(|b| rig.transform_point3(b.offset))
    };
    for joint in ragdoll.joints.values() {
        let (anchor, b1, b2) = match &joint.variant {
            JointVariant::Spherical(j) => (j.position, j.body1, j.body2),
            JointVariant::Revolute(j) => (j.position, j.body1, j.body2),
        };
        let anchor = rig.transform_point3(anchor);
        gizmos.cross(anchor, 0.06, JOINT_COLOR);
        for id in [b1, b2] {
            if let Some(at) = body_at(&id) {
                gizmos.line(anchor, at, JOINT_COLOR.with_alpha(0.5));
            }
        }
    }
}

/// Tint the selected body's row.
fn highlight_ragdoll_rows(
    selected: Res<Selected>,
    mut rows: Query<(&RagdollRow, &mut BackgroundColor)>,
) {
    for (row, mut background) in &mut rows {
        let want = if selected.node == Some(row.0.uuid()) {
            Color::srgba(0.22, 0.26, 0.34, 1.0)
        } else {
            Color::NONE
        };
        if background.0 != want {
            background.0 = want;
        }
    }
}

/// Which ragdoll body the two resolvers below are pointed at.
fn selected_body(world: &World) -> Option<(BodyId, Handle<Ragdoll>)> {
    let id = world.get_resource::<Selected>()?.node?;
    let handle = world.get_resource::<CanvasView>()?.ragdoll.clone()?;
    Some((BodyId::from_uuid(id), handle))
}

fn read_selected_body(world: &World, visit: &mut dyn FnMut(&dyn Reflect)) {
    let Some((id, handle)) = selected_body(world) else {
        return;
    };
    let Some(body) = world
        .get_resource::<Assets<Ragdoll>>()
        .and_then(|r| r.get(&handle))
        .and_then(|r| r.bodies.get(&id))
    else {
        return;
    };
    visit(body.as_reflect());
}

fn write_selected_body(world: &mut World, visit: &mut dyn FnMut(&mut dyn Reflect)) {
    let Some((id, handle)) = selected_body(world) else {
        return;
    };
    let Some(mut ragdolls) = world.get_resource_mut::<Assets<Ragdoll>>() else {
        return;
    };
    let Some(mut ragdoll) = ragdolls.get_mut(&handle) else {
        return;
    };
    let Some(body) = ragdoll.bodies.get_mut(&id) else {
        return;
    };
    visit(body.as_reflect_mut());
}

/// Write the open ragdoll back to its `.rag.ron`.
///
/// `Ragdoll` derives `Serialize` itself — no serial mirror and no type registry, because it
/// holds no `dyn` bodies and no asset handles, only geometry.
fn write_ragdoll(view: &CanvasView, ragdolls: &Assets<Ragdoll>) {
    let (Some(handle), Some(path)) = (&view.ragdoll, &view.path) else {
        return;
    };
    let Some(ragdoll) = ragdolls.get(handle) else {
        return;
    };
    let text = match ron::ser::to_string_pretty(ragdoll, ron::ser::PrettyConfig::default()) {
        Ok(text) => text,
        Err(err) => {
            error!("save {path}: {err}");
            return;
        }
    };
    let file = view.root.join(path);
    let existing = std::fs::read_to_string(&file).unwrap_or_default();
    let text = format!("{}{text}", leading_comment(&existing));
    let backup = file.with_extension("ron.bak");
    if file.exists() && !backup.exists() {
        let _ = std::fs::copy(&file, &backup);
    }
    match std::fs::write(&file, text) {
        Ok(()) => info!("saved {}", file.display()),
        Err(err) => error!("save {}: {err}", file.display()),
    }
}
