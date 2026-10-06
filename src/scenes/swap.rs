//! Tab-switch mechanics. The pure pipeline lives here so it can be
//! tested independently of UI and operator wiring.

use bevy::prelude::*;
use jackdaw_api::prelude::*;

use crate::commands::CommandHistory;
use crate::scenes::{Scenes, TabContent, ViewState};

/// Switch the active tab to `target`. No-op if `target == active`.
/// Cancels any in-flight modal first to avoid corrupt per-frame state.
pub fn swap_active_tab(world: &mut World, target: usize) {
    let current = world.resource::<Scenes>().active;
    if current == target {
        return;
    }
    let tab_count = world.resource::<Scenes>().tabs.len();
    if target >= tab_count {
        warn!("swap_active_tab: target {target} out of range (len {tab_count})");
        return;
    }

    // Cancel any in-flight modal so per-frame state doesn't dangle.
    let _ = world.cancel_active_modal();

    capture_active_tab(world);
    activate_tab(world, target);
}

/// Spawn the target tab's snapshot into a world that has just been
/// cleared. Used by `scene_close_system` when the closed tab was the
/// active tab (so the normal `capture_active_tab` step would try to
/// re-capture a tab that's being dropped).
pub fn reactivate_after_close(world: &mut World, target: usize) {
    activate_tab(world, target);
}

/// Send the open scene behind: its entities stay in its world, out of view. Stash its history
/// and view state. Pre-condition: a tab exists at `Scenes.active`.
pub(crate) fn capture_active_tab(world: &mut World) {
    let active = world.resource::<Scenes>().active;
    let view_state = capture_view_state(world);
    let history = std::mem::take(&mut *world.resource_mut::<CommandHistory>());
    if let Some(tab_world) = crate::scene_io::scene_world(world) {
        crate::scene_io::set_tab_open(world, tab_world, false);
    }
    world.resource_mut::<crate::selection::Selection>().entities.clear();
    let _ = world.run_system_cached(crate::hierarchy::clear_all_tree_rows);
    crate::hierarchy::forget_withheld_rows(world);

    let terrain_data_store = world
        .get_resource_mut::<crate::terrain::TerrainDataStore>()
        .map(|mut store| std::mem::take(&mut *store));
    let navmesh = crate::terrain::navmesh_bake::take_from_world(world);
    let mut scenes = world.resource_mut::<Scenes>();
    let tab = &mut scenes.tabs[active];
    tab.content = if tab.is_refused() {
        TabContent::Unloaded
    } else {
        TabContent::Parked
    };
    tab.view_state = view_state;
    tab.history = history;
    if let Some(terrain_data_store) = terrain_data_store {
        tab.terrain_data_store = terrain_data_store;
    }
    if let Some(navmesh) = navmesh {
        tab.navmesh = navmesh;
    }
}

/// Put the target tab's entities in front (spawning its file the first time) and restore its
/// history and view state.
pub fn activate_tab(world: &mut World, target: usize) {
    world.resource_mut::<Scenes>().active = target;
    let has_terrain_data_store = world.contains_resource::<crate::terrain::TerrainDataStore>();
    let (content, view_state, history, tab_path, terrain_data_store, navmesh) = {
        let mut scenes = world.resource_mut::<Scenes>();
        let tab = &mut scenes.tabs[target];
        (
            std::mem::replace(&mut tab.content, TabContent::Live),
            std::mem::take(&mut tab.view_state),
            std::mem::take(&mut tab.history),
            tab.path.clone(),
            has_terrain_data_store.then(|| std::mem::take(&mut tab.terrain_data_store)),
            std::mem::take(&mut tab.navmesh),
        )
    };
    if let Some(terrain_data_store) = terrain_data_store {
        *world.resource_mut::<crate::terrain::TerrainDataStore>() = terrain_data_store;
    }
    crate::terrain::navmesh_bake::install_in_world(world, navmesh);

    let mut refusal = None;
    match content {
        TabContent::Parked => {
            if let Some(tab_world) = crate::scene_io::scene_world(world) {
                crate::scene_io::set_tab_open(world, tab_world, true);
            }
        }
        TabContent::Unloaded | TabContent::Live => {
            crate::scene_io::ensure_scene_world(world);
            if let Some(path) = tab_path.as_ref()
                && path.is_file()
            {
                let spawned = crate::scene_io::read_scene_file(world, path)
                    .map_err(|refusal| refusal.message)
                    .and_then(|file| crate::scene_io::spawn_scene_file(world, file));
                if let Err(err) = spawned {
                    error!("activate_tab: {err}");
                    refusal = Some(crate::scenes::TabRefusal::SpawnFailed(err));
                }
            }
        }
    }
    let _ = crate::hierarchy::rebuild_hierarchy(world);
    let spawned_ok = refusal.is_none();
    if spawned_ok {
        let kind = crate::scene_io::open_scene_kind(world);
        crate::scene_io::apply_scene_kind(world, kind);
    } else {
        world.remove_resource::<crate::viewport_host::PendingViewportFocus>();
    }
    world.resource_mut::<Scenes>().tabs[target].refusal = refusal;

    let history_depth = history.undo_stack.len();
    *world.resource_mut::<CommandHistory>() = history;
    apply_view_state(world, &view_state, spawned_ok);

    if let Some(mut spath) = world.get_resource_mut::<crate::scene_io::SceneFilePath>() {
        spath.path = tab_path.as_ref().map(|p| p.to_string_lossy().into_owned());
    }

    // A dirty tab keeps its unsaved terrain edits; a clean one re-reads the file.
    if let Some(path) = tab_path.as_ref() {
        let mode = if world
            .resource::<Scenes>()
            .tabs
            .get(target)
            .is_some_and(|tab| tab.dirty)
        {
            crate::scene_io::SidecarImport::FillMissing
        } else {
            crate::scene_io::SidecarImport::RefreshChanged
        };
        crate::scene_io::import_terrain_sidecars(world, &path.to_string_lossy(), mode);
        crate::terrain::navmesh_bake::import_beside_scene(world, &path.to_string_lossy());
    }

    world.resource_mut::<Scenes>().tabs[target].history_depth_at_last_check = history_depth;
}

