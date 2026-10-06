use std::collections::HashSet;
use std::path::{Path, PathBuf};

use bevy::{
    asset::{AssetPath, UntypedHandle},
    bsn::{BsnDocument, BsnNodeKind},
    bsn_asset::DynamicScene,
    light::SunDisk,
    prelude::*,
    scene::{ResolveSceneError, SceneInstanceState, ScenePatch, ScenePatchInstance, SpawnSceneError, WorldSceneExt},
    tasks::{Task, futures_lite::future},
};
use rfd::FileHandle;

use crate::scenes::operators::SceneKind;

use super::registration::{SceneEntity, SceneRootOf, adopt_entity, set_tab_open};
use super::save::save_scene_inner;
use super::{SceneDirtyState, SceneFilePath};

#[derive(Resource)]
pub(super) enum SceneDialogTask {
    Open(Task<Option<FileHandle>>),
    Save(Task<Option<FileHandle>>),
}

/// Open the scene file picker behind File > Open. No-op while another
/// scene dialog is already up.
pub fn spawn_open_dialog(world: &mut World) {
    if world.contains_resource::<SceneDialogTask>() {
        return;
    }
    let dialog =
        crate::native_dialog::file_dialog(world, crate::native_dialog::DialogPurpose::Scene)
            .set_title("Open scene")
            .add_filter("Jackdaw scene", &["bsn"]);
    let task = bevy::tasks::AsyncComputeTaskPool::get().spawn(
        crate::native_dialog::unless_suppressed(move || dialog.pick_file()),
    );
    world.insert_resource(SceneDialogTask::Open(task));
}

/// Whether a load put its document in the world. Every refusal is fail-soft:
/// the scene already open is left standing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoadOutcome {
    Loaded,
    Refused(LoadRefusal),
}

/// Why a load did not happen, in a form the editor can display.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadRefusal {
    pub category: RefusalCategory,
    /// The same sentence the warn log carries.
    pub message: String,
}

/// The kinds of refusal, so a caller can lead with what went wrong.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefusalCategory {
    /// The file could not be read.
    Unreadable,
    /// The text is not a document this editor reads.
    Unparsable,
    /// The document was accepted but did not reach the world.
    NotSpawned,
}

impl RefusalCategory {
    pub fn label(self) -> &'static str {
        match self {
            Self::Unreadable => "the file could not be read",
            Self::Unparsable => "the file is not a scene this editor can read",
            Self::NotSpawned => "the scene could not be spawned",
        }
    }
}

fn refuse(category: RefusalCategory, message: String) -> LoadOutcome {
    warn!("{message}");
    LoadOutcome::Refused(LoadRefusal { category, message })
}

pub fn load_scene_from_file(world: &mut World, chosen: &Path) {
    finish_load_scene(world, chosen);
}

/// [`load_scene_from_file`] for a caller that has to report a refusal.
pub fn load_scene_from_file_with_outcome(world: &mut World, chosen: &Path) -> LoadOutcome {
    finish_load_scene(world, chosen)
}

/// A scene file read and parsed, ready to spawn.
pub struct SceneFile {
    pub path: PathBuf,
    /// The file as an asset path, which bases and handles resolve against.
    pub asset_path: String,
    pub text: String,
    pub bytes: Vec<u8>,
    /// The file's root is a wrapper (no name, no patches) holding the scene's top-level entities.
    pub wrapped: bool,
}

/// Read and parse a scene file, without touching the open scene.
pub fn read_scene_file(world: &World, path: &Path) -> Result<SceneFile, LoadRefusal> {
    let refusal = |category, message: String| LoadRefusal { category, message };
    let bytes = std::fs::read(path).map_err(|err| {
        refusal(
            RefusalCategory::Unreadable,
            format!("failed to read scene file '{}': {err}", path.display()),
        )
    })?;
    let text = String::from_utf8(bytes.clone()).map_err(|err| {
        refusal(
            RefusalCategory::Unreadable,
            format!("'{}' is not UTF-8: {err}", path.display()),
        )
    })?;
    let document = BsnDocument::parse(&text).map_err(|err| {
        refusal(
            RefusalCategory::Unparsable,
            format!("failed to parse '{}': {err:?}", path.display()),
        )
    })?;
    let wrapped = is_wrapper(&document);
    let asset_path = asset_path_of(world, path);
    Ok(SceneFile {
        path: path.to_path_buf(),
        asset_path,
        text,
        bytes,
        wrapped,
    })
}

/// Whether the document's one root only holds the scene's top-level entities.
fn is_wrapper(document: &BsnDocument) -> bool {
    let [root] = document.roots[..] else {
        return false;
    };
    matches!(
        document.node(root).map(|node| &node.kind),
        Some(BsnNodeKind::Entity { name: None, base: None, patches, .. }) if patches.is_empty()
    )
}

/// `path` as an asset path under the project's assets folder.
pub fn asset_path_of(world: &World, path: &Path) -> String {
    let assets = world
        .get_resource::<crate::project::ProjectRoot>()
        .map(crate::project::ProjectRoot::assets_dir);
    match assets {
        Some(assets) => crate::instances::asset_path_of(&assets, path),
        None => path.to_string_lossy().replace('\\', "/"),
    }
}

/// What spawning a scene file came to.
pub enum SceneSpawn {
    /// The file's top-level entities, in order, adopted into the open scene.
    Spawned(Vec<Entity>),
    /// A base the file inherits is still loading; [`PendingSceneSpawns`] finishes it.
    Waiting,
}

/// Scene files waiting on a base to load before they spawn.
#[derive(Resource, Default)]
pub struct PendingSceneSpawns(Vec<PendingSceneSpawn>);

struct PendingSceneSpawn {
    file: SceneFile,
    held: Vec<UntypedHandle>,
    frames: u32,
}

/// How long a scene waits on the files it inherits before it is given up on.
const PENDING_SPAWN_FRAMES: u32 = 600;

