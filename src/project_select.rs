use std::path::{Path, PathBuf};

use bevy::{
    prelude::*,
    tasks::{AsyncComputeTaskPool, Task, futures_lite::future},
    window::{PrimaryWindow, RawHandleWrapper},
};
use jackdaw_feathers::{
    button::{ButtonVariant, IconButtonProps, icon_button},
    icons::{EditorFont, Icon},
    tokens,
};
use jackdaw_localization::LocalizedText;

use crate::{
    AppState,
    project::{self, ProjectRoot},
    windowing::{JackdawIcon, title_bar_repo_link},
};
#[cfg(any(target_os = "windows", target_os = "linux", target_os = "freebsd"))]
use bevy_window_chrome::CaptionFont;
use bevy_window_chrome::{WindowChromeTheme, spawn_window_shell};

pub struct ProjectSelectPlugin;

impl Plugin for ProjectSelectPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(OnEnter(AppState::ProjectSelect), spawn_project_selector)
            .add_systems(
                Update,
                poll_folder_dialog.run_if(in_state(AppState::ProjectSelect)),
            )
            .add_systems(
                Update,
                apply_pending_auto_open.run_if(in_state(AppState::ProjectSelect)),
            )
            .add_systems(
                Update,
                open_pending_scenes
                    .run_if(in_state(AppState::Editor))
                    .run_if(resource_exists::<PendingSceneOpens>),
            );
    }
}

/// Marker for the project selector root UI node.
#[derive(Component, Copy, Clone)]
struct ProjectSelectorRoot;

/// When set, the project selector will skip UI and auto-open the given project.
#[derive(Resource)]
pub struct PendingAutoOpen {
    pub path: PathBuf,
    /// `true` when we got here via a post-restart auto-open ;
    /// the parent process already built + installed the dylib,
    /// so we skip that step (preventing an infinite
    /// build->restart->auto-open->build loop).
    pub skip_build: bool,
}

/// Resource holding the async folder picker task and what the picked
/// folder is for.
#[derive(Resource)]
struct FolderDialogTask {
    task: Task<Option<rfd::FileHandle>>,
    purpose: FolderPurpose,
}

/// Which launcher action opened the folder picker.
#[derive(Clone, Copy)]
enum FolderPurpose {
    /// Open an existing jackdaw project.
    Open,
}

/// Perform a queued auto-open, one frame after entering the project selector.
/// Deferring the open to `Update` (rather than running it from the `OnEnter`
/// spawn) lets `Startup` finish loading the editor extensions first, so the
/// open path finds the resources they register, such as `UntitledCounter`.
/// Removing the resource makes this run exactly once.
fn apply_pending_auto_open(world: &mut World) {
    let Some(pending) = world.remove_resource::<PendingAutoOpen>() else {
        return;
    };
    enter_project_with(world, pending.path, pending.skip_build);
}

fn spawn_project_selector(
    mut commands: Commands,
    theme: Res<WindowChromeTheme>,
    editor_font: Res<EditorFont>,
    icon_font: Res<jackdaw_feathers::icons::IconFont>,
    jackdaw_icon: Res<JackdawIcon>,
    #[cfg(any(target_os = "windows", target_os = "linux", target_os = "freebsd"))]
    caption_font: Res<CaptionFont>,
    pending: Option<Res<PendingAutoOpen>>,
) {
    if pending.is_some() {
        // Camera only, no shell: the open itself runs in `apply_pending_auto_open`
        // (an `Update` system) rather than here, so `Startup` has loaded the
        // editor extensions first. Those extensions register resources the open
        // path needs (e.g. `UntitledCounter`, populated when a project with no
        // scene falls back to creating an untitled one). The build modal still
        // draws over this camera.
        commands.spawn((Camera2d, ProjectSelectorRoot));
        return;
    }

    let recent = project::read_recent_projects();
    let font = editor_font.0.clone();
    let icon_font_handle = icon_font.0.clone();

    // Detect CWD project candidate
    let cwd = std::env::current_dir().unwrap_or_default();
    // What marks a project: a marker like `assets/` promotes any folder
    // that happens to have one to the top of the launcher, where clicking
    // it lands on the not-a-project card.
    let cwd_has_project = cwd.join("jackdaw.toml").is_file() || cwd.join("Cargo.toml").is_file();

    let slots = spawn_window_shell(
        &mut commands,
        &theme,
        #[cfg(any(target_os = "windows", target_os = "linux", target_os = "freebsd"))]
        caption_font,
        ProjectSelectorRoot,
    );
    fill_project_selector(
        &mut commands,
        slots.title_bar,
        slots.body,
        font,
        icon_font_handle,
        jackdaw_icon.0.clone(),
        recent,
        cwd,
        cwd_has_project,
    );
}

