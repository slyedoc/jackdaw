//! A layered landscape (aurora_landscape) through the editor: added from the Add menu's
//! operator, layers added, toggled and reordered from the panel's operators, undone, then
//! saved and opened again with its stacks as they were. Saving also bakes each landscape's
//! evaluated pages, which a scene opened with the editor down loads instead of evaluating.

use crate::util;

use std::path::PathBuf;

use aurora_landscape::{GeneratorLayer, HeightLayer, Landscape, MaterialLayer};
use bevy::prelude::*;
use jackdaw::scenes::Scenes;
use jackdaw::selection::Selection;
use jackdaw_api::prelude::*;

#[track_caller]
fn dispatch(app: &mut App, id: &'static str, params: &[(&'static str, String)]) {
    let mut call = app.world_mut().operator(id);
    for (key, value) in params {
        call = call.param(*key, value.clone());
    }
    let result = call
        .call()
        .unwrap_or_else(|err| panic!("{id} dispatch errored: {err}"));
    assert_eq!(result, OperatorResult::Finished, "{id} did not finish");
    app.update();
}

fn the_landscape(app: &mut App) -> Entity {
    app.world_mut()
        .query_filtered::<Entity, With<Landscape>>()
        .single(app.world())
        .expect("one landscape")
}

/// The landscape's layers in child order: (name, height stack, enabled).
fn stacks(app: &mut App) -> Vec<(String, bool, bool)> {
    let landscape = the_landscape(app);
    let world = app.world();
    world
        .get::<Children>(landscape)
        .into_iter()
        .flatten()
        .filter_map(|&child| {
            let name = world.get::<Name>(child)?.as_str().to_string();
            if let Some(h) = world.get::<HeightLayer>(child) {
                Some((name, true, h.enabled))
            } else {
                world
                    .get::<MaterialLayer>(child)
                    .map(|m| (name, false, m.enabled))
            }
        })
        .collect()
}

fn layer_named(app: &mut App, name: &str) -> Entity {
    let landscape = the_landscape(app);
    let world = app.world();
    world
        .get::<Children>(landscape)
        .into_iter()
        .flatten()
        .copied()
        .find(|&c| world.get::<Name>(c).is_some_and(|n| n.as_str() == name))
        .unwrap_or_else(|| panic!("no layer named {name}"))
}

#[test]
fn a_landscape_and_its_stacks_survive_editing_undo_and_a_save() {
    let mut app = util::editor_test_app();
    dispatch(&mut app, "scene.new", &[]);
    dispatch(&mut app, "entity.add.landscape", &[]);
    let landscape = the_landscape(&mut app);
    assert!(app.world().get::<Landscape>(landscape).is_some());
    assert_eq!(
        stacks(&mut app),
        vec![("Sculpt".into(), true, true), ("Paint".into(), false, true)],
        "a new landscape starts with a sculpt and a paint layer"
    );

    app.world_mut().resource_mut::<Selection>().entities = vec![landscape];
    dispatch(
        &mut app,
        "landscape.layer.add",
        &[("kind", "generator".into())],
    );
    app.world_mut().resource_mut::<Selection>().entities = vec![landscape];
    dispatch(&mut app, "landscape.layer.add", &[("kind", "rule".into())]);

    let generator = layer_named(&mut app, "Generator");
    app.world_mut()
        .get_mut::<GeneratorLayer>(generator)
        .unwrap()
        .hills_amplitude = 77.0;
    dispatch(
        &mut app,
        "landscape.layer.toggle",
        &[("layer", generator.to_bits().to_string())],
    );
    let rule = layer_named(&mut app, "Rule");
    dispatch(
        &mut app,
        "landscape.layer.move",
        &[
            ("layer", rule.to_bits().to_string()),
            ("dir", "down".into()),
        ],
    );
    let edited = vec![
        ("Sculpt".to_string(), true, true),
        ("Rule".to_string(), false, true),
        ("Paint".to_string(), false, true),
        ("Generator".to_string(), true, false),
    ];
    assert_eq!(stacks(&mut app), edited, "toggle and reorder");

    // Undo the move, redo it.
    dispatch(&mut app, "history.undo", &[]);
    let names: Vec<String> = stacks(&mut app).into_iter().map(|s| s.0).collect();
    assert_eq!(
        names,
        ["Sculpt", "Paint", "Generator", "Rule"],
        "the move undoes"
    );
    dispatch(&mut app, "history.redo", &[]);
    assert_eq!(stacks(&mut app), edited, "and redoes");

    // Save, reopen.
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let stage = tempfile::Builder::new()
        .prefix("landscape-")
        .tempdir_in(root.join("target"))
        .unwrap();
    std::fs::create_dir_all(stage.path().join("assets")).unwrap();
    let scene_path = stage.path().join("assets").join("land.bsn");
    {
        let mut scenes = app.world_mut().resource_mut::<Scenes>();
        let active = scenes.active;
        scenes.tabs.get_mut(active).unwrap().path = Some(scene_path.clone());
    }
    dispatch(&mut app, "scene.save", &[]);
    let text = std::fs::read_to_string(&scene_path).expect("scene written");
    assert!(
        text.contains("aurora_landscape"),
        "the scene names the landscape:\n{text}"
    );

    let mut reopened = util::editor_test_app();
    dispatch(&mut reopened, "scene.new", &[]);
    jackdaw::scene_io::load_scene_from_file(reopened.world_mut(), &scene_path);
    reopened.update();
    assert_eq!(
        stacks(&mut reopened),
        edited,
        "the stacks come back as saved"
    );
    let generator = layer_named(&mut reopened, "Generator");
    assert_eq!(
        reopened
            .world()
            .get::<GeneratorLayer>(generator)
            .unwrap()
            .hills_amplitude,
        77.0,
        "a layer's parameters come back"
    );
    let landscape = the_landscape(&mut reopened);
    assert_eq!(
        reopened
            .world()
            .get::<Landscape>(landscape)
            .unwrap()
            .pages_max,
        IVec2::splat(2),
        "the landscape's extent comes back"
    );
}