/// Whether a spawn failed only because a `.bsn` it includes has not been resolved yet.
pub(crate) fn is_unresolved(err: &str) -> bool {
    err.contains("has not been resolved yet")
}

impl PendingSceneSpawns {
    pub fn is_waiting_on(&self, path: &Path) -> bool {
        self.0.iter().any(|pending| pending.file.path == path)
    }

    pub fn forget(&mut self, path: &Path) {
        self.0.retain(|pending| pending.file.path != path);
    }
}

enum Attempt {
    Spawned(Vec<Entity>),
    Missing(AssetPath<'static>),
    Unresolved,
    Failed(String),
}

fn try_spawn(world: &mut World, file: &SceneFile) -> Attempt {
    let document = match BsnDocument::parse(&file.text) {
        Ok(document) => document,
        Err(err) => return Attempt::Failed(format!("{err:?}")),
    };
    let registry = world.resource::<AppTypeRegistry>().clone();
    let _server = world.resource::<AssetServer>().clone();
    let mut handles = jackdaw_runtime::handle_provider(world);
    let scene = match DynamicScene::from_document_with_handles(
        &document,
        file.asset_path.as_str(),
        &registry,
        &mut handles,
    ) {
        Ok(scene) => scene,
        Err(err) => return Attempt::Failed(err.render(&file.text)),
    };
    let root = match world.spawn_scene(scene) {
        Ok(root) => root.id(),
        Err(SpawnSceneError::ResolveSceneError(ResolveSceneError::MissingSceneDependency(path))) => {
            return Attempt::Missing(path);
        }
        Err(err) if is_unresolved(&err.to_string()) => return Attempt::Unresolved,
        Err(err) => return Attempt::Failed(err.to_string()),
    };
    world
        .entity_mut(root)
        .remove::<(ScenePatchInstance, SceneInstanceState)>();
    let roots = if file.wrapped {
        let children: Vec<Entity> = world
            .get::<Children>(root)
            .map(|children| children.iter().collect())
            .unwrap_or_default();
        for &child in &children {
            world.entity_mut(child).remove::<ChildOf>();
            adopt_entity(world, child);
        }
        world.entity_mut(root).despawn();
        children
    } else {
        adopt_entity(world, root);
        vec![root]
    };
    Attempt::Spawned(roots)
}

/// Spawn `.bsn` text (a copy, a duplicate) into the open scene; `source` is the asset path its
/// relative references resolve against. Its top-level entities, adopted.
pub fn spawn_bsn_text(world: &mut World, text: &str, source: &str) -> Result<Vec<Entity>, String> {
    let document = BsnDocument::parse(text).map_err(|err| format!("{err:?}"))?;
    let file = SceneFile {
        path: PathBuf::from(source),
        asset_path: source.to_string(),
        text: text.to_string(),
        bytes: Vec::new(),
        wrapped: is_wrapper(&document),
    };
    match try_spawn(world, &file) {
        Attempt::Spawned(roots) => Ok(roots),
        Attempt::Missing(base) => Err(format!("{base} is not loaded")),
        Attempt::Unresolved => Err("a file it inherits is still loading".to_string()),
        Attempt::Failed(err) => Err(err),
    }
}

/// Spawn a scene file into the world as the open scene's contents.
pub fn spawn_scene_file(world: &mut World, file: SceneFile) -> Result<SceneSpawn, String> {
    match try_spawn(world, &file) {
        Attempt::Spawned(roots) => Ok(SceneSpawn::Spawned(roots)),
        Attempt::Failed(err) => Err(format!("{}: {err}", file.path.display())),
        Attempt::Missing(_) | Attempt::Unresolved => {
            // The file's own scene asset holds what it includes, and resolves it.
            let handle = world
                .resource::<AssetServer>()
                .load::<ScenePatch>(file.asset_path.clone())
                .untyped();
            world
                .get_resource_or_init::<PendingSceneSpawns>()
                .0
                .push(PendingSceneSpawn {
                    file,
                    held: vec![handle],
                    frames: 0,
                });
            Ok(SceneSpawn::Waiting)
        }
    }
}

/// Retry the scene files waiting on bases, once those have loaded.
pub(super) fn finish_pending_scene_spawns(world: &mut World) {
    let Some(mut pending) = world.get_resource_mut::<PendingSceneSpawns>() else {
        return;
    };
    if pending.0.is_empty() {
        return;
    }
    let waiting = std::mem::take(&mut pending.0);
    let server = world.resource::<AssetServer>().clone();
    let mut still = Vec::new();
    for mut spawn in waiting {
        if let Some(failed) = spawn.held.iter().find(|h| server.load_state(h.id()).is_failed()) {
            let base = failed.path().map(ToString::to_string).unwrap_or_default();
            crate::status_bar::notify_error(
                world,
                format!("{}: base {base} did not load", spawn.file.path.display()),
            );
            continue;
        }
        spawn.frames += 1;
        if spawn.frames > PENDING_SPAWN_FRAMES {
            crate::status_bar::notify_error(
                world,
                format!("{}: the files it inherits did not load", spawn.file.path.display()),
            );
            continue;
        }
        if !spawn
            .held
            .iter()
            .all(|h| server.is_loaded_with_dependencies(h.id()))
        {
            still.push(spawn);
            continue;
        }
        match try_spawn(world, &spawn.file) {
            Attempt::Spawned(roots) => scene_file_arrived(world, &spawn.file.path, roots),
            Attempt::Missing(base) => {
                spawn
                    .held
                    .push(server.load::<ScenePatch>(base).untyped());
                still.push(spawn);
            }
            Attempt::Unresolved => still.push(spawn),
            Attempt::Failed(err) => crate::status_bar::notify_error(
                world,
                format!("{}: {err}", spawn.file.path.display()),
            ),
        }
    }
    world
        .get_resource_or_init::<PendingSceneSpawns>()
        .0
        .extend(still);
}

/// A scene file that waited on its bases has spawned: it joins its tab, open or behind.
fn scene_file_arrived(world: &mut World, path: &Path, roots: Vec<Entity>) {
    let scenes = world.resource::<crate::scenes::Scenes>();
    let tab = scenes
        .tabs
        .iter()
        .position(|tab| tab.path.as_deref() == Some(path));
    let tab_world = tab.and_then(|tab| scenes.tabs[tab].world);
    let active = scenes.active;
    match (tab, tab_world) {
        (Some(tab), _) if tab == active => {
            let kind = scene_kind_of(world, &roots);
            apply_scene_kind(world, kind);
        }
        (Some(_), Some(tab_world)) => {
            for &root in &roots {
                let mut root_mut = world.entity_mut(root);
                root_mut.remove::<SceneRootOf>();
                if root_mut.contains::<ChildOf>() {
                    root_mut.insert(ChildOf(tab_world));
                }
                root_mut.insert(SceneRootOf(tab_world));
            }
            set_tab_open(world, tab_world, false);
        }
        _ => {
            for root in roots {
                world.entity_mut(root).despawn();
            }
        }
    }
}

fn finish_load_scene(world: &mut World, chosen: &Path) -> LoadOutcome {
    let file = match read_scene_file(world, chosen) {
        Ok(file) => file,
        Err(refusal) => return refuse(refusal.category, refusal.message),
    };
    world.resource_mut::<SceneFilePath>().last_directory = chosen.parent().map(Path::to_path_buf);
    let loaded_hash = crate::scenes::external_watch::hash_bytes(&file.bytes);
    let path = chosen.to_string_lossy().to_string();

    clear_scene_entities(world);
    match spawn_scene_file(world, file) {
        Ok(SceneSpawn::Spawned(roots)) => {
            info!("Scene loaded from {path} ({} top-level entities)", roots.len());
            let kind = scene_kind_of(world, &roots);
            apply_scene_kind(world, kind);
        }
        Ok(SceneSpawn::Waiting) => info!("{path}: waiting for the files it inherits to load"),
        Err(err) => return refuse(RefusalCategory::NotSpawned, err),
    }

    import_terrain_sidecars(world, &path, SidecarImport::Reload);
    crate::terrain::navmesh_bake::import_beside_scene(world, &path);
    crate::scenes::external_watch::note_known_hash(world, Path::new(&path), loaded_hash);
    world.resource_mut::<SceneFilePath>().path = Some(path);
    crate::ui_palette::backfill_ui_root_size(world);
    world.resource_mut::<SceneDirtyState>().undo_len_at_save = 0;
    LoadOutcome::Loaded
}

/// The open scene's kind picks the viewport; only a UI screen also fronts the panel.
pub fn apply_scene_kind(world: &mut World, kind: SceneKind) {
    let mode = crate::viewport_host::ViewportMode::for_scene_kind(kind);
    if kind == SceneKind::Ui {
        crate::viewport_host::focus_viewport(world, mode);
        crate::viewport_2d::request_2d_fit(world);
    } else {
        crate::viewport_host::set_viewport_mode(world, mode, false);
    }
}

/// Which kind of scene these roots make, read from the markers a save writes. A UI root wins
/// over a 2D one; neither is a 3D scene.
pub fn scene_kind_of(world: &World, roots: &[Entity]) -> SceneKind {
    let mut two_d = false;
    for &root in roots {
        if world.get::<jackdaw_scene_types::UiSceneRoot>(root).is_some() {
            return SceneKind::Ui;
        }
        two_d |= world.get::<jackdaw_scene_types::Scene2dRoot>(root).is_some();
    }
    if two_d {
        SceneKind::TwoD
    } else {
        SceneKind::ThreeD
    }
}

/// The open scene's kind.
pub fn open_scene_kind(world: &mut World) -> SceneKind {
    let roots = super::scene_roots(world);
    scene_kind_of(world, &roots)
}

/// Whether a sidecar import may overwrite data the store already holds.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum SidecarImport {
    /// Disk wins. Used when the user explicitly loads or reloads a scene.
    Reload,
    /// Only fill paths the store has never heard of. Used on tab
    /// activation, where the store may be holding unsaved sculpting that
    /// the file on disk does not have yet.
    FillMissing,
    /// Fill paths the store has never heard of, and re-read the ones whose
    /// file has been written since the store last read it. Used when returning
    /// to a tab with nothing unsaved in it.
    RefreshChanged,
}

