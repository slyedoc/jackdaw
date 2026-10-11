use bevy::prelude::Name;
use jackdaw::scenes::{SceneTab, Scenes, TabContent};

#[test]
fn scenes_default_is_empty() {
    let scenes = Scenes::default();
    assert!(scenes.tabs.is_empty());
    assert_eq!(scenes.active, 0);
}

#[test]
fn push_tab_appends_to_end() {
    let mut scenes = Scenes::default();
    let idx = scenes.push_tab(SceneTab::new_untitled(1));
    assert_eq!(idx, 0);
    assert_eq!(scenes.tabs.len(), 1);
}

#[test]
fn untitled_tab_has_correct_display_name() {
    let tab = SceneTab::new_untitled(3);
    assert_eq!(tab.display_name, "untitled-3");
    assert!(tab.path.is_none());
    assert!(!tab.dirty);
}

#[test]
fn a_fresh_tab_starts_unloaded() {
    let tab = jackdaw::scenes::SceneTab::new_untitled(1);
    assert!(
        matches!(tab.content, TabContent::Unloaded),
        "fresh tab has nothing spawned yet"
    );
    assert!(
        tab.environment.is_none(),
        "and no environment until activated"
    );
}

#[test]
fn view_state_round_trips_camera_transform() {
    use bevy::math::Vec3;
    use bevy::prelude::Transform;
    use jackdaw::scenes::ViewState;
    let vs = ViewState {
        camera_transform: Transform::from_xyz(1.0, 2.0, 3.0),
        ..ViewState::default()
    };
    assert_eq!(vs.camera_transform.translation, Vec3::new(1.0, 2.0, 3.0));
}

#[test]
fn view_state_default_has_empty_selection_and_no_sub_selection() {
    use jackdaw::scenes::ViewState;
    let vs = ViewState::default();
    assert!(vs.selection.is_empty());
    assert!(vs.brush_sub_selection.active_brush.is_none());
    assert!(vs.brush_sub_selection.brushes.is_empty());
}

#[test]
fn document_capture_includes_brushes() {
    use jackdaw_scene_types::Brush;

    let mut app = make_app_with_n_tabs(1);
    jackdaw::scenes::swap::activate_tab(app.world_mut(), 0);
    let brush = app
        .world_mut()
        .spawn((Brush::cuboid(1.0, 1.0, 1.0), Name::new("test_brush")))
        .id();
    jackdaw::scene_io::adopt_entity(app.world_mut(), brush);

    let text = jackdaw::scene_io::emit_bsn_scene_with_inline_assets(
        app.world_mut(),
        std::path::Path::new("."),
    );
    assert!(
        text.contains("test_brush"),
        "expected the brush entity in the captured document:\n{text}"
    );
}

/// A tab switch parks the outgoing tab's entities rather than despawning them, and
/// switching back brings the same entities forward.
#[test]
fn swap_parks_a_tabs_entities_and_brings_them_back() {
    use jackdaw::scene_io::SceneEntity;
    use jackdaw_scene_types::Brush;

    let mut app = make_app_with_n_tabs(2);
    jackdaw::scenes::swap::activate_tab(app.world_mut(), 0);
    let brush = app
        .world_mut()
        .spawn((Brush::cuboid(1.0, 1.0, 1.0), Name::new("a")))
        .id();
    jackdaw::scene_io::adopt_entity(app.world_mut(), brush);

    jackdaw::scenes::swap::swap_active_tab(app.world_mut(), 1);
    assert!(
        app.world().get::<SceneEntity>(brush).is_none(),
        "a parked tab's entities are not the open scene"
    );
    let scenes = app.world().resource::<jackdaw::scenes::Scenes>();
    assert_eq!(scenes.active, 1);
    assert!(matches!(scenes.tabs[0].content, TabContent::Parked));

    jackdaw::scenes::swap::swap_active_tab(app.world_mut(), 0);
    assert!(
        app.world().get::<SceneEntity>(brush).is_some(),
        "tab A's entity is in front again"
    );
    assert!(app.world().get::<Brush>(brush).is_some());
}

/// A tab switch must not lose a sculpted heightmap: the sidecar path lives on the
/// component and the heights travel with the tab's `TerrainDataStore`.
#[test]
fn swap_preserves_a_sculpted_terrain() {
    use bevy::prelude::*;
    use jackdaw::terrain::TerrainDataStore;
    use jackdaw_scene_types::Terrain;
    use jackdaw_terrain::{RegionTerrainData, TerrainData};

    const DATA_PATH: &str = "zone.terrain-0.jdterrain";

    let mut app = make_app_with_n_tabs(2);
    app.init_resource::<TerrainDataStore>();
    jackdaw::scenes::swap::activate_tab(app.world_mut(), 0);

    let sculpted: Vec<f32> = (0..16).map(|i| i as f32 * 0.5).collect();
    app.world_mut().resource_mut::<TerrainDataStore>().insert(
        DATA_PATH.to_string(),
        RegionTerrainData::from_legacy_v1(&TerrainData {
            resolution: 4,
            heights: sculpted.clone(),
            channels: vec![],
        })
        .expect("a power-of-two resolution migrates"),
    );
    let terrain_entity = app
        .world_mut()
        .spawn((
            Terrain {
                resolution: 4,
                data_path: DATA_PATH.to_string(),
                ..default()
            },
            Name::new("ground"),
        ))
        .id();
    jackdaw::scene_io::adopt_entity(app.world_mut(), terrain_entity);

    jackdaw::scenes::swap::swap_active_tab(app.world_mut(), 1);
    assert!(
        app.world()
            .resource::<TerrainDataStore>()
            .get(DATA_PATH)
            .is_none(),
        "the other tab does not see this tab's terrain"
    );
    jackdaw::scenes::swap::swap_active_tab(app.world_mut(), 0);

    let terrain = app
        .world()
        .get::<Terrain>(terrain_entity)
        .expect("the terrain is still there");
    assert_eq!(terrain.data_path, DATA_PATH);
    assert!(
        terrain.heights.is_empty(),
        "heights never enter the component"
    );
    let store = app.world().resource::<TerrainDataStore>();
    assert_eq!(store.heights(DATA_PATH), sculpted.as_slice());
}

