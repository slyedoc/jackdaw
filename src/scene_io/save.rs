use std::path::{Path, PathBuf};

use bevy::{prelude::*, tasks::AsyncComputeTaskPool};

use super::load::SceneDialogTask;
use super::{SceneDirtyState, SceneFilePath};

/// Write `contents` to `path` atomically: write to a temp file beside
/// `path`, then rename over the target. `std::fs::write` truncates the
/// destination in place, so a crash or a full disk mid-write would leave
/// a truncated file where the last-known-good copy used to be; a rename
/// within one directory is a single filesystem operation and cannot
/// observe a half-written state.
///
/// The temp file is created in `path`'s own directory rather than a
/// system temp dir: a rename across filesystems (e.g. `/tmp` to a
/// project on another mount) is not atomic and fails outright on most
/// platforms.
pub(crate) fn write_atomic(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let file_name = path
        .file_name()
        .ok_or_else(|| std::io::Error::other(format!("{} has no file name", path.display())))?;
    let temp_path = parent.join(format!(
        ".{}.tmp-{}",
        file_name.to_string_lossy(),
        std::process::id()
    ));
    if let Err(err) = std::fs::write(&temp_path, contents) {
        let _ = std::fs::remove_file(&temp_path);
        return Err(err);
    }
    if let Err(err) = std::fs::rename(&temp_path, path) {
        let _ = std::fs::remove_file(&temp_path);
        return Err(err);
    }
    Ok(())
}

fn spawn_save_dialog(world: &mut World) {
    let dialog = crate::native_dialog::save_dialog(
        world,
        crate::native_dialog::DialogPurpose::Scene,
        "scene.bsn",
    )
    .add_filter("BSN Scene", &["bsn", "bsb"]);

    let task =
        AsyncComputeTaskPool::get().spawn(crate::native_dialog::unless_suppressed(move || {
            dialog.save_file()
        }));
    world.insert_resource(SceneDialogTask::Save(task));
}

/// What happened when [`save_scene`] or [`save_scene_with_outcome`] ran.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SaveOutcome {
    /// The authoritative files reached disk.
    Saved,
    /// The active tab had no path, so a Save As dialog was opened
    /// instead of writing anything. Whether the scene ends up saved
    /// depends on what the user picks and is only known once
    /// `poll_scene_dialog` observes the dialog resolve -- a caller that
    /// needs to act on an eventual success (not just "a save was
    /// attempted") has to defer that action to there, not treat this
    /// outcome as failure and give up.
    DialogOpened,
    /// The save was attempted and failed.
    Failed,
}

/// Save the active scene, returning whether its authoritative files
/// reached disk. A thin `bool` view over [`save_scene_with_outcome`] for
/// callers that only care about "did the save happen right now" and have
/// no work to defer past an opened Save As dialog.
pub fn save_scene(world: &mut World) -> bool {
    save_scene_with_outcome(world) == SaveOutcome::Saved
}

/// Save the active scene, distinguishing an opened Save As dialog from
/// an outright failure. See [`SaveOutcome`].
pub fn save_scene_with_outcome(world: &mut World) -> SaveOutcome {
    // The active scene tab is the source of truth for which file to
    // save to. Re-sync the global `SceneFilePath` from it so a stale
    // path from a previous tab can never cause us to overwrite the
    // wrong file. Untitled tabs (no path) fall through to Save As.
    // A refused tab keeps its path in front of an empty world, so without
    // this check the save would write that empty world over the file.
    if let Some(reason) = active_tab_refusal(world) {
        error!(
            "Cannot save this tab: its scene was not loaded ({reason}). \
             Fix the file and reopen it; nothing has been written."
        );
        crate::status_bar::notify_error(
            world,
            "Not saved: this tab's scene was not loaded".to_string(),
        );
        return SaveOutcome::Failed;
    }

    let active_tab_path: Option<String> = world
        .get_resource::<crate::scenes::Scenes>()
        .and_then(|s| s.tabs.get(s.active).and_then(|t| t.path.clone()))
        .map(|p| p.to_string_lossy().into_owned());
    if let Some(mut spath) = world.get_resource_mut::<SceneFilePath>() {
        spath.path = active_tab_path;
    }

    let has_path = world.resource::<SceneFilePath>().path.is_some();
    if !has_path {
        save_scene_as(world);
        return SaveOutcome::DialogOpened;
    }

    match save_scene_inner(world) {
        Ok(()) => SaveOutcome::Saved,
        Err(err) => {
            error!("scene save failed: {err}");
            crate::status_bar::notify_error(world, format!("Not saved: {err}"));
            SaveOutcome::Failed
        }
    }
}