/// Read each terrain's binary sidecar into the store.
///
/// A missing or unreadable sidecar warns and leaves the terrain flat
/// rather than failing the load: a scene whose data file was not copied
/// alongside it should still open, so the user can see what happened and
/// fix it. A legacy scene carrying inline `heights` has no sidecar and is
/// left alone here -- `ensure_terrain_data_path` drains the inline values
/// into the store, and the next save writes them out properly.
///
/// Called from two places, because there are two ways a terrain reaches
/// the world: `finish_load_scene` for an explicit open, and
/// `scenes::swap::activate_tab` for a tab that was opened by pushing a
/// parsed document straight onto the tab strip. Wiring only the first
/// leaves every scene opened from the tab strip flat.
///
/// Returns the sidecar paths this call read in, distinguishing a first load from
/// a tab switch that found everything already in the store.
pub(crate) fn import_terrain_sidecars(
    world: &mut World,
    scene_path: &str,
    mode: SidecarImport,
) -> Vec<String> {
    use jackdaw_terrain::sidecar;

    if world
        .get_resource::<crate::terrain::TerrainDataStore>()
        .is_none()
    {
        return Vec::new();
    }
    let scene_dir = std::path::Path::new(scene_path)
        .parent()
        .map(std::path::Path::to_path_buf)
        .unwrap_or_default();

    let mut wanted: Vec<(String, bool)> = Vec::new();
    let mut query = world.query_filtered::<&jackdaw_scene_types::Terrain, With<crate::scene_io::SceneEntity>>();
    for terrain in query.iter(world) {
        if terrain.data_path.is_empty() {
            continue;
        }
        if wanted.iter().any(|(path, _)| path == &terrain.data_path) {
            continue;
        }
        wanted.push((terrain.data_path.clone(), terrain.heights.is_empty()));
    }
    match mode {
        SidecarImport::Reload => {}
        SidecarImport::FillMissing => {
            let store = world.resource::<crate::terrain::TerrainDataStore>();
            wanted.retain(|(path, _)| !store.contains(path));
        }
        SidecarImport::RefreshChanged => {
            let store = world.resource::<crate::terrain::TerrainDataStore>();
            wanted.retain(|(path, _)| {
                !store.contains(path)
                    || sidecar::resolve_path(&scene_dir, path)
                        .is_ok_and(|full| store.sidecar_is_stale(path, &full))
            });
        }
    }
    let assets = crate::asset_index::assets_dir(world);
    let mut imported: std::collections::HashSet<String> = std::collections::HashSet::new();
    for (data_path, no_inline_heights) in wanted {
        let full = match sidecar::resolve_path(&scene_dir, &data_path) {
            Ok(path) => path,
            Err(err) => {
                warn!("Skipping invalid terrain data path {data_path:?}: {err}");
                continue;
            }
        };
        match std::fs::read(&full) {
            // A sidecar written before regions existed, or before slots held
            // paths, migrates on the way in.
            Ok(bytes) => match sidecar::load_from(&bytes, assets.as_deref()) {
                Ok(loaded) => {
                    for message in loaded.warnings() {
                        warn!("{message}");
                    }
                    let data = loaded.data;
                    let mtime = std::fs::metadata(&full)
                        .and_then(|meta| meta.modified())
                        .ok();
                    let mut store = world.resource_mut::<crate::terrain::TerrainDataStore>();
                    store.insert(data_path.clone(), data);
                    store.note_read(&data_path, mtime);
                    imported.insert(data_path);
                }
                Err(err) => {
                    warn!(
                        "Terrain data {} is unreadable ({err}); edits to this terrain \
                         are refused and save will not overwrite the file until it is \
                         fixed and reloaded",
                        full.display()
                    );
                    world
                        .resource_mut::<crate::terrain::TerrainDataStore>()
                        .mark_load_failed(data_path, err.to_string());
                }
            },
            // A legacy scene names no sidecar it ever wrote, so only a
            // terrain that expected one is worth warning about.
            Err(err) if no_inline_heights => {
                warn!(
                    "Terrain data {} is missing ({err}); loading a flat terrain",
                    full.display()
                );
            }
            Err(_) => {}
        }
    }

    settle_terrain_grids(world);
    // Terrains are placed by now, which a group saved beside one is moved
    // into the space of.
    crate::terrain::scatter::migrate_legacy_scatter_groups(world);
    imported.into_iter().collect()
}

