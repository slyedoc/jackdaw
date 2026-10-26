//! Multi-scene editor state. Owns the tab list; every document tab's entities live under its own
//! physics/render world, and only the active tab's carry `SceneEntity`. An editor embedded in a
//! game ([`HostGame`]) also has the Game tab first: the running world itself, never saved.

pub mod confirm_dialog;
pub mod external_watch;
pub mod operators;
pub mod swap;
pub mod ui;

use std::path::PathBuf;

use bevy::{camera::visibility::RenderLayers, gizmos::config::GizmoConfigStore, prelude::*};
use aurora::environment::{InEnvironment, RenderEnvironments};

use crate::commands::CommandHistory;
use crate::project::ProjectRoot;
use crate::terrain::TerrainDataStore;
use crate::terrain::navmesh_bake::TabNavmesh;

pub struct ScenesPlugin;

impl Plugin for ScenesPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Scenes>();
        app.init_resource::<confirm_dialog::PendingTabClose>();
        app.init_resource::<confirm_dialog::PendingQuit>();
        app.add_systems(OnEnter(crate::AppState::Editor), add_game_tab);
        app.add_systems(
            Update,
            (
                mark_active_dirty_on_history_growth,
                persist_tabs_to_project_config,
                ui::rebuild_scene_tab_strip,
                ui::update_scene_tab_visuals,
                ui::update_scene_tab_label_abbreviation,
                ui::show_scene_tab_close_on_hover,
                intercept_window_close,
                follow_tab_world,
            ),
        );
        app.add_observer(ui::on_scene_tab_context_action);
        app.add_plugins(external_watch::ExternalSceneWatchPlugin);
    }
}

/// Intercept `WindowCloseRequested`. If any tab is dirty, show the
/// confirm-quit dialog and consume the event. Otherwise emit `AppExit::Success`
/// so the normal X-click flow still works.
pub fn intercept_window_close(
    mut close_events: bevy::ecs::message::MessageReader<bevy::window::WindowCloseRequested>,
    mut exit: bevy::ecs::message::MessageWriter<bevy::app::AppExit>,
    scenes: Res<Scenes>,
    mut commands: Commands,
    mut pending: ResMut<confirm_dialog::PendingQuit>,
) {
    let mut close_requested = false;
    for _ in close_events.read() {
        close_requested = true;
    }
    if !close_requested {
        return;
    }

    let any_dirty = scenes.tabs.iter().any(|t| t.dirty);
    if !any_dirty {
        exit.write(bevy::app::AppExit::Success);
        return;
    }

    // Dialog already open; ignore the repeated event.
    if pending.active {
        return;
    }
    pending.active = true;
    pending.leaving_project = false;

    commands.queue(|world: &mut World| {
        confirm_dialog::spawn_confirm_quit_dialog(world);
    });
}

#[derive(Resource, Default)]
pub struct Scenes {
    pub tabs: Vec<SceneTab>,
    pub active: usize,
}

/// The app the editor is embedded in: its running world is the Game tab. Insert before the
/// editor is entered.
#[derive(Resource, Clone, Debug)]
pub struct HostGame {
    /// The Game tab's label.
    pub name: String,
}

impl Default for HostGame {
    fn default() -> Self {
        Self {
            name: "Game".into(),
        }
    }
}

/// The Game tab goes first, in front, when the editor comes up over a game.
fn add_game_tab(host: Option<Res<HostGame>>, mut scenes: ResMut<Scenes>) {
    let Some(host) = host else {
        return;
    };
    if scenes.tabs.iter().any(SceneTab::is_game) {
        return;
    }
    scenes.tabs.insert(0, SceneTab::game(&host.name));
    scenes.active = 0;
}

/// What an editor view of the open scene renders: the active tab's world alone.
pub fn scene_layers(world: &World) -> RenderLayers {
    let game = game_environment_bit(
        world
            .try_query_filtered::<&InEnvironment, (With<Camera3d>, Without<crate::EditorEntity>)>()
            .and_then(|mut cameras| cameras.iter(world).next().copied()),
    );
    tab_layers(
        world.get_resource::<RenderEnvironments>(),
        crate::scene_io::scene_environment(world),
        game,
    )
}

/// The live game's world: the one its own (non-editor) camera is in.
fn game_environment_bit(camera: Option<InEnvironment>) -> Option<u8> {
    camera.map(|environment| environment.0)
}

/// A tab with its own environment shows that one; the game tab (none of its own) shows the
/// game camera's; with neither, nothing.
fn tab_layers(
    worlds: Option<&RenderEnvironments>,
    tab_environment: Option<Entity>,
    game: Option<u8>,
) -> RenderLayers {
    match tab_environment.and_then(|tab| worlds?.bit(tab)).or(game) {
        Some(bit) => RenderLayers::layer(bit as usize),
        None => RenderLayers::none(),
    }
}