/// Point the active tab, and the global scene path, at `path`.
///
/// The tab keeps its contents and its history and takes the new file. The next
/// save writes there, and everything that lives beside a scene follows it.
pub fn retarget_active_scene(world: &mut World, path: &str) {
    let path = PathBuf::from(path);
    let display_name = path
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_else(|| "untitled".to_string());
    if let Some(mut scene_path) = world.get_resource_mut::<SceneFilePath>() {
        scene_path.path = Some(path.to_string_lossy().into_owned());
    }
    if let Some(mut scenes) = world.get_resource_mut::<crate::scenes::Scenes>() {
        let active = scenes.active;
        if let Some(tab) = scenes.tabs.get_mut(active) {
            tab.path = Some(path.clone());
            tab.display_name = display_name;
        }
    }
    // A rename leaves the ground unchanged, so the bake taken from it follows the new name.
    // Left pointed at the old file, the check that stops one scene's navmesh landing beside
    // another's would refuse the next save's artifact write.
    if let Some(mut state) =
        world.get_resource_mut::<crate::terrain::navmesh_bake::TerrainNavmeshState>()
        && let Some(baked) = state.baked.as_mut()
    {
        baked.scene = Some(path);
    }
}

pub fn save_scene_as(world: &mut World) {
    // Save As performs the same write, so a refused tab is refused here too.
    if let Some(reason) = active_tab_refusal(world) {
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
    if world.contains_resource::<SceneDialogTask>() {
        return; // Dialog already open
    }
    spawn_save_dialog(world);
}

/// Why the active tab must not be written, if it must not be. A refused
/// activation is the one state where the live world is not the tab's
/// contents.
pub(crate) fn active_tab_refusal(world: &World) -> Option<String> {
    let scenes = world.get_resource::<crate::scenes::Scenes>()?;
    let tab = scenes.tabs.get(scenes.active)?;
    tab.refusal
        .as_ref()
        .map(|refusal| refusal.reason().to_string())
}

/// Save the active tab's scene text and terrain sidecars, synchronously,
/// on the calling (main) thread.
///
/// The write is not spawned onto an async task, because ordering has to
/// hold across the whole boundary -- sidecars before scene text, dirty
/// state cleared only after every authoritative write lands (see the
/// ordering comments below) -- and that is far simpler to
/// reason about, and to keep correct under future edits, as a single
/// synchronous call than as a task whose completion has to be raced
/// against the next save, the next tab swap, or the app closing mid-write.
///
/// The tradeoff: this blocks the main thread for the duration of the
/// write, and `scene.save_all` (`scenes/operators.rs`) calls this once
/// per dirty tab in a loop, so the block time is `O(tabs)`, not `O(1)`.
/// Scene text and terrain sidecars are not sized to make that
/// noticeable in practice, but a project with many large open tabs is
/// the case to watch. If it ever needs revisiting, the fix is a
/// different concurrency shape for `scene.save_all` specifically, not an
/// async version of this function -- the ordering argument above applies
/// to any single save.
pub(crate) fn save_scene_inner(world: &mut World) -> Result<(), BevyError> {
    // Every write of the world to the active tab's file ends here, so the
    // refusal is enforced here too; callers check only for a better message.
    if let Some(reason) = active_tab_refusal(world) {
        crate::status_bar::notify_error(
            world,
            "Not saved: this tab's scene was not loaded".to_string(),
        );
        return Err(BevyError::from(format!(
            "cannot save this tab: its scene was not loaded ({reason})"
        )));
    }

    let path = {
        let scene_path = world.resource::<SceneFilePath>();
        scene_path
            .path
            .clone()
            .expect("save_scene_inner called without a path set")
    };

    // Refresh the save timestamps carried on the scene metadata.
    let now = crate::timestamps::utc_rfc3339_now();
    let mut scene_path = world.resource_mut::<SceneFilePath>();
    scene_path.metadata.modified = now.clone();
    if scene_path.metadata.created.is_empty() {
        scene_path.metadata.created = now;
    }
    if scene_path.metadata.name.is_empty() {
        scene_path.metadata.name = "Untitled".to_string();
    }

    let contents = {
        let parent_path = Path::new(&path)
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_default();
        let body = emit_bsn_scene_for_file(world, &parent_path)
            .map_err(|refusal| BevyError::from(format!("cannot save {path}: {refusal}")))?;
        // Record the jackdaw + Bevy version at the disk boundary only, so
        // the in-memory undo / tab-swap snapshots from the same emitter stay
        // stamp-free.
        crate::scene_io::stamp::with_stamp(&body)
    };

    // Terrain heights and paint channels are authoritative authored data.
    // Complete those writes before the scene text is committed and before
    // the tab is marked clean, so failures remain visible and repeated saves
    // cannot finish out of order. Each write lands via write_atomic, so a
    // sidecar failure partway through never truncates one that already
    // landed; the scene text below is written the same way. There is no
    // cross-file rollback if the scene text write fails after sidecars
    // already landed -- the sidecars stay updated and the tab stays dirty,
    // so the next save retries the scene text with the same sidecars.
    export_terrain_sidecars(world, &path)?;

    let contents = contents.into_bytes();
    write_atomic(Path::new(&path), &contents)
        .map_err(|err| BevyError::from(format!("failed to write scene file {path}: {err}")))?;
    info!("Scene saved to {path}");

    // Record the written bytes so the open-tab watcher does not read this
    // write back as an outside edit.
    crate::scenes::external_watch::note_known_content(world, Path::new(&path), &contents);

    // A terrain bake writes its own artifact when it finishes; this covers a scene that has
    // moved since, keeping the two files named after each other.
    crate::terrain::navmesh_bake::export_beside_scene(world, &path);

    // The authoritative scene and sidecars are now on disk. Only now clear
    // dirty state and retarget a redirected tab at its new `.bsn` path.
    let history_len = world
        .resource::<jackdaw_commands::CommandHistory>()
        .undo_stack
        .len();
    world.resource_mut::<SceneDirtyState>().undo_len_at_save = history_len;
    if let Some(mut scenes) = world.get_resource_mut::<crate::scenes::Scenes>() {
        let active = scenes.active;
        if let Some(tab) = scenes.tabs.get_mut(active) {
            tab.dirty = false;
            tab.history_depth_at_last_check = history_len;
        }
    }

    // Persist current editor layout to project.jsn
    save_layout_to_project(world);

    crate::thumbnail::capture_scene_thumbnail(world, Path::new(&path));

    Ok(())
}

/// Write every terrain's bulk data to its sidecar beside the scene.
///
/// A scene with no terrain is a clean no-op. Each write completes before
/// returning (atomically, via `write_atomic`), and an invalid path,
/// encoding failure, or filesystem error is returned to the save boundary
/// so the tab stays dirty. One file per terrain is named by the
/// scene-relative `data_path` the `Terrain` component carries, so a scene
/// and its terrain data move together.
///
/// A path with no store entry (never loaded -- its sidecar was missing --
/// and never edited) is silently skipped: there is nothing to write and
/// nothing lost. A path marked load-failed (a sidecar existed but could
/// not decode) is also skipped, with a warning: writing zeroed data over
/// a real, if damaged, file would be worse than leaving it alone. Neither
/// case fails the save.
///
/// Only terrains that are actually in the scene are written. The store
/// can outlive them -- an undo can bring one back, and other tabs keep
/// their own entries -- so writing the whole store would scatter files
/// for terrains this scene does not own.
pub(crate) fn export_terrain_sidecars(
    world: &mut World,
    scene_path: &str,
) -> Result<(), BevyError> {
    use jackdaw_terrain::sidecar;

    if world
        .get_resource::<crate::terrain::TerrainDataStore>()
        .is_none()
    {
        return Ok(());
    }
    let scene_dir = Path::new(scene_path)
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_default();

    let mut paths: Vec<String> = Vec::new();
    let mut query = world.query_filtered::<&jackdaw_scene_types::Terrain, With<crate::scene_io::SceneEntity>>();
    for terrain in query.iter(world) {
        if !terrain.data_path.is_empty() && !paths.contains(&terrain.data_path) {
            paths.push(terrain.data_path.clone());
        }
    }

    let store = world.resource::<crate::terrain::TerrainDataStore>();
    let mut writes: Vec<(String, PathBuf, Vec<u8>)> = Vec::new();
    for data_path in paths {
        let path = match sidecar::resolve_path(&scene_dir, &data_path) {
            Ok(path) => path,
            Err(err) => {
                return Err(BevyError::from(format!(
                    "invalid terrain data path {data_path:?}: {err}"
                )));
            }
        };
        if store.is_load_failed(&data_path) {
            warn!(
                "Not saving terrain data {data_path:?}: it failed to load, so writing \
                 would overwrite the original file with empty data. Fix or replace the \
                 sidecar and reload the scene."
            );
            continue;
        }
        // No entry means this path was never loaded (its sidecar was
        // missing, and the load stayed lenient) and never edited since:
        // nothing to write, and nothing lost by skipping it.
        let Some(data) = store.get(&data_path) else {
            continue;
        };
        let bytes = sidecar::save(data).map_err(|err| {
            BevyError::from(format!(
                "terrain sidecar {data_path:?} cannot be written: {err}"
            ))
        })?;
        writes.push((data_path, path, bytes));
    }

    for (data_path, path, bytes) in writes {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|err| {
                BevyError::from(format!(
                    "failed to create terrain data directory {} for {}: {err}",
                    parent.display(),
                    path.display()
                ))
            })?;
        }
        write_atomic(&path, &bytes).map_err(|err| {
            BevyError::from(format!(
                "failed to write terrain data {}: {err}",
                path.display()
            ))
        })?;
        info!("Terrain data written to {}", path.display());
        // Without noting the write, the store keeps the mtime it loaded from
        // and every later activation re-imports the whole terrain.
        let mtime = std::fs::metadata(&path)
            .and_then(|meta| meta.modified())
            .ok();
        world
            .resource_mut::<crate::terrain::TerrainDataStore>()
            .note_read(&data_path, mtime);
    }
    Ok(())
}