/// Settle every loaded terrain onto the geometry its cells are drawn at, and
/// empty the migration inlets it may have arrived with.
///
/// Two sidecar formats arrive here. One states its own geometry, and the
/// component takes its cell size from the file, so a scene whose text is older
/// than its sidecar still draws correctly. One predates that field and is placed
/// by the rectangle the component declares, turned into the spacing and corner
/// that rectangle drew with.
///
/// Nothing moves in either case: the derived geometry matches the one the
/// rectangle implied, so every stored cell keeps its world position. The settled
/// terrain states where its cells are rather than implying it, and can hold
/// cells the rectangle left unreachable.
///
/// The inlets are reset afterwards, so a saved scene carries no `size` or
/// `resolution`.
pub(crate) fn settle_terrain_grids(world: &mut World) {
    use jackdaw_terrain::sidecar;

    if !world.contains_resource::<crate::terrain::TerrainDataStore>() {
        return;
    }
    let defaults = jackdaw_scene_types::Terrain::default();
    let mut query = world.query::<(Entity, &jackdaw_scene_types::Terrain, Option<&Name>)>();
    let pending: Vec<Settling> = query
        .iter(world)
        .map(|(entity, terrain, name)| {
            let stored = world
                .resource::<crate::terrain::TerrainDataStore>()
                .grid(&terrain.data_path);
            Settling {
                entity,
                name: name
                    .map(std::string::ToString::to_string)
                    .filter(|name| !name.is_empty())
                    .unwrap_or_else(|| terrain.data_path.clone()),
                data_path: terrain.data_path.clone(),
                grid: sidecar::resolve_grid(stored, terrain.size, terrain.resolution),
                // Only a sidecar that states no geometry is placed by the declared
                // rectangle, so only that one can be respaced.
                respaced: stored.is_none().then_some(()).and_then(|()| {
                    sidecar::declared_rect_respacing(terrain.size, terrain.resolution)
                }),
            }
        })
        .collect();

    for settling in pending {
        if let Some((x, z)) = settling.respaced {
            let message = format!(
                "{}: this terrain was drawn {x} metres per cell across and {z} along, \
                 which one square cell cannot describe. Its grid is respaced to {x} \
                 on both axes, so its ground has moved along Z.",
                settling.name,
            );
            warn!("{message}");
            crate::terrain::toast_terrain_notice(world, &message);
        }
        if !settling.data_path.is_empty() {
            world
                .resource_mut::<crate::terrain::TerrainDataStore>()
                .set_grid(&settling.data_path, settling.grid);
        }
        if let Some(mut terrain) = world.get_mut::<jackdaw_scene_types::Terrain>(settling.entity) {
            terrain.cell_size = settling.grid.cell_size;
            terrain.size = defaults.size;
            terrain.resolution = defaults.resolution;
        }
    }
}