/// Viewport cameras see the active tab's world, and so do the gizmos drawn into it. Hidden, the
/// editor gives the gizmos back to the game.
fn follow_tab_world(
    worlds: Option<Res<RenderEnvironments>>,
    scenes: Res<Scenes>,
    shown: Option<Res<crate::EditorShown>>,
    state: Option<Res<State<crate::AppState>>>,
    cameras: Query<
        (Entity, Option<&RenderLayers>),
        Or<(
            With<crate::viewport::MainViewportCamera>,
            With<crate::camera_preview::CameraPreviewCamera>,
        )>,
    >,
    gizmos: Option<ResMut<GizmoConfigStore>>,
    game_cameras: Query<&InEnvironment, (With<Camera3d>, Without<crate::EditorEntity>)>,
    mut commands: Commands,
) {
    let tab_environment = scenes
        .tabs
        .get(scenes.active)
        .and_then(|tab| tab.environment);
    let game = game_environment_bit(game_cameras.iter().next().copied());
    let layers = tab_layers(worlds.as_deref(), tab_environment, game);
    for (camera, current) in &cameras {
        if current != Some(&layers) {
            commands.entity(camera).insert(layers.clone());
        }
    }
    let up = shown.is_some_and(|shown| shown.0)
        && state.is_some_and(|state| *state.get() == crate::AppState::Editor);
    // Hidden, the gizmos go back to the game's world.
    let gizmo_layers = if up {
        layers
    } else {
        game.map_or(RenderLayers::none(), |bit| RenderLayers::layer(bit as usize))
    };
    if let Some(mut store) = gizmos {
        let differs = store
            .iter()
            .any(|(_, config, _)| config.render_layers != gizmo_layers);
        if differs {
            for (_, config, _) in store.iter_mut() {
                config.render_layers = gizmo_layers.clone();
            }
        }
    }
}

/// Whether another tab can get a world: each tab renders in its own world bit, and the 8 bits
/// are shared with the editor's preview worlds.
pub fn refuse_new_tab(world: &mut World) -> bool {
    let full = world
        .get_resource::<RenderEnvironments>()
        .is_some_and(|worlds| (1..8).all(|bit| worlds.environment(bit).is_some()));
    if full {
        crate::status_bar::notify_error(world, "Too many open tabs: close one first".to_string());
    }
    full
}

impl Scenes {
    /// Append a tab and return its index. Does not activate it.
    pub fn push_tab(&mut self, tab: SceneTab) -> usize {
        let idx = self.tabs.len();
        self.tabs.push(tab);
        idx
    }
}

#[derive(Default, Clone, Copy, Debug, PartialEq, Eq)]
pub enum TabKind {
    /// A `.bsn` file, or one not saved yet.
    #[default]
    Document,
    /// The host game's running world: edited live, never saved, never closed.
    Game,
}

/// Where a tab's entities are.
#[derive(Default, Debug)]
pub enum TabContent {
    /// Not spawned yet: activating the tab spawns its file (or starts empty).
    #[default]
    Unloaded,
    /// In front: the open scene is this tab's.
    Live,
    /// Behind another tab: its entities stay in its world, out of view.
    Parked,
}

pub struct SceneTab {
    pub path: Option<PathBuf>,
    pub display_name: String,
    pub dirty: bool,
    pub kind: TabKind,
    /// The file's root is its one entity (a tree, a character), not a scene of top-level
    /// entities; a save writes it back that way.
    pub root_file: bool,
    pub content: TabContent,
    /// The tab's environment, its 3D roots' parent; spawned on first activation.
    pub environment: Option<Entity>,
    pub view_state: ViewState,
    pub history: CommandHistory,
    /// Decoded terrain sidecars owned by this tab while it is inactive.
    /// The active tab's store lives in the world as a resource instead.
    pub terrain_data_store: TerrainDataStore,
    /// This tab's baked navmesh and any bake still running for it. Held per tab,
    /// like the terrain data: a bake describes one scene's ground and must not
    /// follow the editor to another.
    pub navmesh: TabNavmesh,
    /// Recorded `CommandHistory.undo_stack.len()` as of the last time
    /// the dirty-tracking system ran (or the tab was activated, or
    /// saved). If the live history is deeper than this, the user has
    /// made a change since the last check, and the tab is marked
    /// `dirty`.
    pub history_depth_at_last_check: usize,
    /// Why the tab is in front without its contents in the world, if it is.
    /// Set, the world is not the file's contents and must not be written back.
    pub refusal: Option<TabRefusal>,
}

/// Why an activation refused to put a tab's document in the world. The next
/// successful activation clears it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TabRefusal {
    /// The document names vocabulary this build will not spawn.
    Rejected(String),
    /// The spawn was attempted and failed.
    SpawnFailed(String),
}

impl TabRefusal {
    /// The message shown for a save this refusal blocks.
    pub fn reason(&self) -> &str {
        match self {
            Self::Rejected(reason) | Self::SpawnFailed(reason) => reason,
        }
    }
}

impl SceneTab {
    /// Whether writing this tab's file would write a world that never held it.
    pub fn is_refused(&self) -> bool {
        self.refusal.is_some()
    }

    pub fn is_game(&self) -> bool {
        self.kind == TabKind::Game
    }