pub fn save_layout_to_project(world: &mut World) {
    let Some(root) = world
        .get_resource::<crate::project::ProjectRoot>()
        .map(|p| p.root.clone())
    else {
        return;
    };

    // Snapshot the live tree into the active workspace before
    // serializing, so the saved registry reflects what's on screen.
    let live_tree = world.resource::<jackdaw_panels::tree::DockTree>().clone();
    let active_id = world
        .resource::<jackdaw_panels::WorkspaceRegistry>()
        .active
        .clone();
    if let Some(id) = active_id {
        let mut registry = world.resource_mut::<jackdaw_panels::WorkspaceRegistry>();
        if let Some(ws) = registry.get_mut(&id) {
            ws.tree = live_tree;
        }
    }

    let persist = jackdaw_panels::WorkspacesPersist::from_registry(
        world.resource::<jackdaw_panels::WorkspaceRegistry>(),
    );
    let layout_json = match serde_json::to_value(&persist) {
        Ok(v) => v,
        Err(e) => {
            warn!("Failed to serialize workspaces: {e}");
            return;
        }
    };

    let mut project = world
        .resource_mut::<crate::project::ProjectRoot>()
        .config
        .clone();
    project.layout = Some(layout_json);

    if let Err(e) = crate::project::save_project_config(&root, &project) {
        warn!("Failed to save project config: {e}");
    } else {
        world.resource_mut::<crate::project::ProjectRoot>().config = project;
    }
}