/// Spawn default lighting for a new / empty scene (Sun directional
/// light + no ambient). The ambient override is always applied since
/// it is a `Resource` mutation, not a spawn.
///
/// The Sun is registered in the document and saved like any authored entity,
/// so it is seeded only into a world holding no document and no
/// `DirectionalLight`: a scene whose author kept no light must not gain one on
/// the next save.
pub fn spawn_default_lighting(world: &mut World) {
    world.insert_resource(GlobalAmbientLight::NONE);

    if !super::scene_roots(world).is_empty() {
        return;
    }

    let has_directional = world
        .query_filtered::<&DirectionalLight, With<SceneEntity>>()
        .iter(world)
        .next()
        .is_some();
    if has_directional {
        return;
    }

    let sun = world
        .spawn((
            Name::new("Sun"),
            DirectionalLight {
                illuminance: 20_000.0,
                ..default()
            },
            SunDisk::EARTH,
            Transform::from_xyz(10.0, 20.0, 10.0).with_rotation(Quat::from_euler(
                EulerRot::XYZ,
                -0.8,
                0.4,
                0.0,
            )),
        ))
        .id();
    adopt_entity(world, sun);
}

/// One terrain moving from the rectangle it declared to the geometry its cells
/// are drawn at.
struct Settling {
    entity: Entity,
    /// What to call this terrain in a notice to the author.
    name: String,
    data_path: String,
    grid: jackdaw_terrain::sidecar::GridGeometry,
    /// The two spacings a non-square declared rectangle asked for, when this
    /// settling is respacing one.
    respaced: Option<(f32, f32)>,
}

/// Collect `roots` and their full descendant subtrees into a set,
/// walking the `Children` relation. Each root is included; the returned
/// set dedups the walk so a shared descendant is visited only once.
fn collect_subtree(world: &World, roots: impl IntoIterator<Item = Entity>) -> HashSet<Entity> {
    let mut set = HashSet::new();
    let mut stack: Vec<Entity> = roots.into_iter().collect();
    while let Some(entity) = stack.pop() {
        if !set.insert(entity) {
            continue;
        }
        if let Some(children) = world.get::<Children>(entity) {
            stack.extend(children.iter());
        }
    }
    set
}

/// Drop every drag that is holding on to a scene entity.
///
/// A drag keeps writing transforms onto the ids it grabbed, which a reload or
/// tab switch leaves despawned or reused by unrelated entities.
fn forget_dragged_entities(world: &mut World) {
    if let Some(mut drag) = world.get_resource_mut::<crate::modal_transform::ViewportDragState>() {
        drag.pending = None;
        drag.active = None;
    }
    if let Some(mut gizmo) = world.get_resource_mut::<crate::gizmos::GizmoDragState>() {
        gizmo.active = false;
        gizmo.axis = None;
        gizmo.targets.clear();
        gizmo.camera = None;
        gizmo.viewport = None;
    }

    // Every other gesture that parks entity ids in a resource for the length of
    // a drag.
    use crate::brush::topology_ops as topo;
    forget_drag_resource::<crate::ui_stage::UiManipulation>(world);
    forget_drag_resource::<topo::extrude::ExtrudeModalState>(world);
    forget_drag_resource::<topo::inset::InsetModalState>(world);
    forget_drag_resource::<topo::edge_bevel::EdgeBevelModalState>(world);
    forget_drag_resource::<topo::vertex_bevel::VertexBevelModalState>(world);
    forget_drag_resource::<topo::edge_slide_modal::EdgeSlideModalState>(world);
    forget_drag_resource::<topo::vertex_slide_modal::VertexSlideModalState>(world);
}

/// Put one drag-state resource back to its resting value, if the app has it.
fn forget_drag_resource<R: Resource<Mutability = bevy::ecs::component::Mutable> + Default>(
    world: &mut World,
) {
    if world.contains_resource::<R>() {
        *world.resource_mut::<R>() = R::default();
    }
}

/// Remove scene entities from the world (named non-editor entities + their descendants).
pub(crate) fn clear_scene_entities(world: &mut World) {
    // The baked navmesh belongs to the scene being cleared, not to the tab it was in. A tab
    // switch has stashed it by the time this runs (`capture_active_tab`), so only a bake
    // whose scene is going away is dropped here.
    crate::terrain::navmesh_bake::forget_scene_navmesh(world);

    world
        .resource_mut::<crate::selection::Selection>()
        .entities
        .clear();

    if let Err(err) = world.run_system_cached(crate::hierarchy::clear_all_tree_rows) {
        error!("Failed to clear tree rows: {err}");
    }
    // The rows the outliner was still waiting on, and the ones it gave up on,
    // named entities in the scene that is going away.
    crate::hierarchy::forget_withheld_rows(world);

    // Clear undo/redo stacks; they hold entity references that become
    // stale when the scene is dropped. Callers who want to preserve
    // history (e.g. undo/redo itself) use `despawn_scene_entities`
    // directly.
    let mut history = world.resource_mut::<jackdaw_commands::CommandHistory>();
    history.undo_stack.clear();
    history.redo_stack.clear();

    if let Err(err) = despawn_scene_entities(world) {
        error!("clear_scene_entities failed: {err}");
    }
}