#[test]
fn scene_tabs_keep_same_named_terrain_sidecars_isolated() {
    use jackdaw::terrain::TerrainDataStore;
    use jackdaw_terrain::{TerrainData, sidecar};

    const DATA_PATH: &str = "zone.terrain-0.jdterrain";

    let mut app = make_app_with_n_tabs(0);
    app.init_resource::<TerrainDataStore>();

    let scene_text = format!(
        "bevy_ecs::hierarchy::Children [\n    #ground\n    jackdaw_scene_types::types::Terrain {{ resolution: 4, data_path: \"{DATA_PATH}\" }}\n]\n"
    );

    let first_dir = tempfile::tempdir().unwrap();
    let second_dir = tempfile::tempdir().unwrap();
    let first_scene = first_dir.path().join("zone.bsn");
    let second_scene = second_dir.path().join("zone.bsn");
    std::fs::write(&first_scene, &scene_text).unwrap();
    std::fs::write(&second_scene, &scene_text).unwrap();

    let first_heights = vec![1.0; 16];
    let second_heights = vec![2.0; 16];
    for (dir, heights) in [
        (first_dir.path(), first_heights.as_slice()),
        (second_dir.path(), second_heights.as_slice()),
    ] {
        let bytes = sidecar::encode(&TerrainData {
            resolution: 4,
            heights: heights.to_vec(),
            channels: vec![],
        })
        .expect("terrain data encodes");
        std::fs::write(dir.join(DATA_PATH), bytes).unwrap();
    }

    jackdaw::scenes::operators::scene_open_system(app.world_mut(), &first_scene);
    assert_eq!(
        app.world()
            .resource::<TerrainDataStore>()
            .heights(DATA_PATH),
        first_heights,
    );

    jackdaw::scenes::operators::scene_open_system(app.world_mut(), &second_scene);
    assert_eq!(
        app.world()
            .resource::<TerrainDataStore>()
            .heights(DATA_PATH),
        second_heights,
        "the second tab must hydrate its own same-named sidecar",
    );

    jackdaw::scenes::swap::swap_active_tab(app.world_mut(), 0);
    assert_eq!(
        app.world()
            .resource::<TerrainDataStore>()
            .heights(DATA_PATH),
        first_heights,
        "switching back must restore the first tab's terrain data",
    );
    jackdaw::scenes::swap::swap_active_tab(app.world_mut(), 1);
    assert_eq!(
        app.world()
            .resource::<TerrainDataStore>()
            .heights(DATA_PATH),
        second_heights,
        "switching again must restore the second tab's terrain data",
    );
}

#[test]
fn swap_preserves_camera_transform_per_tab() {
    use bevy::prelude::*;
    use bevy::winit::WinitPlugin;

    let mut app = App::new();
    app.add_plugins(bevy::app::PluginGroup::build(DefaultPlugins).disable::<WinitPlugin>());
    app.add_plugins(jackdaw_scene_types::SceneTypesPlugin::default());
    app.init_resource::<jackdaw::scenes::Scenes>();
    app.init_resource::<jackdaw::commands::CommandHistory>();
    app.init_resource::<jackdaw::selection::Selection>();

    let cam = app
        .world_mut()
        .spawn((
            Camera3d::default(),
            Transform::from_xyz(1.0, 2.0, 3.0),
            jackdaw::viewport::MainViewportCamera,
        ))
        .id();

    {
        let mut scenes = app.world_mut().resource_mut::<jackdaw::scenes::Scenes>();
        scenes.tabs.push(jackdaw::scenes::SceneTab::new_untitled(1));
        scenes.tabs.push(jackdaw::scenes::SceneTab::new_untitled(2));
        scenes.active = 0;
    }

    jackdaw::scenes::swap::swap_active_tab(app.world_mut(), 1);
    if let Some(mut tf) = app.world_mut().get_mut::<Transform>(cam) {
        *tf = Transform::from_xyz(10.0, 20.0, 30.0);
    }
    jackdaw::scenes::swap::swap_active_tab(app.world_mut(), 0);

    let tf = app.world().get::<Transform>(cam).unwrap();
    assert_eq!(tf.translation, Vec3::new(1.0, 2.0, 3.0));
}