fn fill_project_selector(
    commands: &mut Commands,
    title_bar: Entity,
    body: Entity,
    font: Handle<Font>,
    icon_font_handle: Handle<Font>,
    jackdaw_icon: Handle<Image>,
    recent: project::RecentProjects,
    cwd: PathBuf,
    cwd_has_project: bool,
) {
    commands
        .entity(title_bar)
        .with_children(|title_bar_parent| {
            title_bar_parent.spawn((
                Node {
                    flex_direction: FlexDirection::Row,
                    align_items: AlignItems::Center,
                    justify_content: JustifyContent::SpaceBetween,
                    width: Val::Percent(100.0),
                    height: Val::Percent(100.0),
                    padding: UiRect::horizontal(Val::Px(tokens::SPACING_MD)),
                    ..Default::default()
                },
                Pickable::IGNORE,
                children![
                    (
                        Node {
                            flex_direction: FlexDirection::Row,
                            align_items: AlignItems::Center,
                            column_gap: Val::Px(tokens::SPACING_MD),
                            ..Default::default()
                        },
                        children![
                            title_bar_repo_link(jackdaw_icon),
                            (
                                Text::new("jackdaw"),
                                TextFont {
                                    font: font.clone().into(),
                                    font_size: tokens::TEXT_SIZE,
                                    ..Default::default()
                                },
                                TextColor(tokens::TEXT_PRIMARY),
                                Pickable::IGNORE,
                            ),
                        ],
                    ),
                    (
                        Text::new(format!("v{}", env!("CARGO_PKG_VERSION"))),
                        TextFont {
                            font: font.clone().into(),
                            font_size: tokens::TEXT_SIZE_SM,
                            ..Default::default()
                        },
                        TextColor(tokens::DOC_TAB_INACTIVE_LABEL),
                        Pickable::IGNORE,
                    )
                ],
            ));
        });
    commands.entity(body).with_children(|body_parent| {
        body_parent
            .spawn(Node {
                width: Val::Percent(100.0),
                flex_grow: 1.0,
                flex_direction: FlexDirection::Row,
                column_gap: Val::Px(8.0),
                padding: UiRect::new(Val::Px(8.0), Val::Px(8.0), Val::Px(0.0), Val::Px(8.0)),
                ..Default::default()
            })
            .with_children(|content| {
                // Left rail owns project-creation actions. Keeping these
                // separate from the project list makes the launcher read like
                // the rest of the editor: tools on the side, content adjacent.
                content
                    .spawn((
                        Node {
                            width: Val::Px(300.0),
                            height: Val::Percent(100.0),
                            flex_direction: FlexDirection::Column,
                            row_gap: Val::Px(8.0),
                            padding: UiRect::all(Val::Px(10.0)),
                            border: UiRect::all(Val::Px(1.0)),
                            border_radius: BorderRadius::all(Val::Px(tokens::BORDER_RADIUS_LG)),
                            ..Default::default()
                        },
                        BackgroundColor(tokens::PANEL_BG),
                        BorderColor::all(tokens::BORDER_SUBTLE),
                    ))
                    .with_children(|sidebar| {
                        spawn_launcher_section_label(sidebar, "Start", font.clone());

                        let browse_entity = spawn_launcher_action_button(
                            sidebar,
                            "Open Folder",
                            Icon::FolderOpen,
                            font.clone(),
                            icon_font_handle.clone(),
                            tokens::TOOLBAR_BG,
                            tokens::HOVER_BG,
                        );
                        sidebar
                            .commands()
                            .entity(browse_entity)
                            .observe(spawn_browse_dialog);

                        sidebar.spawn((
                            Node {
                                flex_grow: 1.0,
                                ..Default::default()
                            },
                            Pickable::IGNORE,
                        ));

                        sidebar.spawn((
                            LocalizedText::new("source-checkout"),
                            TextFont {
                                font: font.clone().into(),
                                font_size: tokens::TEXT_SIZE_SM,
                                ..Default::default()
                            },
                            TextColor(tokens::DOC_TAB_INACTIVE_LABEL),
                        ));
                        sidebar.spawn((
                            Text::new(cwd.to_string_lossy().to_string()),
                            TextFont {
                                font: font.clone().into(),
                                font_size: tokens::TEXT_SIZE_XS,
                                ..Default::default()
                            },
                            TextColor(tokens::TEXT_SECONDARY),
                            Node {
                                max_width: Val::Px(260.0),
                                overflow: Overflow::clip(),
                                ..Default::default()
                            },
                        ));
                    });

                // Main panel lists openable projects. The current checkout is
                // promoted above recents so local development builds are one
                // click from the launcher.
                content
                    .spawn((
                        Node {
                            flex_grow: 1.0,
                            height: Val::Percent(100.0),
                            flex_direction: FlexDirection::Column,
                            border: UiRect::all(Val::Px(1.0)),
                            border_radius: BorderRadius::all(Val::Px(tokens::BORDER_RADIUS_LG)),
                            overflow: Overflow::clip(),
                            ..Default::default()
                        },
                        BackgroundColor(tokens::PANEL_BG),
                        BorderColor::all(tokens::BORDER_SUBTLE),
                    ))
                    .with_children(|projects| {
                        projects.spawn((
                            Node {
                                width: Val::Percent(100.0),
                                height: Val::Px(34.0),
                                align_items: AlignItems::Center,
                                padding: UiRect::axes(Val::Px(12.0), Val::Px(0.0)),
                                border: UiRect::bottom(Val::Px(1.0)),
                                border_radius: BorderRadius::top(Val::Px(tokens::BORDER_RADIUS_LG)),
                                ..Default::default()
                            },
                            BackgroundColor(tokens::PANEL_HEADER_BG),
                            BorderColor::all(tokens::BORDER_SUBTLE),
                            children![(
                                Text::new("Projects"),
                                TextFont {
                                    font: font.clone().into(),
                                    font_size: tokens::TEXT_SIZE,
                                    ..Default::default()
                                },
                                TextColor(tokens::TEXT_PRIMARY),
                            )],
                        ));

                        projects
                            .spawn(Node {
                                flex_direction: FlexDirection::Column,
                                row_gap: Val::Px(6.0),
                                padding: UiRect::all(Val::Px(10.0)),
                                width: Val::Percent(100.0),
                                flex_grow: 1.0,
                                ..Default::default()
                            })
                            .with_children(|list| {
                                if cwd_has_project {
                                    let cwd_name = cwd
                                        .file_name()
                                        .map(|n| n.to_string_lossy().to_string())
                                        .unwrap_or_else(|| cwd.to_string_lossy().to_string());
                                    spawn_launcher_section_label(
                                        list,
                                        "Current Directory",
                                        font.clone(),
                                    );
                                    spawn_project_row(
                                        list,
                                        &cwd_name,
                                        &cwd.to_string_lossy(),
                                        font.clone(),
                                        icon_font_handle.clone(),
                                        cwd.clone(),
                                        None,
                                        true,
                                    );
                                }

                                spawn_launcher_section_label(list, "Recent", font.clone());
                                let mut shown_recent = 0usize;
                                for entry in &recent.projects {
                                    if cwd_has_project
                                        && dunce::simplified(entry.path.as_path())
                                            == dunce::simplified(cwd.as_path())
                                    {
                                        continue;
                                    }
                                    spawn_project_row(
                                        list,
                                        &entry.name,
                                        &entry.path.to_string_lossy(),
                                        font.clone(),
                                        icon_font_handle.clone(),
                                        entry.path.clone(),
                                        Some(entry.last_opened.as_str()),
                                        false,
                                    );
                                    shown_recent += 1;
                                }

                                if shown_recent == 0 {
                                    spawn_empty_recent_state(
                                        list,
                                        font.clone(),
                                        icon_font_handle.clone(),
                                    );
                                }
                            });
                    });
            });
    });
}