/// Despawn the open scene's entities (those of the active tab), keeping editor infrastructure
/// and the undo stacks.
pub(crate) fn despawn_scene_entities(world: &mut World) -> Result<(), BevyError> {
    forget_dragged_entities(world);
    let roots: Vec<Entity> = world
        .query_filtered::<Entity, With<SceneEntity>>()
        .iter(world)
        .collect();
    let scene_set = collect_subtree(world, roots);

    // Models still queued for these entities belong to the scene going away.
    // Whoever respawns asks for them again, and holding them would both keep
    // the glTFs they name alive and hand render roots to entities of a scene
    // that is no longer open.
    crate::entity_ops::forget_model_roots(world, &scene_set);

    for entity in scene_set {
        if let Ok(entity_mut) = world.get_entity_mut(entity) {
            entity_mut.despawn();
        }
    }

    // Sweep any leftover chunk mesh children. Despawning a parent brush
    // does not always cascade through `ChildOf` in time; orphan chunk
    // meshes would otherwise survive, keep their `Transform` and
    // `MeshMaterial3d`, and render as a ghost box at world origin in
    // the next scene.
    let orphan_chunks: Vec<Entity> = world
        .query_filtered::<Entity, (With<crate::brush::BrushMeshChunk>, Without<ChildOf>)>()
        .iter(world)
        .collect();
    for entity in orphan_chunks {
        if let Ok(entity_mut) = world.get_entity_mut(entity) {
            entity_mut.despawn();
        }
    }

    Ok(())
}

pub(super) fn poll_scene_dialog(world: &mut World) {
    let Some(mut task) = world.remove_resource::<SceneDialogTask>() else {
        return;
    };

    match &mut task {
        SceneDialogTask::Open(t) => {
            let Some(result) = future::block_on(future::poll_once(t)) else {
                world.insert_resource(task); // Not ready, put it back
                return;
            };
            if let Some(file) = result {
                let path = file.path().to_path_buf();
                crate::native_dialog::remember_pick(
                    world,
                    crate::native_dialog::DialogPurpose::Scene,
                    &path,
                );
                crate::scenes::operators::scene_open_system(world, &path);
            }
        }
        SceneDialogTask::Save(t) => {
            let Some(result) = future::block_on(future::poll_once(t)) else {
                world.insert_resource(task); // Not ready, put it back
                return;
            };
            if let Some(file) = result {
                let path = file.path().to_path_buf();
                let last_dir = path.parent().map(std::path::Path::to_path_buf);

                // The dialog resolves against the active tab, which need not
                // be the one it was opened over, so re-check before the
                // retarget below renames a tab whose world is not its file.
                if let Some(reason) = crate::scene_io::save::active_tab_refusal(world) {
                    error!(
                        "Cannot save this tab: its scene was not loaded ({reason}). \
                         Fix the file and reopen it; nothing has been written."
                    );
                    crate::status_bar::notify_error(
                        world,
                        "Not saved: this tab's scene was not loaded".to_string(),
                    );
                    return;
                }

                // Bind the picked path onto the active scene tab so
                // subsequent swaps/saves go to the right file, and the
                // dirty-state and display name reflect "saved scene"
                // instead of "untitled-N". One function moves everything that follows a
                // scene to a new name, so a rename cannot take part of it.
                crate::scene_io::retarget_active_scene(world, &path.to_string_lossy());
                world.resource_mut::<SceneFilePath>().last_directory = last_dir;
                crate::native_dialog::remember_pick(
                    world,
                    crate::native_dialog::DialogPurpose::Scene,
                    &path,
                );

                match save_scene_inner(world) {
                    Ok(()) => {}
                    Err(err) => error!("scene save (after Save As dialog) failed: {err}"),
                }
            }
        }
    }
}

#[cfg(test)]
mod terrain_sidecar_import_tests {
    use std::path::PathBuf;

    use bevy::prelude::*;
    use jackdaw_terrain::{RegionTerrainData, TerrainData, sidecar};

    use super::{SidecarImport, import_terrain_sidecars};
    use crate::terrain::TerrainDataStore;