#[test]
fn scene_new_appends_an_untitled_tab() {
    use bevy::prelude::*;
    use bevy::winit::WinitPlugin;

    let mut app = App::new();
    app.add_plugins(bevy::app::PluginGroup::build(DefaultPlugins).disable::<WinitPlugin>());
    app.add_plugins(jackdaw_scene_types::SceneTypesPlugin::default());
    app.init_resource::<jackdaw::scenes::Scenes>();
    app.init_resource::<jackdaw::commands::CommandHistory>();
    app.init_resource::<jackdaw::selection::Selection>();
    app.init_resource::<jackdaw::scenes::operators::UntitledCounter>();

    // The first run appends and activates tab 0; the second swaps to tab 1.
    app.world_mut()
        .run_system_cached(jackdaw::scenes::operators::scene_new_system)
        .unwrap();
    let scenes = app.world().resource::<jackdaw::scenes::Scenes>();
    assert_eq!(scenes.tabs.len(), 1);
    assert_eq!(scenes.active, 0);
    assert!(scenes.tabs[0].path.is_none());
    assert!(scenes.tabs[0].display_name.starts_with("untitled-"));
    let has_light = {
        let mut names = app.world_mut().query::<&Name>();
        names
            .iter(app.world())
            .any(|name| name.as_str() == "Directional Light")
    };
    assert!(
        has_light,
        "new untitled scene should spawn the default directional light"
    );

    app.world_mut()
        .run_system_cached(jackdaw::scenes::operators::scene_new_system)
        .unwrap();
    let scenes = app.world().resource::<jackdaw::scenes::Scenes>();
    assert_eq!(scenes.tabs.len(), 2);
    assert_eq!(scenes.active, 1);
    assert_ne!(scenes.tabs[0].display_name, scenes.tabs[1].display_name);
}

fn make_app_with_n_tabs(n: usize) -> bevy::app::App {
    use bevy::prelude::*;
    use bevy::winit::WinitPlugin;

    let mut app = App::new();
    app.add_plugins(bevy::app::PluginGroup::build(DefaultPlugins).disable::<WinitPlugin>());
    app.add_plugins(jackdaw_scene_types::SceneTypesPlugin::default());
    app.init_resource::<jackdaw::scenes::Scenes>();
    app.init_resource::<jackdaw::commands::CommandHistory>();
    app.init_resource::<jackdaw::selection::Selection>();
    app.init_resource::<jackdaw::scenes::operators::UntitledCounter>();
    app.init_resource::<jackdaw::scene_io::SceneFilePath>();
    app.init_resource::<jackdaw::scene_io::SceneDirtyState>();
    {
        let mut scenes = app.world_mut().resource_mut::<jackdaw::scenes::Scenes>();
        for i in 0..n {
            scenes
                .tabs
                .push(jackdaw::scenes::SceneTab::new_untitled((i + 1) as u32));
        }
        scenes.active = 0;
    }
    app
}

#[test]
fn scene_switch_changes_active_index() {
    let mut app = make_app_with_n_tabs(2);
    jackdaw::scenes::operators::scene_switch_system(app.world_mut(), 1);
    assert_eq!(app.world().resource::<jackdaw::scenes::Scenes>().active, 1);
}

#[test]
fn scene_save_all_writes_each_path_bound_tab() {
    let mut app = make_app_with_n_tabs(2);
    let tmp = tempfile::tempdir().expect("temp directory");
    let path_a = tmp.path().join("a.bsn");
    let path_b = tmp.path().join("b.bsn");
    {
        let mut scenes = app.world_mut().resource_mut::<jackdaw::scenes::Scenes>();
        scenes.tabs[0].path = Some(path_a.clone());
        scenes.tabs[1].path = Some(path_b.clone());
    }

    assert!(jackdaw::scenes::operators::scene_save_all_system(
        app.world_mut()
    ));

    assert!(path_a.exists(), "tab 0 should have been saved");
    assert!(path_b.exists(), "tab 1 should have been saved");
}

#[test]
fn scene_save_all_reports_failure_and_keeps_a_tab_dirty() {
    let mut app = make_app_with_n_tabs(1);
    let tmp = tempfile::tempdir().expect("temp directory");
    let blocked_parent = tmp.path().join("not-a-directory");
    std::fs::write(&blocked_parent, b"file blocks directory creation")
        .expect("create blocking file");
    let scene_path = blocked_parent.join("zone.bsn");

    {
        let mut scenes = app.world_mut().resource_mut::<jackdaw::scenes::Scenes>();
        scenes.tabs[0].path = Some(scene_path);
        scenes.tabs[0].dirty = true;
    }
    let data_path = "zone.terrain-0.jdterrain";
    let mut store = jackdaw::terrain::TerrainDataStore::default();
    store.insert(
        data_path.to_string(),
        jackdaw_terrain::RegionTerrainData::from_legacy_v1(&jackdaw_terrain::TerrainData {
            resolution: 2,
            heights: vec![0.0; 4],
            channels: vec![],
        })
        .expect("a power-of-two resolution migrates"),
    );
    app.world_mut().insert_resource(store);
    app.world_mut().spawn(jackdaw_scene_types::Terrain {
        resolution: 2,
        data_path: data_path.to_string(),
        ..Default::default()
    });

    let saved = jackdaw::scenes::operators::scene_save_all_system(app.world_mut());

    assert!(
        !saved,
        "Save All must report an authoritative write failure"
    );
    assert!(app.world().resource::<jackdaw::scenes::Scenes>().tabs[0].dirty);
}