/// Whether the active tab holds a file whose root is its one entity (a character, a prefab)
/// rather than a scene of top-level entities.
fn active_tab_is_root_file(world: &World) -> bool {
    world
        .get_resource::<crate::scenes::Scenes>()
        .and_then(|scenes| scenes.tabs.get(scenes.active))
        .is_some_and(|tab| tab.kind == crate::scenes::TabKind::Prefab)
}

/// The open scene as `.bsn` text. A tab holding a root-entity file writes that entity; a scene
/// writes its top-level entities under the unnamed root a scene file has.
pub fn scene_text(world: &mut World) -> Result<String, String> {
    let roots = super::scene_roots(world);
    let settings = super::write_settings(world);
    if active_tab_is_root_file(world) && roots.len() == 1 {
        return bevy::bsn_asset::write_scene_text(world, roots[0], &settings)
            .map_err(|err| err.to_string());
    }
    bevy::bsn_asset::write_scene_roots_text(world, &roots, &settings).map_err(|err| err.to_string())
}

/// [`scene_text`], for a save.
pub fn emit_bsn_scene_for_file(world: &mut World, _parent_path: &Path) -> Result<String, String> {
    scene_text(world)
}

/// [`scene_text`], for a snapshot that has to produce something.
pub fn emit_bsn_scene_with_inline_assets(world: &mut World, _parent_path: &Path) -> String {
    scene_text(world).unwrap_or_else(|err| {
        warn!("scene snapshot: {err}");
        String::new()
    })
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_named_save_renames_the_tab_it_saves() {
        use crate::scenes::{SceneTab, Scenes};

        let mut world = World::new();
        world.init_resource::<SceneFilePath>();
        let mut scenes = Scenes::default();
        scenes.push_tab(SceneTab::new_untitled(1));
        world.insert_resource(scenes);

        retarget_active_scene(&mut world, "/tmp/zones/harbour.bsn");

        assert_eq!(
            world.resource::<SceneFilePath>().path.as_deref(),
            Some("/tmp/zones/harbour.bsn")
        );
        let scenes = world.resource::<Scenes>();
        assert_eq!(scenes.tabs[0].display_name, "harbour");
        assert_eq!(
            scenes.tabs[0].path.as_deref(),
            Some(Path::new("/tmp/zones/harbour.bsn"))
        );
    }

    /// C2: `write_atomic` must fully replace an existing file's content
    /// (proving the write goes through a rename, not an in-place
    /// truncate) and must not leave its temp file behind.
    #[test]
    fn write_atomic_replaces_existing_content_and_cleans_up_its_temp_file() {
        let tmp = tempfile::tempdir().expect("temp directory");
        let target = tmp.path().join("scene.bsn");
        std::fs::write(&target, b"old contents, much longer than the new ones")
            .expect("seed original file");

        write_atomic(&target, b"new").expect("atomic write succeeds");

        assert_eq!(std::fs::read(&target).expect("read back"), b"new");
        let stray_temp = std::fs::read_dir(tmp.path())
            .expect("read temp dir")
            .filter_map(Result::ok)
            .any(|entry| entry.path() != target);
        assert!(!stray_temp, "no temp file may remain beside the target");
    }

    /// C2: a failed write must not touch the destination at all -- the
    /// crash-safety property this exists for is that a partial write can
    /// only ever land in the temp file, never in the file callers read.
    #[test]
    fn write_atomic_leaves_the_destination_untouched_on_failure() {
        let tmp = tempfile::tempdir().expect("temp directory");
        let missing_parent = tmp.path().join("does-not-exist").join("scene.bsn");

        let err = write_atomic(&missing_parent, b"new").expect_err("parent dir is missing");
        assert!(!missing_parent.exists());
        assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
    }
}

/// Tests for the terrain sidecar sibling writer.
///
/// These drive the synchronous save boundary directly: success means the
/// bytes are already durable enough to read back, and failure is returned.
#[cfg(test)]
mod terrain_sidecar_tests {
    use std::path::PathBuf;