    /// The host game's tab, in front.
    pub fn game(name: &str) -> Self {
        Self {
            display_name: name.to_string(),
            kind: TabKind::Game,
            content: TabContent::Live,
            ..Self::new_untitled(0)
        }
    }

    pub fn new_untitled(n: u32) -> Self {
        Self {
            path: None,
            display_name: format!("untitled-{n}"),
            dirty: false,
            kind: TabKind::Document,
            root_file: false,
            content: TabContent::default(),
            environment: None,
            view_state: ViewState::with_default_camera(),
            history: CommandHistory::default(),
            terrain_data_store: TerrainDataStore::default(),
            navmesh: TabNavmesh::default(),
            history_depth_at_last_check: 0,
            refusal: None,
        }
    }
}

/// When `Scenes` mutates, mirror the open-tab paths into the project
/// config so they survive across sessions. Skipped if no project root
/// is loaded (e.g. project-selector screen, headless tests).
pub fn persist_tabs_to_project_config(
    scenes: Res<Scenes>,
    project_root: Option<ResMut<ProjectRoot>>,
) {
    if !scenes.is_changed() {
        return;
    }
    let Some(mut project_root) = project_root else {
        return;
    };

    let saved: Vec<(usize, String)> = scenes
        .tabs
        .iter()
        .enumerate()
        .filter_map(|(index, t)| Some((index, t.path.as_ref()?)))
        .filter_map(|(index, p)| Some((index, project_root.to_relative(p).to_str()?.to_owned())))
        .collect();
    let last_active_tab = saved
        .iter()
        .position(|(index, _)| *index == scenes.active)
        .unwrap_or(0);
    let last_open_tabs: Vec<String> = saved.into_iter().map(|(_, path)| path).collect();

    let cfg = &mut project_root.config;
    if cfg.last_open_tabs == last_open_tabs && cfg.last_active_tab == last_active_tab {
        return;
    }
    cfg.last_open_tabs = last_open_tabs;
    cfg.last_active_tab = last_active_tab;

    let root = project_root.root.clone();
    let project = project_root.config.clone();
    if let Err(e) = crate::project::save_project_config(&root, &project) {
        warn!("Failed to persist tab list to project.jsn: {e}");
    }
}

/// Per-frame system: compare the active tab's recorded history depth
/// with the live `CommandHistory`. Any growth means the user did
/// something; mark the tab dirty. Shrinkage (undo) is ignored. Save
/// resets the recorded depth.
pub fn mark_active_dirty_on_history_growth(
    history: Res<CommandHistory>,
    mut scenes: ResMut<Scenes>,
) {
    if scenes.tabs.is_empty() {
        return;
    }
    let active = scenes.active;
    let current_depth = history.undo_stack.len();
    let tab = scenes.bypass_change_detection().tabs.get_mut(active);
    let Some(tab) = tab.filter(|tab| !tab.is_game()) else {
        return;
    };
    let recorded = tab.history_depth_at_last_check;
    let needs_dirty_flip = current_depth > recorded && !tab.dirty;
    let needs_depth_update = current_depth != recorded;
    if !needs_dirty_flip && !needs_depth_update {
        return;
    }
    scenes.set_changed();
    let tab = &mut scenes.tabs[active];
    if needs_dirty_flip {
        tab.dirty = true;
    }
    tab.history_depth_at_last_check = current_depth;
}

#[derive(Default, Clone)]
pub struct ViewState {
    pub camera_transform: Transform,
    /// Optional projection matrix. `None` means use the editor's default
    /// perspective on restore. Stored as a `Mat4` so we don't have to
    /// reflect the entire `Projection` enum across tab swaps.
    pub camera_projection: Option<bevy::math::Mat4>,
    pub edit_mode: crate::brush::EditMode,
    pub selection: Vec<Entity>,
    /// Brush sub-element selection (verts, edges, faces) for whichever
    /// brush is active in `selection`.
    pub brush_sub_selection: crate::brush::BrushSelection,
    /// How the 2D viewport was framed while this tab was active. `None` means
    /// no framing was ever chosen, so the tab takes whatever its next
    /// activation gives it.
    pub ui_view: Option<crate::viewport_2d::Ui2dView>,
    /// The viewport mode the user picked for this tab, overriding the one its
    /// scene kind asks for. `None` falls back to the document's kind.
    pub viewport_mode: Option<crate::viewport_host::ViewportMode>,
}

impl ViewState {
    /// Default `ViewState` for a freshly-created tab. Uses the same
    /// initial camera framing as the viewport setup (looking at the
    /// origin from `(0, 4, 8)`), so a new untitled scene shows the
    /// grid + axes instead of the camera sitting on the origin and
    /// rendering the grid edge-on.
    pub fn with_default_camera() -> Self {
        Self {
            camera_transform: Transform::from_xyz(0.0, 4.0, 8.0)
                .looking_at(bevy::math::Vec3::ZERO, bevy::math::Vec3::Y),
            ..Self::default()
        }
    }
}