/// A refused tab's document never spawned, but its path is still set, and
/// `scene.save_all` reaches the write without passing the single-save entry
/// point: otherwise Ctrl+Shift+S replaces the user's scene with an empty world.
#[test]
fn scene_save_all_leaves_a_refused_tabs_file_untouched() {
    let mut app = make_app_with_n_tabs(1);
    let tmp = tempfile::tempdir().expect("temp directory");
    let scene_path = tmp.path().join("zone.bsn");
    let original = "// the user's scene, which the editor could not spawn\n";
    std::fs::write(&scene_path, original).expect("write the scene the tab names");

    {
        let mut scenes = app.world_mut().resource_mut::<jackdaw::scenes::Scenes>();
        scenes.tabs[0].path = Some(scene_path.clone());
        scenes.tabs[0].dirty = true;
        scenes.tabs[0].refusal = Some(jackdaw::scenes::TabRefusal::Rejected(
            "names a retired component".to_string(),
        ));
    }

    let saved = jackdaw::scenes::operators::scene_save_all_system(app.world_mut());

    assert!(
        !saved,
        "Save All must report failure when a tab could not be written"
    );
    assert_eq!(
        std::fs::read_to_string(&scene_path).expect("the file is still there"),
        original,
        "the refused tab's file was overwritten from a world that never held it",
    );
    assert!(
        app.world().resource::<jackdaw::scenes::Scenes>().tabs[0].dirty,
        "a tab that was not written stays dirty",
    );
}

#[test]
fn scene_close_blocks_closing_last_tab() {
    let mut app = make_app_with_n_tabs(1);
    jackdaw::scenes::operators::scene_close_system(app.world_mut(), 0);
    let scenes = app.world().resource::<jackdaw::scenes::Scenes>();
    assert_eq!(scenes.tabs.len(), 1);
    assert_eq!(scenes.active, 0);
}

#[test]
fn scene_close_drops_inactive_tab_and_shifts_active_index() {
    let mut app = make_app_with_n_tabs(2);
    {
        let mut scenes = app.world_mut().resource_mut::<jackdaw::scenes::Scenes>();
        scenes.active = 1;
    }
    jackdaw::scenes::operators::scene_close_system(app.world_mut(), 0);
    let scenes = app.world().resource::<jackdaw::scenes::Scenes>();
    assert_eq!(scenes.tabs.len(), 1);
    assert_eq!(scenes.active, 0);
}

#[test]
fn scene_close_drops_active_tab_and_picks_neighbor() {
    let mut app = make_app_with_n_tabs(2);
    {
        let mut scenes = app.world_mut().resource_mut::<jackdaw::scenes::Scenes>();
        scenes.active = 1;
    }
    jackdaw::scenes::operators::scene_close_system(app.world_mut(), 1);
    let scenes = app.world().resource::<jackdaw::scenes::Scenes>();
    assert_eq!(scenes.tabs.len(), 1);
    assert_eq!(scenes.active, 0);
}

#[test]
fn pushing_to_history_marks_active_tab_dirty() {
    use bevy::prelude::*;

    struct NoOpCommand;
    impl jackdaw::commands::EditorCommand for NoOpCommand {
        fn execute(&mut self, _world: &mut World) {}
        fn undo(&mut self, _world: &mut World) {}
        fn description(&self) -> &str {
            "noop"
        }
    }

    let mut app = make_app_with_n_tabs(1);
    assert!(!app.world().resource::<jackdaw::scenes::Scenes>().tabs[0].dirty);

    app.world_mut()
        .resource_mut::<jackdaw::commands::CommandHistory>()
        .push_executed(Box::new(NoOpCommand));

    app.world_mut()
        .run_system_cached(jackdaw::scenes::mark_active_dirty_on_history_growth)
        .unwrap();

    assert!(app.world().resource::<jackdaw::scenes::Scenes>().tabs[0].dirty);
}

#[test]
fn closing_dirty_tab_defers_via_pending_close_resource() {
    let mut app = make_app_with_n_tabs(2);
    // Also register PendingTabClose so the test can query it.
    app.init_resource::<jackdaw::scenes::confirm_dialog::PendingTabClose>();

    {
        let mut scenes = app.world_mut().resource_mut::<jackdaw::scenes::Scenes>();
        scenes.tabs[0].dirty = true;
        scenes.active = 1;
    }

    assert!(
        app.world()
            .resource::<jackdaw::scenes::confirm_dialog::PendingTabClose>()
            .tab_index
            .is_none()
    );

    jackdaw::scenes::operators::scene_close_system(app.world_mut(), 0);

    assert_eq!(
        app.world().resource::<jackdaw::scenes::Scenes>().tabs.len(),
        2
    );
    assert_eq!(
        app.world()
            .resource::<jackdaw::scenes::confirm_dialog::PendingTabClose>()
            .tab_index,
        Some(0)
    );
}