    use bevy::prelude::*;
    use jackdaw_terrain::{RegionTerrainData, TerrainData, sidecar};

    use super::{emit_bsn_scene_with_inline_assets, export_terrain_sidecars};
    use crate::terrain::TerrainDataStore;

    fn unique_tmp_dir(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("jd_terr_{}_{label}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    /// A sculpted 4x4 terrain, distinctive enough that a zeroed or
    /// truncated round-trip fails loudly.
    fn sculpted() -> TerrainData {
        TerrainData {
            resolution: 4,
            heights: (0..16).map(|i| i as f32 * 0.5).collect(),
            channels: vec![],
        }
    }

    /// A store holding ground for `terrain`, in regions sized to keep the fixture small.
    /// Nothing allocates implicitly, so a terrain a test writes to is laid down first.
    fn store_holding(terrain: &jackdaw_scene_types::Terrain) -> TerrainDataStore {
        let mut regions = jackdaw_terrain::TerrainRegions::new(
            jackdaw_terrain::RegionSize::new(terrain.resolution.next_power_of_two())
                .expect("a power of two"),
        );
        regions
            .ensure_grid(terrain.resolution)
            .expect("inside the region cap");
        let mut store = TerrainDataStore::default();
        store.insert(
            terrain.data_path.clone(),
            jackdaw_terrain::RegionTerrainData {
                regions,
                ..Default::default()
            },
        );
        store
    }

    fn document(data: &TerrainData) -> RegionTerrainData {
        let mut document =
            RegionTerrainData::from_legacy_v1(data).expect("a power-of-two resolution migrates");
        // The load path settles a document onto the geometry its cells are drawn at before
        // a save, so a stand-in for a document the editor holds states its own.
        document.grid = Some(jackdaw_terrain::sidecar::GridGeometry::DEFAULT);
        document
    }

    fn world_with_terrain(data_path: &str, data: TerrainData) -> World {
        let mut world = World::new();
        let mut store = TerrainDataStore::default();
        store.insert(data_path.to_string(), document(&data));
        world.insert_resource(store);
        world.spawn(jackdaw_scene_types::Terrain {
            resolution: 4,
            data_path: data_path.to_string(),
            ..default()
        });
        world
    }

    fn read_decode(path: &std::path::Path) -> Option<RegionTerrainData> {
        std::fs::read(path)
            .ok()
            .and_then(|bytes| sidecar::load(&bytes).ok())
    }

    #[test]
    fn writes_a_sidecar_that_decodes_back_to_the_sculpted_heights() {
        let tmp = unique_tmp_dir("roundtrip");
        let scene_path = tmp.join("zone.bsn");
        let sidecar_path = tmp.join("zone.terrain-0.jdterrain");

        let mut world = world_with_terrain("zone.terrain-0.jdterrain", sculpted());
        export_terrain_sidecars(&mut world, &scene_path.to_string_lossy())
            .expect("sidecar write succeeds");

        let decoded = read_decode(&sidecar_path)
            .unwrap_or_else(|| panic!("sidecar not written: {}", sidecar_path.display()));
        assert_eq!(decoded, document(&sculpted()));

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// Saving twice with no edits in between must produce the same bytes,
    /// so a sidecar can be committed and diffed like any other artifact.
    #[test]
    fn saving_twice_produces_identical_bytes() {
        let tmp = unique_tmp_dir("stable");
        let scene_path = tmp.join("zone.bsn");
        let sidecar_path = tmp.join("zone.terrain-0.jdterrain");

        let mut world = world_with_terrain("zone.terrain-0.jdterrain", sculpted());
        export_terrain_sidecars(&mut world, &scene_path.to_string_lossy())
            .expect("first write succeeds");
        let first = std::fs::read(&sidecar_path).expect("read first write");

        export_terrain_sidecars(&mut world, &scene_path.to_string_lossy())
            .expect("second write succeeds");
        let second = std::fs::read(&sidecar_path).expect("read second write");

        assert_eq!(first, second);

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn a_sidecar_write_failure_is_returned_to_the_save_caller() {
        let tmp = unique_tmp_dir("write-failure");
        let blocked_parent = tmp.join("not-a-directory");
        std::fs::write(&blocked_parent, b"file blocks directory creation")
            .expect("create blocking file");
        let scene_path = blocked_parent.join("zone.bsn");
        let sidecar_path = blocked_parent.join("zone.terrain-0.jdterrain");

        let mut world = world_with_terrain("zone.terrain-0.jdterrain", sculpted());
        let error = export_terrain_sidecars(&mut world, &scene_path.to_string_lossy())
            .expect_err("the write failure must reach the save boundary");

        assert!(
            error
                .to_string()
                .contains(&sidecar_path.display().to_string()),
            "the error names the failed sidecar: {error}",
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// A scene with no terrain must not litter the project with an empty
    /// sidecar, exactly as the navmesh writer no-ops when nothing is baked.
    #[test]
    fn no_sidecar_when_the_scene_has_no_terrain() {
        let tmp = unique_tmp_dir("bare");
        let scene_path = tmp.join("bare.bsn");

        let mut world = World::new();
        world.insert_resource(TerrainDataStore::default());
        export_terrain_sidecars(&mut world, &scene_path.to_string_lossy())
            .expect("terrain-less scene is a no-op");
        let stray = std::fs::read_dir(&tmp)
            .expect("temp dir readable")
            .filter_map(Result::ok)
            .any(|e| {
                e.path()
                    .extension()
                    .is_some_and(|ext| ext == sidecar::EXTENSION)
            });
        assert!(!stray, "no sidecar may be written for a terrain-less scene");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// Heights live in the sidecar, not the scene text, so a terrain adds only
    /// a handful of lines to a `.bsn`.
    #[test]
    fn a_512_terrain_contributes_almost_nothing_to_the_scene_text() {
        use bevy::ecs::reflect::AppTypeRegistry;

        let mut world = World::new();
        world.init_resource::<AppTypeRegistry>();
        {
            let registry = world.resource::<AppTypeRegistry>().clone();
            let mut writer = registry.write();
            writer.register::<Name>();
            writer.register::<jackdaw_scene_types::Terrain>();
        }

        let mut store = TerrainDataStore::default();
        store.insert(
            "zone.terrain-0.jdterrain".to_string(),
            document(&TerrainData {
                resolution: 512,
                heights: vec![0.75; 512 * 512],
                channels: vec![],
            }),
        );
        world.insert_resource(store);

        let entity = world
            .spawn((
                Name::new("ground"),
                jackdaw_scene_types::Terrain {
                    resolution: 512,
                    data_path: "zone.terrain-0.jdterrain".to_string(),
                    ..default()
                },
            ))
            .id();
        crate::scene_io::adopt_entity(&mut world, entity);

        let text = emit_bsn_scene_with_inline_assets(&mut world, std::path::Path::new("."));
        assert!(
            text.contains("zone.terrain-0.jdterrain"),
            "the scene must name the sidecar it depends on:\n{text}"
        );
        assert!(
            text.len() < 2048,
            "a 512 terrain must not bloat the scene text; got {} bytes:\n{text}",
            text.len()
        );
    }

    /// Store entries whose terrain is not in this scene belong to another
    /// tab (or to an undone delete) and must not be written beside it.
    #[test]
    fn only_terrains_present_in_the_scene_are_written() {
        let tmp = unique_tmp_dir("scoped");
        let scene_path = tmp.join("zone.bsn");

        let mut world = world_with_terrain("zone.terrain-0.jdterrain", sculpted());
        world.resource_mut::<TerrainDataStore>().insert(
            "other-scene.terrain-0.jdterrain".to_string(),
            document(&sculpted()),
        );
        export_terrain_sidecars(&mut world, &scene_path.to_string_lossy())
            .expect("this scene's sidecar writes");

        assert!(
            read_decode(&tmp.join("zone.terrain-0.jdterrain")).is_some(),
            "this scene's terrain is written"
        );
        assert!(
            !tmp.join("other-scene.terrain-0.jdterrain").exists(),
            "another scene's terrain must not be written here"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// Painting and saving over a sidecar from a newer build must not write a
    /// zeroed document over it.
    #[test]
    fn an_unreadable_sidecar_is_never_overwritten_by_a_save() {
        let tmp = unique_tmp_dir("unreadable");
        let scene_path = tmp.join("zone.bsn");
        let sidecar_path = tmp.join("zone.terrain-0.jdterrain");

        let mut original = sidecar::save(&document(&sculpted())).expect("encodes");
        original[8..10].copy_from_slice(&(sidecar::VERSION_10 + 1).to_le_bytes());
        std::fs::write(&sidecar_path, &original).expect("write sidecar");

        let mut world = World::new();
        world.insert_resource(TerrainDataStore::default());
        world.spawn(jackdaw_scene_types::Terrain {
            resolution: 4,
            data_path: "zone.terrain-0.jdterrain".to_string(),
            ..default()
        });

        crate::scene_io::import_terrain_sidecars(
            &mut world,
            &scene_path.to_string_lossy(),
            crate::scene_io::SidecarImport::Reload,
        );
        assert!(
            world
                .resource::<TerrainDataStore>()
                .is_load_failed("zone.terrain-0.jdterrain"),
            "a decode failure must mark the entry load-failed",
        );

        // The stroke: an edit attempt must be refused, not minted as
        // zeroed data.
        let brushed = jackdaw_scene_types::Terrain {
            resolution: 4,
            data_path: "zone.terrain-0.jdterrain".to_string(),
            ..default()
        };
        assert!(
            world
                .resource_mut::<TerrainDataStore>()
                .entry_for(&brushed)
                .is_none(),
            "edits to a load-failed terrain must be refused",
        );

        // Ctrl+S: the save must succeed and must not touch the file.
        export_terrain_sidecars(&mut world, &scene_path.to_string_lossy())
            .expect("save must not hard-error on a load-failed entry");
        assert_eq!(
            std::fs::read(&sidecar_path).expect("sidecar still on disk"),
            original,
            "the unreadable original must survive the save byte-for-byte",
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// The texture-set reference and every cell's control word round-trip
    /// through a save unchanged.
    #[test]
    fn authored_paint_and_materials_survive_a_save_and_reload() {
        use jackdaw_terrain::Control;

        let tmp = unique_tmp_dir("paint-roundtrip");
        let scene_path = tmp.join("zone.bsn");
        let data_path = "zone.terrain-0.jdterrain";
        let terrain = jackdaw_scene_types::Terrain {
            resolution: 4,
            data_path: data_path.to_string(),
            ..default()
        };
        let painted = Control::default()
            .with_base_id(3)
            .with_overlay_id(7)
            .with_blend(200);

        let mut world = World::new();
        world.insert_resource(store_holding(&terrain));
        world.spawn(terrain.clone());
        let authored = {
            let mut store = world.resource_mut::<TerrainDataStore>();
            store
                .entry_for(&terrain)
                .expect("keyed")
                .set_heights(&sculpted().heights);
            store.control_mut(&terrain).expect("keyed")[5] = painted;
            store
                .set_materials(
                    data_path,
                    vec![
                        jackdaw_terrain::sidecar::TerrainMaterialSlot::new("grass"),
                        jackdaw_terrain::sidecar::TerrainMaterialSlot {
                            material: "rock_05".to_string(),
                            uv_scale: 0.25,
                            detile: 0.5,
                            occlusion: String::new(),
                            roughness: String::new(),
                        },
                    ],
                )
                .expect("plain material names are accepted");
            store.get(data_path).expect("authored").clone()
        };

        export_terrain_sidecars(&mut world, &scene_path.to_string_lossy()).expect("save succeeds");

        // A fresh store, as a reopened editor has.
        let mut reopened = World::new();
        reopened.insert_resource(TerrainDataStore::default());
        reopened.spawn(terrain.clone());
        crate::scene_io::import_terrain_sidecars(
            &mut reopened,
            &scene_path.to_string_lossy(),
            crate::scene_io::SidecarImport::Reload,
        );

        let store = reopened.resource::<TerrainDataStore>();
        assert_eq!(
            store.get(data_path),
            Some(&authored),
            "the reloaded document must equal what was authored",
        );
        assert_eq!(store.control(data_path)[5], painted);
        assert_eq!(store.materials(data_path).len(), 2);
        assert_eq!(store.materials(data_path)[1].material, "rock_05");
        assert_eq!(store.materials(data_path)[1].uv_scale, 0.25);
        assert_eq!(store.materials(data_path)[1].detile, 0.5);
        assert_eq!(store.heights(data_path), sculpted().heights.as_slice());
        assert!(
            store.take_control_dirty(data_path).is_some(),
            "loaded paint must be marked for upload, or it never reaches the material",
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// 129 vertices per edge is 2^7 + 1, so no single power-of-two region holds it. It has
    /// to open, be editable, and round-trip every height, seam row included.
    #[test]
    fn a_non_power_of_two_sidecar_opens_and_embeds_every_height() {
        let tmp = unique_tmp_dir("non-pow2");
        let scene_path = tmp.join("zone.bsn");
        let sidecar_path = tmp.join("zone.terrain-0.jdterrain");

        let heights: Vec<f32> = (0..129 * 129).map(|i| i as f32 * 0.5).collect();
        let original = sidecar::encode(&TerrainData {
            resolution: 129,
            heights: heights.clone(),
            channels: vec![],
        })
        .expect("v1 encodes");
        std::fs::write(&sidecar_path, &original).expect("write sidecar");

        let odd = jackdaw_scene_types::Terrain {
            resolution: 129,
            data_path: "zone.terrain-0.jdterrain".to_string(),
            ..default()
        };
        let mut world = World::new();
        world.insert_resource(TerrainDataStore::default());
        world.spawn(odd.clone());

        crate::scene_io::import_terrain_sidecars(
            &mut world,
            &scene_path.to_string_lossy(),
            crate::scene_io::SidecarImport::Reload,
        );
        let store = world.resource::<TerrainDataStore>();
        assert!(
            !store.is_load_failed("zone.terrain-0.jdterrain"),
            "a 2^k + 1 vertex grid is storable and must not be quarantined",
        );
        // A 129-vertex grid lands in the 2x2 block of 128-cell regions that holds it, so the
        // terrain is 256 cells across with the authored 129 embedded at its corner. Each
        // authored height stays on the cell it described; the rest is ground the regions
        // brought with them.
        let document = store.get("zone.terrain-0.jdterrain").expect("loaded");
        assert_eq!(document.grid_resolution(), 256);
        for (at, want) in heights.iter().enumerate() {
            let (x, z) = ((at % 129) as i32, (at / 129) as i32);
            assert_eq!(document.regions.height_at(x, z), *want);
        }
        assert!(
            world
                .resource_mut::<TerrainDataStore>()
                .entry_for(&odd)
                .is_some(),
            "edits to it must be accepted",
        );

        export_terrain_sidecars(&mut world, &scene_path.to_string_lossy()).expect("save succeeds");
        let reloaded = read_decode(&sidecar_path).expect("sidecar rewritten");
        for (at, want) in heights.iter().enumerate() {
            let (x, z) = ((at % 129) as i32, (at / 129) as i32);
            assert_eq!(
                reloaded.regions.height_at(x, z),
                *want,
                "the rewrite must keep every height, seam row included",
            );
        }
        assert_eq!(reloaded.regions.region_size().get(), 128);

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// A pre-region sidecar opens, migrates, and is rewritten in the current
    /// format with no user action.
    #[test]
    fn a_pre_region_sidecar_migrates_on_load_and_saves_in_the_current_format() {
        let tmp = unique_tmp_dir("migrate");
        let scene_path = tmp.join("zone.bsn");
        let sidecar_path = tmp.join("zone.terrain-0.jdterrain");
        std::fs::write(
            &sidecar_path,
            sidecar::encode(&sculpted()).expect("v1 encodes"),
        )
        .expect("write sidecar");

        let mut world = World::new();
        world.insert_resource(TerrainDataStore::default());
        world.spawn(jackdaw_scene_types::Terrain {
            resolution: 4,
            data_path: "zone.terrain-0.jdterrain".to_string(),
            ..default()
        });
        crate::scene_io::import_terrain_sidecars(
            &mut world,
            &scene_path.to_string_lossy(),
            crate::scene_io::SidecarImport::Reload,
        );
        assert_eq!(
            world
                .resource::<TerrainDataStore>()
                .heights("zone.terrain-0.jdterrain"),
            sculpted().heights.as_slice(),
        );

        export_terrain_sidecars(&mut world, &scene_path.to_string_lossy()).expect("save succeeds");
        let rewritten = std::fs::read(&sidecar_path).expect("read back");
        assert_eq!(
            u16::from_le_bytes([rewritten[8], rewritten[9]]),
            sidecar::VERSION_10,
        );
        // The load settled this terrain onto the geometry its declared rectangle drew with
        // (four vertices across the default 100 metres, cornered at -size/2) and the rewrite
        // records it, so reading the file back needs no rectangle.
        let mut migrated = document(&sculpted());
        migrated.grid = Some(sidecar::GridGeometry {
            cell_size: 100.0 / 3.0,
            anchor: Vec2::splat(-50.0),
        });
        assert_eq!(
            read_decode(&sidecar_path),
            Some(migrated),
            "the migrated document must survive the rewrite",
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// A terrain that was edited and then flattened persists as a region on
    /// disk, rather than an empty document that reopens as no terrain.
    #[test]
    fn a_flat_but_authored_terrain_round_trips_as_a_present_region() {
        let tmp = unique_tmp_dir("flat-authored");
        let scene_path = tmp.join("zone.bsn");
        let data_path = "zone.terrain-0.jdterrain";
        let terrain = jackdaw_scene_types::Terrain {
            resolution: 4,
            data_path: data_path.to_string(),
            ..default()
        };

        let mut world = World::new();
        world.insert_resource(store_holding(&terrain));
        world.spawn(terrain.clone());
        {
            let mut store = world.resource_mut::<TerrainDataStore>();
            let mut entry = store.entry_for(&terrain).expect("keyed");
            entry.heights_mut()[0] = 5.0;
            entry.set_heights(&[0.0; 16]);
        }
        export_terrain_sidecars(&mut world, &scene_path.to_string_lossy()).expect("save succeeds");

        let reloaded = read_decode(&tmp.join(data_path)).expect("sidecar written");
        assert_eq!(reloaded.regions.region_count(), 1);
        assert!(reloaded.contiguous_grid().is_some());

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// C1 pinning test for the twin bug: a scene whose sidecar was never
    /// copied alongside it (missing, not corrupt) loads flat and must
    /// stay saveable indefinitely, not hard-error on every save attempt.
    #[test]
    fn a_never_loaded_missing_sidecar_stays_saveable() {
        let tmp = unique_tmp_dir("never-loaded");
        let scene_path = tmp.join("zone.bsn");
        let sidecar_path = tmp.join("zone.terrain-0.jdterrain");

        let mut world = World::new();
        world.insert_resource(TerrainDataStore::default());
        world.spawn(jackdaw_scene_types::Terrain {
            resolution: 4,
            data_path: "zone.terrain-0.jdterrain".to_string(),
            ..default()
        });

        crate::scene_io::import_terrain_sidecars(
            &mut world,
            &scene_path.to_string_lossy(),
            crate::scene_io::SidecarImport::Reload,
        );
        assert!(
            !world
                .resource::<TerrainDataStore>()
                .contains("zone.terrain-0.jdterrain")
        );
        assert!(
            !world
                .resource::<TerrainDataStore>()
                .is_load_failed("zone.terrain-0.jdterrain")
        );

        export_terrain_sidecars(&mut world, &scene_path.to_string_lossy())
            .expect("a scene with a never-loaded terrain must remain saveable");
        assert!(
            !sidecar_path.exists(),
            "nothing should be written for data that never existed"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