fn spawn_project_row(
    parent: &mut ChildSpawnerCommands,
    name: &str,
    path_display: &str,
    font: Handle<Font>,
    icon_font: Handle<Font>,
    project_path: PathBuf,
    last_opened: Option<&str>,
    is_cwd: bool,
) {
    // Rows use the same dense panel styling as editor lists: icon, primary
    // label, path, and an optional remove action for persisted recents.
    let row_entity = parent
        .spawn((
            Node {
                flex_direction: FlexDirection::Row,
                width: Val::Percent(100.0),
                min_height: Val::Px(46.0),
                padding: UiRect::axes(Val::Px(10.0), Val::Px(8.0)),
                border: UiRect::all(Val::Px(1.0)),
                border_radius: BorderRadius::all(Val::Px(tokens::BORDER_RADIUS_LG)),
                align_items: AlignItems::Center,
                column_gap: Val::Px(10.0),
                ..Default::default()
            },
            BorderColor::all(tokens::BORDER_SUBTLE),
            jackdaw_feathers::list_view::list_row(),
        ))
        .id();

    let project_icon = parent
        .commands()
        .spawn((
            Node {
                width: Val::Px(26.0),
                height: Val::Px(26.0),
                align_items: AlignItems::Center,
                justify_content: JustifyContent::Center,
                border_radius: BorderRadius::all(Val::Px(tokens::BORDER_RADIUS_MD)),
                ..Default::default()
            },
            BackgroundColor(tokens::DOC_TAB_ACTIVE_BG),
            children![(
                Text::new(String::from(Icon::Folder.unicode())),
                TextFont {
                    font: icon_font.clone().into(),
                    font_size: tokens::ICON_SM,
                    ..Default::default()
                },
                TextColor(tokens::DIR_ICON_COLOR),
            )],
            Pickable::IGNORE,
        ))
        .id();
    parent.commands().entity(row_entity).add_child(project_icon);

    let info_column = parent
        .commands()
        .spawn((
            Node {
                flex_direction: FlexDirection::Column,
                flex_grow: 1.0,
                row_gap: Val::Px(2.0),
                overflow: Overflow::clip(),
                ..Default::default()
            },
            children![
                (
                    Node {
                        flex_direction: FlexDirection::Row,
                        column_gap: Val::Px(8.0),
                        align_items: AlignItems::Center,
                        ..Default::default()
                    },
                    children![
                        (
                            Text::new(name.to_string()),
                            TextFont {
                                font: font.clone().into(),
                                font_size: tokens::TEXT_SIZE,
                                ..Default::default()
                            },
                            TextColor(tokens::TEXT_PRIMARY),
                        ),
                        if_cwd_badge(is_cwd, font.clone()),
                        // A folder that has been moved or deleted is
                        // still listed, so one stat per row says so up
                        // front rather than at the failing click.
                        missing_badge(&project_path, font.clone()),
                    ],
                ),
                (
                    Text::new(row_subtitle(path_display, last_opened)),
                    TextFont {
                        font: font.clone().into(),
                        font_size: tokens::TEXT_SIZE_SM,
                        ..Default::default()
                    },
                    TextColor(tokens::TEXT_SECONDARY),
                    Node {
                        max_width: Val::Percent(100.0),
                        overflow: Overflow::clip(),
                        ..Default::default()
                    },
                ),
            ],
            Pickable::IGNORE,
        ))
        .id();

    parent.commands().entity(row_entity).add_child(info_column);

    if !is_cwd {
        let remove_path = project_path.clone();
        let x_button = parent
            .commands()
            .spawn(icon_button(
                IconButtonProps::new(Icon::X).variant(ButtonVariant::Ghost),
                &icon_font,
            ))
            .id();

        parent.commands().entity(x_button).observe(
            move |mut click: On<PointerClick>, mut commands: Commands| {
                click.propagate(false);
                let path = remove_path.clone();
                project::remove_recent(&path);
                commands.entity(row_entity).try_despawn();
            },
        );

        parent.commands().entity(row_entity).add_child(x_button);
    }

    parent.commands().entity(row_entity).observe(
        move |_: On<PointerClick>, mut commands: Commands| {
            let path = project_path.clone();
            commands.queue(move |world: &mut World| {
                enter_project(world, path);
            });
        },
    );
}