#[test]
fn window_close_with_dirty_tabs_does_not_exit() {
    let mut app = make_app_with_n_tabs(2);
    {
        let mut scenes = app.world_mut().resource_mut::<jackdaw::scenes::Scenes>();
        scenes.tabs[0].dirty = true;
    }
    app.world_mut()
        .init_resource::<jackdaw::scenes::confirm_dialog::PendingQuit>();

    app.add_systems(
        bevy::prelude::Update,
        jackdaw::scenes::intercept_window_close,
    );
    app.add_message::<bevy::window::WindowCloseRequested>();
    app.add_message::<bevy::app::AppExit>();

    app.world_mut()
        .resource_mut::<bevy::ecs::message::Messages<bevy::window::WindowCloseRequested>>()
        .write(bevy::window::WindowCloseRequested {
            window: bevy::prelude::Entity::PLACEHOLDER,
        });
    app.update();

    let exits: Vec<_> = app
        .world_mut()
        .resource_mut::<bevy::ecs::message::Messages<bevy::app::AppExit>>()
        .drain()
        .collect();
    assert!(exits.is_empty(), "should not have emitted AppExit");

    assert!(
        app.world()
            .resource::<jackdaw::scenes::confirm_dialog::PendingQuit>()
            .active
    );
}

#[test]
fn tab_swap_preserves_entity_ordering_and_components() {
    let mut app = make_app_with_n_tabs(2);
    jackdaw::scenes::swap::activate_tab(app.world_mut(), 0);
    let alpha = app.world_mut().spawn(Name::new("alpha")).id();
    let beta = app.world_mut().spawn(Name::new("beta")).id();
    jackdaw::scene_io::adopt_entity(app.world_mut(), alpha);
    jackdaw::scene_io::adopt_entity(app.world_mut(), beta);

    jackdaw::scenes::swap::swap_active_tab(app.world_mut(), 1);
    jackdaw::scenes::swap::swap_active_tab(app.world_mut(), 0);

    assert_eq!(
        jackdaw::scene_io::scene_roots(app.world_mut()),
        vec![alpha, beta],
        "both roots survive the round trip, in order"
    );
    assert_eq!(
        app.world().get::<Name>(beta).map(Name::as_str),
        Some("beta")
    );
}

#[test]
fn window_close_with_no_dirty_tabs_exits_cleanly() {
    let mut app = make_app_with_n_tabs(1);
    app.world_mut()
        .init_resource::<jackdaw::scenes::confirm_dialog::PendingQuit>();

    app.add_systems(
        bevy::prelude::Update,
        jackdaw::scenes::intercept_window_close,
    );
    app.add_message::<bevy::window::WindowCloseRequested>();
    app.add_message::<bevy::app::AppExit>();

    app.world_mut()
        .resource_mut::<bevy::ecs::message::Messages<bevy::window::WindowCloseRequested>>()
        .write(bevy::window::WindowCloseRequested {
            window: bevy::prelude::Entity::PLACEHOLDER,
        });
    app.update();

    let exits: Vec<_> = app
        .world_mut()
        .resource_mut::<bevy::ecs::message::Messages<bevy::app::AppExit>>()
        .drain()
        .collect();
    assert_eq!(exits.len(), 1);
}

#[test]
fn each_tab_has_its_own_undo_stack() {
    use bevy::prelude::*;

    struct CounterCommand;
    impl jackdaw::commands::EditorCommand for CounterCommand {
        fn execute(&mut self, _world: &mut World) {}
        fn undo(&mut self, _world: &mut World) {}
        fn description(&self) -> &str {
            "counter"
        }
    }

    let mut app = make_app_with_n_tabs(2);

    app.world_mut()
        .resource_mut::<jackdaw::commands::CommandHistory>()
        .push_executed(Box::new(CounterCommand));
    app.world_mut()
        .resource_mut::<jackdaw::commands::CommandHistory>()
        .push_executed(Box::new(CounterCommand));
    assert_eq!(
        app.world()
            .resource::<jackdaw::commands::CommandHistory>()
            .undo_stack
            .len(),
        2,
        "tab A has 2 entries"
    );

    jackdaw::scenes::swap::swap_active_tab(app.world_mut(), 1);
    assert_eq!(
        app.world()
            .resource::<jackdaw::commands::CommandHistory>()
            .undo_stack
            .len(),
        0,
        "tab B has its own empty undo stack"
    );

    app.world_mut()
        .resource_mut::<jackdaw::commands::CommandHistory>()
        .push_executed(Box::new(CounterCommand));
    assert_eq!(
        app.world()
            .resource::<jackdaw::commands::CommandHistory>()
            .undo_stack
            .len(),
        1,
        "tab B has 1 entry"
    );

    jackdaw::scenes::swap::swap_active_tab(app.world_mut(), 0);
    assert_eq!(
        app.world()
            .resource::<jackdaw::commands::CommandHistory>()
            .undo_stack
            .len(),
        2,
        "tab A's 2-entry stack is restored after swap-back"
    );

    jackdaw::scenes::swap::swap_active_tab(app.world_mut(), 1);
    assert_eq!(
        app.world()
            .resource::<jackdaw::commands::CommandHistory>()
            .undo_stack
            .len(),
        1,
        "tab B's stack persists across the round-trip"
    );
}