    fn unique_tmp_dir(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("jd_terrin_{}_{label}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    fn on_disk() -> TerrainData {
        TerrainData {
            resolution: 4,
            heights: (0..16).map(|i| i as f32).collect(),
            channels: vec![],
        }
    }

    fn document(data: &TerrainData) -> RegionTerrainData {
        RegionTerrainData::from_legacy_v1(data).expect("a power-of-two resolution migrates")
    }

    /// A world with one terrain naming `data_path`, and a pre-region-format
    /// sidecar for it written beside `zone.bsn` in a fresh temp dir.
    fn world_and_scene(label: &str, data_path: &str) -> (World, PathBuf) {
        let tmp = unique_tmp_dir(label);
        let bytes = sidecar::encode(&on_disk()).expect("encodes");
        std::fs::write(tmp.join(data_path), bytes).expect("write sidecar");

        let mut world = World::new();
        world.insert_resource(TerrainDataStore::default());
        world.spawn(jackdaw_scene_types::Terrain {
            resolution: 4,
            data_path: data_path.to_string(),
            ..default()
        });
        (world, tmp.join("zone.bsn"))
    }

    #[test]
    fn a_terrain_with_no_stored_data_is_hydrated_from_its_sidecar() {
        let (mut world, scene) = world_and_scene("fill", "zone.terrain-0.jdterrain");
        import_terrain_sidecars(
            &mut world,
            &scene.to_string_lossy(),
            SidecarImport::FillMissing,
        );
        assert_eq!(
            world
                .resource::<TerrainDataStore>()
                .heights("zone.terrain-0.jdterrain"),
            on_disk().heights.as_slice(),
        );
        let _ = std::fs::remove_dir_all(scene.parent().expect("temp dir"));
    }

    /// The regression this mode exists for: sculpt, switch tabs without
    /// saving, switch back. The store is the truth, not the older file.
    #[test]
    fn fill_missing_leaves_unsaved_edits_alone() {
        let (mut world, scene) = world_and_scene("unsaved", "zone.terrain-0.jdterrain");
        let unsaved = TerrainData {
            resolution: 4,
            heights: vec![99.0; 16],
            channels: vec![],
        };
        world
            .resource_mut::<TerrainDataStore>()
            .insert("zone.terrain-0.jdterrain".to_string(), document(&unsaved));

        import_terrain_sidecars(
            &mut world,
            &scene.to_string_lossy(),
            SidecarImport::FillMissing,
        );
        assert_eq!(
            world
                .resource::<TerrainDataStore>()
                .heights("zone.terrain-0.jdterrain"),
            vec![99.0; 16].as_slice(),
            "a tab swap must not re-read over unsaved sculpting",
        );
        let _ = std::fs::remove_dir_all(scene.parent().expect("temp dir"));
    }

    /// An explicit open, by contrast, is exactly a request for what is on
    /// disk.
    #[test]
    fn reload_overwrites_what_the_store_was_holding() {
        let (mut world, scene) = world_and_scene("reload", "zone.terrain-0.jdterrain");
        world.resource_mut::<TerrainDataStore>().insert(
            "zone.terrain-0.jdterrain".to_string(),
            document(&TerrainData {
                resolution: 4,
                heights: vec![99.0; 16],
                channels: vec![],
            }),
        );

        import_terrain_sidecars(&mut world, &scene.to_string_lossy(), SidecarImport::Reload);
        assert_eq!(
            world
                .resource::<TerrainDataStore>()
                .heights("zone.terrain-0.jdterrain"),
            on_disk().heights.as_slice(),
        );
        let _ = std::fs::remove_dir_all(scene.parent().expect("temp dir"));
    }

    /// A scene whose sidecar was never copied alongside it opens flat with
    /// a warning rather than failing.
    #[test]
    fn a_missing_sidecar_loads_flat_rather_than_erroring() {
        let tmp = unique_tmp_dir("missing");
        let mut world = World::new();
        world.insert_resource(TerrainDataStore::default());
        world.spawn(jackdaw_scene_types::Terrain {
            resolution: 4,
            data_path: "gone.jdterrain".to_string(),
            ..default()
        });

        import_terrain_sidecars(
            &mut world,
            &tmp.join("zone.bsn").to_string_lossy(),
            SidecarImport::Reload,
        );
        assert!(
            world
                .resource::<TerrainDataStore>()
                .heights("gone.jdterrain")
                .is_empty(),
            "a missing sidecar leaves the terrain flat",
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }
}

/// Settling a loaded terrain onto the geometry its cells are drawn at: the
/// sidecar's own geometry where it states one, and the declared rectangle where
/// it does not.
#[cfg(test)]
mod grid_settling_tests {
    use bevy::prelude::*;
    use jackdaw_terrain::{
        RegionTerrainData,
        region::{RegionCoord, RegionSize, TerrainRegions},
    };

    use super::{SidecarImport, import_terrain_sidecars, settle_terrain_grids};
    use crate::terrain::TerrainDataStore;

    /// Regions covering `span` regions per axis from the origin, at 256 cells per region.
    fn regions_spanning(span: i32) -> TerrainRegions {
        let mut regions = TerrainRegions::new(RegionSize::DEFAULT);
        for rz in 0..span {
            for rx in 0..span {
                regions.ensure_region(RegionCoord::new(rx, rz));
            }
        }
        regions
    }

    /// A document whose regions cover `span` regions per axis.
    fn document(span: i32) -> RegionTerrainData {
        RegionTerrainData {
            regions: regions_spanning(span),
            ..default()
        }
    }

    const DATA_PATH: &str = "scene.terrain-1.jdterrain";

    /// A terrain whose sidecar states no geometry, so the declared rectangle places its
    /// cells.
    fn world_with_legacy_terrain() -> World {
        world_with(256, 4, DATA_PATH)
    }

    fn world_with(resolution: u32, span: i32, data_path: &str) -> World {
        let mut world = World::new();
        let mut store = TerrainDataStore::default();
        // No stored geometry, so the declared rectangle places these cells.
        store.insert(data_path.to_string(), legacy_document(span));
        world.insert_resource(store);
        world.spawn(jackdaw_scene_types::Terrain {
            resolution,
            size: Vec2::splat(100.0),
            data_path: data_path.to_string(),
            ..default()
        });
        world
    }

    /// Where a terrain declaring a `size` by `resolution` rectangle draws the vertex at grid
    /// `(x, z)`, in entity-local space.
    ///
    /// The rectangle is centred on the entity and its `resolution` counts vertices, so the
    /// first sits at `-size/2` and the last on the far edge. The migration reproduces this
    /// mapping; it is spelled out here independently of the code under test.
    fn declared_rect_vertex(size: Vec2, resolution: u32, x: u32, z: u32) -> Vec2 {
        let spacing = size / (resolution.max(2) - 1) as f32;
        -size / 2.0 + Vec2::new(x as f32, z as f32) * spacing
    }

    /// A document as a sidecar without geometry hands it over: cells, and nothing saying
    /// where they sit.
    fn legacy_document(span: i32) -> RegionTerrainData {
        RegionTerrainData {
            grid: None,
            ..document(span)
        }
    }

    /// Where the settled geometry puts the vertex at grid `(x, z)`.
    fn settled_vertex(world: &mut World, x: u32, z: u32) -> Vec2 {
        let grid = world
            .resource::<TerrainDataStore>()
            .grid(DATA_PATH)
            .expect("the load settles a geometry onto every terrain");
        grid.anchor + Vec2::new(x as f32, z as f32) * grid.cell_size
    }

    /// The migration re-describes rather than moves: whatever rectangle a scene declared,
    /// every stored cell comes out of the load at the world position it had.
    ///
    /// Both forms are covered: a scene that elided the pair and refilled it from the
    /// component's defaults, and one that wrote it out explicitly.
    #[test]
    fn a_declared_rects_ground_stays_where_it_was_through_the_migration() {
        for (size, resolution) in [
            // Elided: refilled from the component defaults.
            (Vec2::splat(100.0), 256u32),
            // Explicit, the shape the shape panel offers.
            (Vec2::splat(1024.0), 1024),
            // A 2^k+1 grid, which lands on a whole spacing.
            (Vec2::splat(128.0), 129),
        ] {
            let mut world = World::new();
            let mut store = TerrainDataStore::default();
            store.insert(DATA_PATH.to_string(), legacy_document(4));
            world.insert_resource(store);
            world.spawn(jackdaw_scene_types::Terrain {
                resolution,
                size,
                data_path: DATA_PATH.to_string(),
                ..default()
            });

            settle_terrain_grids(&mut world);

            for (x, z) in [
                (0u32, 0u32),
                (1, 0),
                (0, 1),
                (resolution - 1, resolution - 1),
            ] {
                assert_eq!(
                    settled_vertex(&mut world, x, z),
                    declared_rect_vertex(size, resolution, x, z),
                    "vertex ({x}, {z}) of a {size:?} by {resolution} terrain moved",
                );
            }
        }
    }

    /// A cell is square, so a rectangle asking for two spacings cannot be re-described
    /// exactly. X wins and Z is respaced, which moves ground, so the settling warns.
    #[test]
    fn a_non_square_rect_settles_on_its_x_spacing_and_says_so() {
        let mut world = World::new();
        let mut store = TerrainDataStore::default();
        store.insert(DATA_PATH.to_string(), legacy_document(4));
        world.insert_resource(store);
        world.spawn(jackdaw_scene_types::Terrain {
            resolution: 1024,
            size: Vec2::new(2000.0, 500.0),
            data_path: DATA_PATH.to_string(),
            ..default()
        });

        settle_terrain_grids(&mut world);

        let mut query = world.query::<&jackdaw_scene_types::Terrain>();
        assert_eq!(
            query.single(&world).expect("one terrain").cell_size,
            2000.0 / 1023.0,
            "the X axis spacing is the one a scalar cell size keeps",
        );
        assert_eq!(
            jackdaw_terrain::sidecar::declared_rect_respacing(Vec2::new(2000.0, 500.0), 1024),
            Some((2000.0 / 1023.0, 500.0 / 1023.0)),
            "both spacings are available to name in the warning",
        );
    }

    /// The inlets are read once and emptied, so a saved scene carries the derived cell size
    /// and no rectangle for a later load to re-derive from.
    #[test]
    fn settling_a_terrain_fills_its_cell_size_and_empties_the_inlets() {
        let mut world = world_with(256, 4, DATA_PATH);
        settle_terrain_grids(&mut world);

        let defaults = jackdaw_scene_types::Terrain::default();
        let mut query = world.query::<&jackdaw_scene_types::Terrain>();
        let terrain = query.single(&world).expect("one terrain");
        assert_eq!(terrain.cell_size, 100.0 / 255.0);
        assert_eq!(terrain.size, defaults.size);
        assert_eq!(terrain.resolution, defaults.resolution);
    }

    /// A sidecar that states its own geometry wins over a scene text that declares a
    /// rectangle, the state a save interrupted between its two files leaves behind.
    #[test]
    fn a_sidecar_that_states_its_geometry_outranks_stale_scene_text() {
        use jackdaw_terrain::sidecar::GridGeometry;

        let stated = GridGeometry {
            cell_size: 2.5,
            anchor: Vec2::new(7.0, -3.0),
        };
        let mut world = world_with(256, 4, DATA_PATH);
        world
            .resource_mut::<TerrainDataStore>()
            .set_grid(DATA_PATH, stated);

        settle_terrain_grids(&mut world);

        assert_eq!(
            world.resource::<TerrainDataStore>().grid(DATA_PATH),
            Some(stated)
        );
        let mut query = world.query::<&jackdaw_scene_types::Terrain>();
        assert_eq!(query.single(&world).expect("one terrain").cell_size, 2.5);
    }

    /// A scene opened a second time, with the store warm, reads no sidecar again.
    #[test]
    fn reopening_a_warm_store_reads_no_sidecar_again() {
        use jackdaw_terrain::{RegionTerrainData, sidecar};

        let tmp = std::env::temp_dir().join(format!("jd_fb_warm_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).expect("create temp dir");
        let data_path = "zone.terrain-0.jdterrain";
        std::fs::write(
            tmp.join(data_path),
            sidecar::save(&RegionTerrainData {
                regions: regions_spanning(4),
                ..default()
            })
            .expect("encodes"),
        )
        .expect("write sidecar");

        let mut world = World::new();
        world.insert_resource(TerrainDataStore::default());
        world.spawn(jackdaw_scene_types::Terrain {
            resolution: 256,
            size: Vec2::splat(100.0),
            data_path: data_path.to_string(),
            ..default()
        });
        let scene = tmp.join("zone.bsn").to_string_lossy().to_string();

        let first = import_terrain_sidecars(&mut world, &scene, SidecarImport::FillMissing);
        assert_eq!(first.len(), 1, "the first load reads the sidecar in");

        let second = import_terrain_sidecars(&mut world, &scene, SidecarImport::FillMissing);
        assert!(
            second.is_empty(),
            "the store already holds it, so nothing is read again: {second:?}"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// A tab switch re-runs the import to fill anything missing. With every sidecar already
    /// in the store it imports nothing, costing no file read and moving no stored data.
    #[test]
    fn an_import_that_finds_everything_already_loaded_reads_nothing() {
        let mut world = world_with_legacy_terrain();
        let scene = std::path::Path::new("/nonexistent/zone.bsn");

        // A store holding every sidecar has been through a load, so its terrains are
        // settled onto their geometry.
        settle_terrain_grids(&mut world);

        // Nothing is missing, so FillMissing has nothing to read.
        let before = world.resource::<TerrainDataStore>().get(DATA_PATH).cloned();
        import_terrain_sidecars(
            &mut world,
            &scene.to_string_lossy(),
            SidecarImport::FillMissing,
        );

        assert_eq!(
            world.resource::<TerrainDataStore>().get(DATA_PATH).cloned(),
            before,
            "a no-op import leaves the store alone"
        );
    }
}