/// The row's second line: the path, plus when the project was last
/// opened when that is known.
fn row_subtitle(path_display: &str, last_opened: Option<&str>) -> String {
    match last_opened.and_then(crate::timestamps::relative_to_now) {
        Some(when) => format!("{path_display}  .  {when}"),
        None => path_display.to_string(),
    }
}

/// A `missing` chip for a recent whose folder is no longer there.
/// Clicking still explains it and offers to forget the entry; this only
/// stops the list from claiming the project is fine.
fn missing_badge(project_path: &Path, font: Handle<Font>) -> impl Bundle {
    let missing = !project_path.is_dir();
    (
        Text::new(if missing { "missing" } else { "" }.to_string()),
        TextFont {
            font: font.into(),
            font_size: tokens::TEXT_SIZE_XS,
            ..Default::default()
        },
        TextColor(tokens::TEXT_ERROR),
        Node {
            display: if missing {
                Display::Flex
            } else {
                Display::None
            },
            ..Default::default()
        },
        Pickable::IGNORE,
    )
}

fn if_cwd_badge(is_cwd: bool, font: Handle<Font>) -> impl Bundle {
    let text = if is_cwd { "current dir" } else { "" };
    (
        Text::new(text.to_string()),
        TextFont {
            font: font.into(),
            font_size: tokens::TEXT_SIZE_SM,
            ..Default::default()
        },
        TextColor(tokens::TEXT_ACCENT),
    )
}