/// A tab switch into a UI scene has to bring the viewport forward the way
/// `finish_load_scene` does, or the scene activates behind whichever panel was
/// in front while the editor insists it is open.
#[test]
fn activating_a_ui_scene_tab_brings_the_viewport_forward() {
    let mut app = make_app_with_n_tabs(2);
    let leaf = dock_tree_with_a_viewport(&mut app);
    assert_eq!(
        active_window(&app, leaf).as_deref(),
        Some("jackdaw.outliner"),
        "the viewport starts behind another tab",
    );

    let _tmp = set_tab_file(&mut app, 1, UI_SCENE_BSN);
    jackdaw::scenes::swap::swap_active_tab(app.world_mut(), 1);

    assert_eq!(
        active_window(&app, leaf).as_deref(),
        Some(jackdaw::viewport::VIEWPORT_WINDOW_ID),
        "activating a UI-scene tab must front the panel that can show it",
    );
    assert_eq!(
        viewport_mode(&app),
        Some(jackdaw::viewport_host::ViewportMode::TwoD),
        "and ask it for the canvas the scene is drawn on",
    );
}

/// The other half of the same rule: an ordinary scene must not yank the
/// viewport forward over whatever the user was working in.
#[test]
fn activating_an_ordinary_scene_tab_leaves_the_front_panel_alone() {
    let mut app = make_app_with_n_tabs(2);
    let leaf = dock_tree_with_a_viewport(&mut app);

    let _tmp = set_tab_file(&mut app, 1, WORLD_BSN);
    jackdaw::scenes::swap::swap_active_tab(app.world_mut(), 1);

    assert_eq!(
        active_window(&app, leaf).as_deref(),
        Some("jackdaw.outliner"),
        "a scene with no UI root must leave the dock as the user left it",
    );
}

/// `finish_load_scene` returns before focusing when a UI document fails to
/// spawn. `activate_tab` cannot return, since the tab bookkeeping still has to
/// run, so it gates the focus explicitly.
#[test]
fn a_ui_scene_tab_that_fails_to_spawn_does_not_steal_the_viewport() {
    let mut app = make_app_with_n_tabs(2);
    let leaf = dock_tree_with_a_viewport(&mut app);
    app.world_mut()
        .insert_resource(jackdaw::viewport_host::ViewportModeIntent {
            mode: jackdaw::viewport_host::ViewportMode::ThreeD,
            chosen: false,
        });

    // A UI root beside a type this build does not know, so the spawn fails.
    let _tmp = set_tab_file(
        &mut app,
        1,
        "bevy_ecs::hierarchy::Children [\n    #Overlay\n    jackdaw_scene_types::UiSceneRoot\n    no_such_crate::Unknown\n]\n",
    );
    // And the tab carries an override of its own, which the restore would
    // apply after the activation had already declined to.
    app.world_mut().resource_mut::<Scenes>().tabs[1]
        .view_state
        .viewport_mode = Some(jackdaw::viewport_host::ViewportMode::TwoD);

    jackdaw::scenes::swap::swap_active_tab(app.world_mut(), 1);

    assert!(
        app.world().resource::<Scenes>().tabs[1].is_refused(),
        "the fixture must fail to spawn, or it proves nothing",
    );
    assert_eq!(
        active_window(&app, leaf).as_deref(),
        Some("jackdaw.outliner"),
        "a scene that never spawned must not front an empty stage",
    );
    assert_eq!(
        viewport_mode(&app),
        Some(jackdaw::viewport_host::ViewportMode::ThreeD),
        "nor put the viewport in a mode read from a document that is not there",
    );
    assert_eq!(
        *app.world()
            .resource::<jackdaw::viewport_host::ViewportModeIntent>(),
        jackdaw::viewport_host::ViewportModeIntent {
            mode: jackdaw::viewport_host::ViewportMode::ThreeD,
            chosen: false,
        },
        "and the refused tab's stored mode is not the choice the panels follow",
    );
}

/// A file naming a type this build does not know spawns nothing at all, not the
/// half that still resolves.
#[test]
fn activating_a_tab_whose_file_cannot_spawn_spawns_nothing() {
    let mut app = make_app_with_n_tabs(2);
    let _tmp = set_tab_file(
        &mut app,
        1,
        "bevy_ecs::hierarchy::Children [\n    #Overlay\n    jackdaw_ui::UiCanvas\n    --\n    #World\n    bevy_transform::components::transform::Transform\n]\n",
    );

    jackdaw::scenes::swap::swap_active_tab(app.world_mut(), 1);

    let spawned = spawned_names(&mut app);
    assert!(
        !spawned
            .iter()
            .any(|name| name == "Overlay" || name == "World"),
        "a refused document spawns nothing at all, not the half that still parses: {spawned:?}"
    );
}

fn spawned_names(app: &mut bevy::app::App) -> Vec<String> {
    use bevy::prelude::Name;
    let mut names = app.world_mut().query::<&Name>();
    names
        .iter(app.world())
        .map(|name| name.as_str().to_string())
        .collect()
}