/// Captures camera transform, edit mode, and selection.
fn capture_view_state(world: &mut World) -> ViewState {
    use crate::brush::{BrushSelection, EditMode};
    use crate::selection::Selected;
    use crate::viewport::MainViewportCamera;
    use crate::viewport_2d::Viewport2dPanelHost;

    let mut cam_q = world.query_filtered::<&Transform, With<MainViewportCamera>>();
    let camera_transform = cam_q.iter(world).next().copied().unwrap_or_default();

    // The 2D framing lives on the panel host, not on the derived camera, so it
    // needs its own pass; with several panels open the first one wins. Only a
    // framing the user chose is captured, since restoring a default would keep
    // a never-framed tab from ever being framed.
    let mut host_q = world.query::<&Viewport2dPanelHost>();
    let ui_view = host_q
        .iter(world)
        .next()
        .filter(|host| host.view_touched)
        .map(|host| host.view);

    let edit_mode = world
        .get_resource::<EditMode>()
        .copied()
        .unwrap_or_default();
    let brush_sub_selection = world
        .get_resource::<BrushSelection>()
        .cloned()
        .unwrap_or_default();

    let mut sel_q = world.query_filtered::<Entity, With<Selected>>();
    let selection: Vec<Entity> = sel_q.iter(world).collect();

    // Only a mode the user picked; one implied by the scene's kind is
    // recomputed on the next activation.
    let viewport_mode = world
        .get_resource::<crate::viewport_host::ViewportModeIntent>()
        .and_then(|intent| intent.chosen.then_some(intent.mode));

    ViewState {
        camera_transform,
        camera_projection: None,
        edit_mode,
        selection,
        brush_sub_selection,
        ui_view,
        viewport_mode,
    }
}

/// Restores camera transform, edit mode, and selection. `spawned` says whether
/// the tab's document is live in the world; a refused tab keeps its view state
/// but does not get its viewport mode applied.
fn apply_view_state(world: &mut World, view_state: &ViewState, spawned: bool) {
    use crate::brush::{BrushSelection, EditMode};
    use crate::selection::{Selected, Selection};
    use crate::viewport::MainViewportCamera;
    use crate::viewport_2d::Viewport2dPanelHost;

    // Camera transform.
    let mut cam_q = world.query_filtered::<&mut Transform, With<MainViewportCamera>>();
    if let Some(mut tf) = cam_q.iter_mut(world).next() {
        *tf = view_state.camera_transform;
    }

    // 2D viewport framing; `apply_2d_view` carries it onto the camera next
    // frame.
    let mut host_q = world.query::<&mut Viewport2dPanelHost>();
    if let Some(mut host) = host_q.iter_mut(world).next() {
        match view_state.ui_view {
            // A remembered framing outranks the activation's fit request.
            Some(view) => {
                host.set_view(view);
                host.fit_pending = false;
            }
            None => host.reset_view(),
        }
    }

    // A mode the user picked outranks the one the scene's kind implies.
    if let Some(mode) = view_state.viewport_mode
        && spawned
    {
        crate::viewport_host::set_viewport_mode(world, mode, true);
    }

    // Edit mode.
    if let Some(mut em) = world.get_resource_mut::<EditMode>() {
        *em = view_state.edit_mode;
    }

    // Brush sub-selection.
    if let Some(mut bs) = world.get_resource_mut::<BrushSelection>() {
        *bs = view_state.brush_sub_selection.clone();
    }

    let entities: Vec<Entity> = view_state
        .selection
        .iter()
        .copied()
        .filter(|&entity| world.get_entity(entity).is_ok())
        .collect();

    if let Some(mut selection) = world.get_resource_mut::<Selection>() {
        selection.entities.clone_from(&entities);
    }

    let mut prev_q = world.query_filtered::<Entity, With<Selected>>();
    let prev: Vec<Entity> = prev_q.iter(world).collect();
    for e in prev {
        if entities.contains(&e) {
            continue;
        }
        if let Ok(mut ec) = world.get_entity_mut(e) {
            ec.remove::<Selected>();
        }
    }
    for &e in &entities {
        if let Ok(mut ec) = world.get_entity_mut(e) {
            ec.insert(Selected);
        }
    }
}