fn spawn_browse_dialog(
    _: On<PointerClick>,
    commands: Commands,
    raw_handle: Query<&RawHandleWrapper, With<PrimaryWindow>>,
) {
    pick_project_folder(commands, raw_handle, FolderPurpose::Open);
}

fn pick_project_folder(
    mut commands: Commands,
    raw_handle: Query<&RawHandleWrapper, With<PrimaryWindow>>,
    purpose: FolderPurpose,
) {
    let title = match purpose {
        FolderPurpose::Open => "Select project folder",
    };
    let dialog = crate::native_dialog::dialog_at(
        crate::native_dialog::launcher_project_directory(),
        raw_handle.single().ok(),
    )
    .set_title(title);

    let task =
        AsyncComputeTaskPool::get().spawn(crate::native_dialog::unless_suppressed(move || {
            dialog.pick_folder()
        }));
    commands.insert_resource(FolderDialogTask { task, purpose });
}

fn poll_folder_dialog(world: &mut World) {
    let Some(mut task_res) = world.get_resource_mut::<FolderDialogTask>() else {
        return;
    };
    let Some(result) = future::block_on(future::poll_once(&mut task_res.task)) else {
        return;
    };
    let purpose = task_res.purpose;
    world.remove_resource::<FolderDialogTask>();

    let Some(handle) = result else {
        return;
    };
    let path = handle.path().to_path_buf();
    match purpose {
        // Opening an unset-up project falls through to the import
        // offer anyway; going straight to the preview just skips one
        // click for a user who already knows they are importing.
        FolderPurpose::Open => enter_project(world, path),
    }
}