/// The other half: an ordinary tab still spawns.
#[test]
fn activating_an_ordinary_tab_still_spawns_it() {
    let mut app = make_app_with_n_tabs(2);
    let _tmp = set_tab_file(&mut app, 1, WORLD_BSN);

    jackdaw::scenes::swap::swap_active_tab(app.world_mut(), 1);

    assert!(
        spawned_names(&mut app).iter().any(|name| name == "World"),
        "an ordinary document spawns"
    );
}

const WORLD_BSN: &str = "bevy_ecs::hierarchy::Children [\n    #World\n    bevy_transform::components::transform::Transform\n]\n";

const UI_SCENE_BSN: &str = "bevy_ecs::hierarchy::Children [\n    #Overlay\n    bevy_ui::ui_node::Node\n    jackdaw_scene_types::UiSceneRoot\n]\n";

/// Point `tab` at a fresh file holding `text`, which its next activation spawns.
fn set_tab_file(app: &mut bevy::app::App, tab: usize, text: &str) -> tempfile::TempDir {
    let tmp = tempfile::tempdir().expect("a temp dir");
    let path = tmp.path().join("tab.bsn");
    std::fs::write(&path, text).expect("the fixture lands on disk");
    app.world_mut().resource_mut::<Scenes>().tabs[tab].path = Some(path);
    tmp
}

/// What the viewport panels of the active tab are being asked to show, or
/// `None` before anything has asked them for anything.
fn viewport_mode(app: &bevy::app::App) -> Option<jackdaw::viewport_host::ViewportMode> {
    app.world()
        .get_resource::<jackdaw::viewport_host::ViewportModeIntent>()
        .map(|intent| intent.mode)
}

/// A dock holding the viewport behind another tab, so a focus change is
/// visible as a change rather than as the starting state.
fn dock_tree_with_a_viewport(app: &mut bevy::app::App) -> jackdaw_panels::tree::NodeId {
    use jackdaw_panels::{
        area::DockAreaStyle,
        tree::{DockLeaf, DockTree},
    };

    app.init_resource::<DockTree>();
    let mut tree = app.world_mut().resource_mut::<DockTree>();
    tree.set_root_leaf(
        DockLeaf::new("center", DockAreaStyle::default()).with_windows(vec![
            "jackdaw.outliner".to_string(),
            jackdaw::viewport::VIEWPORT_WINDOW_ID.to_string(),
        ]),
    )
}

fn active_window(app: &bevy::app::App, leaf: jackdaw_panels::tree::NodeId) -> Option<String> {
    let tree = app.world().resource::<jackdaw_panels::tree::DockTree>();
    let leaf = tree.get(leaf)?.as_leaf()?;
    let active = leaf.active?;
    leaf.tabs()
        .find_map(|(window, tab)| (tab == active).then(|| window.to_string()))
}

/// The editor seeds a default `Sun` on entering the editor, after the project's
/// persisted tabs have opened. A document written without a light must not pick
/// one up, or the next save writes an entity the author never made.
#[test]
fn an_opened_document_with_no_light_does_not_gain_a_sun() {
    let mut app = make_app_with_n_tabs(0);

    let dir = std::env::temp_dir().join("jackdaw_no_sun_on_open");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("unlit.bsn");
    std::fs::write(
        &path,
        "// jackdaw 0.19.0 | bevy 0.19\n#Prop\nbevy_transform::components::transform::Transform\n",
    )
    .unwrap();
    jackdaw::scenes::operators::scene_open_system(app.world_mut(), &path);

    // What `OnEnter(AppState::Editor)` does in the binary.
    jackdaw::scene_io::spawn_default_lighting(app.world_mut());

    let text = jackdaw::scene_io::emit_bsn_scene_with_inline_assets(
        app.world_mut(),
        std::path::Path::new("."),
    );
    assert!(
        !text.contains("DirectionalLight"),
        "a saved document must not carry a light the author never wrote:\n{text}"
    );

    let _ = std::fs::remove_file(&path);
}

/// The template case the seed exists for: nothing is open, so a new
/// scene still gets its `Sun`.
#[test]
fn a_scene_with_nothing_open_still_gets_its_default_sun() {
    let mut app = make_app_with_n_tabs(0);
    jackdaw::scenes::operators::scene_new_system(app.world_mut());

    jackdaw::scene_io::spawn_default_lighting(app.world_mut());

    let text = jackdaw::scene_io::emit_bsn_scene_with_inline_assets(
        app.world_mut(),
        std::path::Path::new("."),
    );
    assert!(
        text.contains("DirectionalLight"),
        "a new scene keeps its default lighting:\n{text}"
    );
}

// ---------------------------------------------------------------------------
// A tab that would not spawn is not a tab a save may write over
// ---------------------------------------------------------------------------

/// The document that every activation path refuses: a type this build does not know.
const RETIRED_TAB_BSN: &str =
    "bevy_ecs::hierarchy::Children [\n    #Overlay\n    jackdaw_ui::UiCanvas\n]\n";