/// Tick until the landscape's pages are evaluated: its kernels compile (slangc) on worker
/// threads, so this waits on the clock, not a frame count.
fn settle(app: &mut App) {
    for _ in 0..2000 {
        std::thread::sleep(std::time::Duration::from_millis(10));
        app.update();
        let landscape = the_landscape(app);
        if app
            .world()
            .get::<aurora_landscape::LandscapePages>(landscape)
            .is_some_and(|p| p.is_complete())
        {
            return;
        }
    }
    panic!("the landscape's pages never evaluated");
}

#[test]
fn sculpted_pages_are_saved_beside_the_scene_and_come_back() {
    let mut app = util::editor_test_app();
    dispatch(&mut app, "scene.new", &[]);
    dispatch(&mut app, "entity.add.landscape", &[]);
    settle(&mut app);
    let sculpt = layer_named(&mut app, "Sculpt");
    app.world_mut()
        .resource_mut::<aurora_landscape::SculptDabs>()
        .0
        .push(aurora_landscape::SculptDab {
            layer: sculpt,
            center: Vec2::new(10.0, 10.0),
            radius: 6.0,
            strength: 3.0,
            kind: aurora_landscape::BrushKind::Raise,
            target: 0.0,
            material: 0,
        });
    let landscape = the_landscape(&mut app);
    let mut raised = 0.0;
    for _ in 0..500 {
        std::thread::sleep(std::time::Duration::from_millis(10));
        app.update();
        raised = app
            .world()
            .get::<aurora_landscape::LandscapePages>(landscape)
            .unwrap()
            .height_at(Vec2::new(10.0, 10.0));
        if raised > 2.0 {
            break;
        }
    }
    assert!(raised > 2.0, "the dab raised the ground ({raised})");

    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let stage = tempfile::Builder::new()
        .prefix("landscape-pages-")
        .tempdir_in(root.join("target"))
        .unwrap();
    let assets = stage.path().join("assets");
    std::fs::create_dir_all(&assets).unwrap();
    app.insert_resource(jackdaw::project::ProjectRoot {
        root: stage.path().to_path_buf(),
        config: default(),
    });
    let scene_path = assets.join("hill.bsn");
    {
        let mut scenes = app.world_mut().resource_mut::<Scenes>();
        let active = scenes.active;
        scenes.tabs.get_mut(active).unwrap().path = Some(scene_path.clone());
    }
    dispatch(&mut app, "scene.save", &[]);
    let sculpt_files: Vec<_> = std::fs::read_dir(&assets)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "sculpt"))
        .collect();
    assert_eq!(
        sculpt_files.len(),
        1,
        "one .sculpt beside the scene: {sculpt_files:?}"
    );
    let text = std::fs::read_to_string(&scene_path).unwrap();
    assert!(
        text.contains(".sculpt"),
        "the scene names its sculpt file:\n{text}"
    );
    assert!(text.contains(".pages"), "the scene names its bake:\n{text}");

    let mut reopened = util::editor_test_app_at(&assets);
    reopened.insert_resource(jackdaw::project::ProjectRoot {
        root: stage.path().to_path_buf(),
        config: default(),
    });
    dispatch(&mut reopened, "scene.new", &[]);
    jackdaw::scene_io::load_scene_from_file(reopened.world_mut(), &scene_path);
    settle(&mut reopened);
    let landscape = the_landscape(&mut reopened);
    let mut height = 0.0;
    for _ in 0..500 {
        std::thread::sleep(std::time::Duration::from_millis(10));
        reopened.update();
        height = reopened
            .world()
            .get::<aurora_landscape::LandscapePages>(landscape)
            .unwrap()
            .height_at(Vec2::new(10.0, 10.0));
        if (height - raised).abs() < 1.0e-3 {
            break;
        }
    }
    assert!(
        (height - raised).abs() < 1.0e-3,
        "the sculpt came back from its file ({height} vs {raised})"
    );
}