/// Entry point for **every** "open a project" action from the
/// launcher (new-scaffold completion, recent-project click, manual
/// folder browse). Anything without a `Cargo.toml`, and any Cargo
/// project with a `jackdaw.toml`, transitions straight to the editor;
/// an unrecognized Cargo project gets the import offer.
pub fn enter_project(world: &mut World, root: PathBuf) {
    enter_project_with(world, root, false);
}

/// Same as [`enter_project`] but lets the caller bypass the build
/// step. Used by the post-restart auto-open path: the parent
/// process already produced the dylib, the loader picked it up at
/// startup, so a second build-and-install would either be a no-op
/// or (for games) trigger another restart loop.
pub fn enter_project_with(world: &mut World, root: PathBuf, skip_build: bool) {
    if skip_build {
        transition_to_editor(world, root);
        return;
    }
    // A folder that is gone (a stale recent entry) or that was never a cargo
    // project stays on the launcher.
    if !root.join("Cargo.toml").is_file() {
        warn!("{} is not a cargo project", root.display());
        return;
    }
    transition_to_editor(world, root);
}

/// Apply the project-root state change and flip `AppState` to
/// `Editor`. Called from [`enter_project`] (no build needed) and
/// from the build-complete poller (build finished, transitioning).
///
/// The scenes the project was last left on are queued rather than opened here;
/// [`open_pending_scenes`] takes them one a frame, and falls back to
/// `<root>/assets/scene.bsn` when the project remembers none, so the user
/// lands in a populated editor rather than an empty one. That is the
/// convention the game template ships with.
///
/// Every open funnels through here, so the asset-root check lives here: no
/// other path can install a [`ProjectRoot`] whose `assets/` the asset server is
/// not reading from.
fn transition_to_editor(world: &mut World, root: PathBuf) {
    let plan = world
        .get_resource::<crate::restart::AssetProjectRoot>()
        .map(|asset_root| {
            crate::restart::plan_open(&asset_root.0, &root, crate::restart::can_restart())
        });
    match plan {
        None | Some(crate::restart::OpenPlan::Here) => {}
        Some(crate::restart::OpenPlan::Reopen) => {
            reopen_in_new_process(root);
            return;
        }
        Some(crate::restart::OpenPlan::Unreachable) => {
            warn!("{} cannot be opened from this process", root.display());
            return;
        }
    }

    let config = project::load_project_config(&root)
        .unwrap_or_else(|| project::create_default_project(&root));

    project::touch_recent(&root, &config.name);

    world.insert_resource(ProjectRoot::new(root.clone(), config));

    // Despawn the launcher UI.
    let mut to_despawn = Vec::new();
    let mut query = world.query_filtered::<Entity, With<ProjectSelectorRoot>>();
    for entity in query.iter(world) {
        to_despawn.push(entity);
    }
    for entity in to_despawn {
        if let Ok(ec) = world.get_entity_mut(entity) {
            ec.despawn();
        }
    }

    let mut next_state = world.resource_mut::<NextState<AppState>>();
    next_state.set(AppState::Editor);

    let last_open_tabs = world
        .resource::<crate::project::ProjectRoot>()
        .config
        .last_open_tabs
        .clone();
    let last_active = world
        .resource::<crate::project::ProjectRoot>()
        .config
        .last_active_tab;

    let paths: Vec<PathBuf> = last_open_tabs
        .iter()
        .filter_map(|rel| {
            let abs = root.join(rel);
            if !abs.is_file() {
                warn!("Persisted tab not found, skipping: {abs:?}");
                return None;
            }
            Some(abs)
        })
        .collect();
    world.insert_resource(PendingSceneOpens {
        paths: paths.into(),
        active: last_active,
        named: false,
        root,
    });
}