/// A two-tab app whose second tab names a real file the activation will refuse.
/// Returns the file and what it held before the swap.
fn refused_tab_over_a_file() -> (
    bevy::app::App,
    std::path::PathBuf,
    String,
    tempfile::TempDir,
) {
    let tmp = tempfile::tempdir().expect("a temp dir");
    let scene = tmp.path().join("overlay.bsn");
    std::fs::write(&scene, RETIRED_TAB_BSN).expect("the fixture lands on disk");
    let before = std::fs::read_to_string(&scene).expect("read it back");

    let mut app = make_app_with_n_tabs(2);
    {
        let mut scenes = app.world_mut().resource_mut::<jackdaw::scenes::Scenes>();
        scenes.tabs[1].path = Some(scene.clone());
        scenes.tabs[1].display_name = "overlay".to_string();
    }

    jackdaw::scenes::swap::swap_active_tab(app.world_mut(), 1);
    (app, scene, before, tmp)
}

#[test]
fn a_refused_activation_marks_the_tab_rather_than_leaving_a_live_save_path() {
    let (app, _scene, _before, _tmp) = refused_tab_over_a_file();

    let scenes = app.world().resource::<jackdaw::scenes::Scenes>();
    assert_eq!(scenes.active, 1, "the refused tab is the one in front");
    assert!(
        scenes.tabs[1].is_refused(),
        "a tab the editor would not spawn has to say so, or every write path \
         below it believes the empty world is the file's contents",
    );
}

#[test]
fn saving_a_refused_tab_does_not_touch_the_file_it_names() {
    let (mut app, scene, before, _tmp) = refused_tab_over_a_file();

    assert!(
        !jackdaw::scene_io::save_scene(app.world_mut()),
        "the save has to refuse, not report success over an empty world",
    );
    assert_eq!(
        jackdaw::scene_io::save_scene_with_outcome(app.world_mut()),
        jackdaw::scene_io::SaveOutcome::Failed,
        "and say it failed rather than that it opened a dialog",
    );

    let after = std::fs::read_to_string(&scene).expect("the file is still there");
    assert_eq!(after, before, "the user's file is exactly as they left it",);
}

#[test]
fn leaving_a_refused_tab_does_not_park_the_empty_world_as_its_contents() {
    let (mut app, scene, before, _tmp) = refused_tab_over_a_file();

    jackdaw::scenes::swap::swap_active_tab(app.world_mut(), 0);

    let scenes = app.world().resource::<jackdaw::scenes::Scenes>();
    assert!(
        matches!(scenes.tabs[1].content, TabContent::Unloaded),
        "the refused tab goes back to its file, not to the empty world it showed",
    );
    assert_eq!(
        std::fs::read_to_string(&scene).expect("the file is still there"),
        before,
    );
}

#[test]
fn an_activation_that_works_takes_the_refusal_back_off() {
    let (mut app, scene, _before, _tmp) = refused_tab_over_a_file();

    std::fs::write(
        &scene,
        "bevy_ecs::hierarchy::Children [\n    #Overlay\n    bevy_transform::components::transform::Transform\n]\n",
    )
    .expect("the corrected fixture lands on disk");
    jackdaw::scenes::swap::swap_active_tab(app.world_mut(), 0);
    jackdaw::scenes::swap::swap_active_tab(app.world_mut(), 1);

    assert!(
        !app.world().resource::<jackdaw::scenes::Scenes>().tabs[1].is_refused(),
        "a tab refused once is not refused forever",
    );
}

// ---------------------------------------------------------------------------
// A drag does not outlive the entities it grabbed
// ---------------------------------------------------------------------------

#[test]
fn switching_tabs_ends_every_drag_that_was_holding_the_scene() {
    use bevy::prelude::*;

    let mut app = make_app_with_n_tabs(2);
    app.init_resource::<jackdaw::modal_transform::ViewportDragState>();
    app.init_resource::<jackdaw::gizmos::GizmoDragState>();

    let dragged = app
        .world_mut()
        .spawn((Name::new("Crate"), Transform::default()))
        .id();
    let camera = app.world_mut().spawn(Name::new("Camera")).id();
    let viewport = app.world_mut().spawn(Name::new("Viewport")).id();

    {
        let mut drag = app
            .world_mut()
            .resource_mut::<jackdaw::modal_transform::ViewportDragState>();
        drag.active = Some(jackdaw::modal_transform::ActiveDrag {
            entity: dragged,
            start_transform: Transform::default(),
            start_viewport_cursor: Vec2::ZERO,
            camera,
            viewport,
        });
    }
    {
        let mut gizmo = app
            .world_mut()
            .resource_mut::<jackdaw::gizmos::GizmoDragState>();
        gizmo.active = true;
        gizmo.targets.push(jackdaw::gizmos::GizmoTarget {
            entity: dragged,
            start_transform: Transform::default(),
        });
        gizmo.camera = Some(camera);
    }

    // A tab switch parks every authored entity, so the ids both drags are
    // holding now name a tab that is not in front.
    jackdaw::scenes::swap::swap_active_tab(app.world_mut(), 1);

    let drag = app
        .world()
        .resource::<jackdaw::modal_transform::ViewportDragState>();
    assert!(
        drag.active.is_none() && drag.pending.is_none(),
        "a viewport drag cannot keep writing transforms onto a parked tab",
    );
    let gizmo = app.world().resource::<jackdaw::gizmos::GizmoDragState>();
    assert!(
        !gizmo.active && gizmo.targets.is_empty() && gizmo.camera.is_none(),
        "and neither can a gizmo drag: it held {} target(s)",
        gizmo.targets.len(),
    );
}