/// Tick until the landscape's pages hold its bake.
fn settle_baked(app: &mut App) {
    for _ in 0..2000 {
        std::thread::sleep(std::time::Duration::from_millis(10));
        app.update();
        let landscape = the_landscape(app);
        if app
            .world()
            .get::<aurora_landscape::LandscapePages>(landscape)
            .is_some_and(|p| p.is_baked())
        {
            return;
        }
    }
    panic!("the landscape's bake never loaded");
}

#[test]
fn a_saved_landscape_loads_its_bake_while_the_editor_is_down() {
    let mut app = util::editor_test_app();
    dispatch(&mut app, "scene.new", &[]);
    dispatch(&mut app, "entity.add.landscape", &[]);
    settle(&mut app);
    let landscape = the_landscape(&mut app);
    app.world_mut().resource_mut::<Selection>().entities = vec![landscape];
    dispatch(
        &mut app,
        "landscape.layer.add",
        &[("kind", "generator".into())],
    );
    let probe = Vec2::new(40.0, -25.0);
    let mut evaluated = 0.0;
    for _ in 0..1000 {
        std::thread::sleep(std::time::Duration::from_millis(10));
        app.update();
        let pages = app
            .world()
            .get::<aurora_landscape::LandscapePages>(landscape)
            .unwrap();
        evaluated = pages.height_at(probe);
        if pages.is_complete() && evaluated != 0.0 {
            break;
        }
    }
    assert!(evaluated != 0.0, "the generator raised the ground");

    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let stage = tempfile::Builder::new()
        .prefix("landscape-bake-")
        .tempdir_in(root.join("target"))
        .unwrap();
    let assets = stage.path().join("assets");
    std::fs::create_dir_all(&assets).unwrap();
    app.insert_resource(jackdaw::project::ProjectRoot {
        root: stage.path().to_path_buf(),
        config: default(),
    });
    let scene_path = assets.join("baked.bsn");
    {
        let mut scenes = app.world_mut().resource_mut::<Scenes>();
        let active = scenes.active;
        scenes.tabs.get_mut(active).unwrap().path = Some(scene_path.clone());
    }
    dispatch(&mut app, "scene.save", &[]);
    let bakes = std::fs::read_dir(&assets)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "pages"))
        .count();
    assert_eq!(bakes, 1, "one .pages beside the scene");

    let mut reopened = util::editor_test_app_at(&assets);
    reopened.insert_resource(jackdaw::project::ProjectRoot {
        root: stage.path().to_path_buf(),
        config: default(),
    });
    dispatch(&mut reopened, "scene.new", &[]);
    jackdaw::scene_io::load_scene_from_file(reopened.world_mut(), &scene_path);
    // Test apps run with the editor off (`AppState::Off`).
    settle_baked(&mut reopened);
    let landscape = the_landscape(&mut reopened);
    let height = reopened
        .world()
        .get::<aurora_landscape::LandscapePages>(landscape)
        .unwrap()
        .height_at(probe);
    assert_eq!(height, evaluated, "the bake holds the evaluated heights");

    // The editor up: the stacks take over.
    reopened
        .world_mut()
        .resource_mut::<NextState<jackdaw::AppState>>()
        .set(jackdaw::AppState::Editor);
    reopened.insert_resource(jackdaw::EditorShown(true));
    reopened.update();
    reopened.update();
    assert!(
        !reopened
            .world()
            .get::<aurora_landscape::LandscapePages>(landscape)
            .unwrap()
            .is_baked(),
        "live editing evaluates the stacks"
    );
}