/// What the footer calls the scenes a project opens with.
const SCENE_OPEN_PHASE: &str = "scene open";

/// The scenes an opening project still has to put in front of the user.
///
/// Spawning one costs a whole frame on a scene of any size, so they open one a
/// frame with the footer naming the one coming next: the window draws, says
/// what it is about to do, and only then does it.
#[derive(Resource)]
struct PendingSceneOpens {
    paths: std::collections::VecDeque<PathBuf>,
    /// The tab the project was last left on.
    active: usize,
    /// Whether the footer has already had a frame to name the next scene.
    named: bool,
    root: PathBuf,
}

fn open_pending_scenes(world: &mut World) {
    let Some(next) = world.resource::<PendingSceneOpens>().paths.front().cloned() else {
        finish_pending_scene_opens(world);
        return;
    };
    if !world.resource::<PendingSceneOpens>().named {
        world.resource_mut::<PendingSceneOpens>().named = true;
        let name = next
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_else(|| next.display().to_string());
        crate::status_bar::begin_phase(world, SCENE_OPEN_PHASE, format!("Opening {name}"));
        return;
    }
    {
        let mut pending = world.resource_mut::<PendingSceneOpens>();
        pending.paths.pop_front();
        pending.named = false;
    }
    crate::scenes::operators::scene_open_system(world, &next);
}

/// Bring the tab the project was last left on forward, and fall back to a
/// scene of some kind when nothing opened.
///
/// A project with no persisted tabs, or whose every persisted entry has gone
/// from disk, falls back to `assets/scene.bsn` (the legacy `.jsn` sibling if
/// that is all there is) or an empty untitled scene, so the user never lands
/// in the editor with no scene at all.
fn finish_pending_scene_opens(world: &mut World) {
    let Some(pending) = world.remove_resource::<PendingSceneOpens>() else {
        return;
    };
    let tab_count = world.resource::<crate::scenes::Scenes>().tabs.len();
    if tab_count > 0 {
        crate::scenes::swap::swap_active_tab(world, pending.active.min(tab_count - 1));
    } else {
        let assets = pending.root.join("assets");
        let bsn = assets.join("scene.bsn");
        let jsn = assets.join("scene.jsn");
        let scene_path = if bsn.is_file() {
            Some(bsn)
        } else if jsn.is_file() {
            Some(jsn)
        } else {
            None
        };
        match scene_path {
            Some(scene_path) => crate::scenes::operators::scene_open_system(world, &scene_path),
            None => crate::scenes::operators::scene_new_system(world),
        }
    }
    crate::status_bar::finish_phase(world, SCENE_OPEN_PHASE);
}

/// Hand `root` to a process rooted at it, without letting this one shut down
/// first. Asking for an exit can lose the reopen: teardown can destroy the
/// window while the render thread is inside a swapchain present, and that fault
/// kills the process with the reopen still pending. Replacing the process image
/// ends every thread at once, so there is nothing left to race.
///
/// Everything a reopen needs is on disk by this point: leaving an open project
/// for the launcher asks about unsaved changes first, and open tabs are written
/// to the project config as they change.
///
/// Never returns outside tests; under test it records the request instead, so
/// the funnel can be exercised without replacing the test binary.
#[cfg(not(test))]
fn reopen_in_new_process(root: PathBuf) {
    crate::restart::relaunch_into_project(&root)
}

#[cfg(test)]
fn reopen_in_new_process(root: PathBuf) {
    crate::restart::request_project_relaunch(root);
}

fn spawn_launcher_section_label(
    parent: &mut ChildSpawnerCommands,
    label: &str,
    font: Handle<Font>,
) {
    parent.spawn((
        Text::new(label.to_string()),
        TextFont {
            font: font.into(),
            font_size: tokens::TEXT_SIZE_SM,
            ..Default::default()
        },
        TextColor(tokens::DOC_TAB_INACTIVE_LABEL),
        Node {
            margin: UiRect::top(Val::Px(4.0)),
            ..Default::default()
        },
        Pickable::IGNORE,
    ));
}

fn spawn_empty_recent_state(
    parent: &mut ChildSpawnerCommands,
    font: Handle<Font>,
    icon_font: Handle<Font>,
) {
    // Empty-state copy is intentionally terse; the left rail already exposes
    // the creation and browse affordances.
    parent.spawn((
        Node {
            width: Val::Percent(100.0),
            min_height: Val::Px(90.0),
            flex_direction: FlexDirection::Column,
            align_items: AlignItems::Center,
            justify_content: JustifyContent::Center,
            row_gap: Val::Px(6.0),
            border: UiRect::all(Val::Px(1.0)),
            border_radius: BorderRadius::all(Val::Px(tokens::BORDER_RADIUS_LG)),
            ..Default::default()
        },
        BackgroundColor(tokens::TOOLBAR_BG),
        BorderColor::all(tokens::BORDER_SUBTLE),
        children![
            (
                Text::new(String::from(Icon::FolderOpen.unicode())),
                TextFont {
                    font: icon_font.into(),
                    font_size: tokens::ICON_LG,
                    ..Default::default()
                },
                TextColor(tokens::DOC_TAB_INACTIVE_LABEL),
            ),
            (
                Text::new("No recent projects"),
                TextFont {
                    font: font.into(),
                    font_size: tokens::TEXT_SIZE,
                    ..Default::default()
                },
                TextColor(tokens::TEXT_SECONDARY),
            ),
        ],
        Pickable::IGNORE,
    ));
}

fn spawn_launcher_action_button(
    parent: &mut ChildSpawnerCommands,
    label: &str,
    icon: Icon,
    font: Handle<Font>,
    icon_font: Handle<Font>,
    idle_bg: Color,
    hover_bg: Color,
) -> Entity {
    // Shared launcher button primitive for actions that need an icon + label
    // but do not fit the generic icon-only button component.
    let button = parent
        .spawn((
            Node {
                width: Val::Percent(100.0),
                height: Val::Px(34.0),
                flex_direction: FlexDirection::Row,
                align_items: AlignItems::Center,
                column_gap: Val::Px(8.0),
                padding: UiRect::axes(Val::Px(10.0), Val::Px(0.0)),
                border: UiRect::all(Val::Px(1.0)),
                border_radius: BorderRadius::all(Val::Px(tokens::BORDER_RADIUS_LG)),
                ..Default::default()
            },
            BackgroundColor(idle_bg),
            BorderColor::all(tokens::BORDER_SUBTLE),
            children![
                (
                    Text::new(String::from(icon.unicode())),
                    TextFont {
                        font: icon_font.into(),
                        font_size: tokens::ICON_SM,
                        ..Default::default()
                    },
                    TextColor(tokens::TEXT_PRIMARY),
                ),
                (
                    Text::new(label.to_string()),
                    TextFont {
                        font: font.into(),
                        font_size: tokens::TEXT_SIZE,
                        ..Default::default()
                    },
                    TextColor(tokens::TEXT_PRIMARY),
                ),
            ],
        ))
        .id();

    parent.commands().entity(button).observe(
        move |hover: On<PointerOver>, mut bg: Query<&mut BackgroundColor>| {
            if let Ok(mut bg) = bg.get_mut(hover.event_target()) {
                bg.0 = hover_bg;
            }
        },
    );
    parent.commands().entity(button).observe(
        move |out: On<PointerOut>, mut bg: Query<&mut BackgroundColor>| {
            if let Ok(mut bg) = bg.get_mut(out.event_target()) {
                bg.0 = idle_bg;
            }
        },
    );

    button
}
