//! A tab's environment: its sky, exposure and physics settings, saved as the scene file's root.

#[path = "../util/mod.rs"]
mod util;

use std::any::TypeId;
use std::path::{Path, PathBuf};

use bevy::{prelude::*, scene::AuthoredComponents};
use bevy_aurora::{
    environment::Environment,
    sky::{EnvironmentExposure, Sky},
};
use jackdaw_api::prelude::*;

fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/definition_project")
}

fn settle(app: &mut App) {
    for _ in 0..6 {
        app.update();
    }
}

/// An editor with a project of its own and a scene of `document` open.
fn editor_on(document: &str) -> (util::TestApp, tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().expect("tempdir");
    std::fs::copy(
        fixture_dir().join("jackdaw.toml"),
        tmp.path().join("jackdaw.toml"),
    )
    .expect("the manifest copies");
    std::fs::create_dir_all(tmp.path().join("assets")).expect("an assets folder");

    let mut app = util::editor_test_app();
    app.world_mut()
        .insert_resource(jackdaw::project::ProjectRoot {
            root: tmp.path().to_path_buf(),
            config: default(),
        });
    app.world_mut()
        .resource_mut::<NextState<jackdaw::AppState>>()
        .set(jackdaw::AppState::Editor);
    app.update();
    settle(&mut app);

    let scene = tmp.path().join("assets/valley.bsn");
    std::fs::write(&scene, document).expect("the scene is written");
    jackdaw::scenes::operators::scene_open_system(app.world_mut(), &scene);
    settle(&mut app);
    (app, tmp, scene)
}

const TWO_ROOTS: &str = "Children [\n    #Rock\n    bevy_transform::components::transform::Transform\n    --\n    #Tree\n    bevy_transform::components::transform::Transform\n]\n";

fn environment(app: &App) -> Entity {
    jackdaw::scene_io::scene_environment(app.world()).expect("the tab has an environment")
}

fn authored(app: &App, entity: Entity) -> Vec<TypeId> {
    app.world()
        .get::<AuthoredComponents>(entity)
        .map(|set| set.0.iter().copied().collect())
        .unwrap_or_default()
}

fn named(app: &mut App, name: &str) -> Entity {
    app.world_mut()
        .query::<(Entity, &Name)>()
        .iter(app.world())
        .find(|(_, n)| n.as_str() == name)
        .map(|(entity, _)| entity)
        .unwrap_or_else(|| panic!("{name} spawned"))
}

#[test]
fn a_scene_without_settings_opens_under_a_fresh_environment_and_saves_as_it_was() {
    let (mut app, _tmp, scene) = editor_on(TWO_ROOTS);
    let environment = environment(&app);
    assert!(app.world().get::<Environment>(environment).is_some());
    assert!(authored(&app, environment).is_empty());
    let rock = named(&mut app, "Rock");
    assert_eq!(
        app.world().get::<ChildOf>(rock).map(ChildOf::parent),
        Some(environment),
        "the scene's roots live under its environment",
    );

    assert!(jackdaw::scene_io::save_scene(app.world_mut()));
    let text = std::fs::read_to_string(&scene).expect("saved");
    assert!(
        !text.contains("EnvironmentExposure") && !text.contains("Sky"),
        "nothing authored on the environment, nothing written for it:\n{text}",
    );
    assert!(text.contains("#Rock") && text.contains("#Tree"), "{text}");
}

#[test]
fn an_authored_setting_saves_as_the_files_root_and_reopens_on_the_environment() {
    let (mut app, _tmp, scene) = editor_on(TWO_ROOTS);
    let environment = environment(&app);
    app.world_mut()
        .entity_mut(environment)
        .insert((EnvironmentExposure { ev100: 9.5 }, Sky::Atmosphere));
    jackdaw::scene_io::author(
        app.world_mut(),
        environment,
        TypeId::of::<EnvironmentExposure>(),
    );
    jackdaw::scene_io::author(app.world_mut(), environment, TypeId::of::<Sky>());

    assert!(jackdaw::scene_io::save_scene(app.world_mut()));
    let text = std::fs::read_to_string(&scene).expect("saved");
    let (root, children) = text.split_once("Children").expect(&text);
    assert!(
        root.contains("ev100: 9.5"),
        "the root carries the exposure:\n{text}"
    );
    assert!(root.contains("Atmosphere"), "and the sky:\n{text}");
    assert!(
        !root.contains("bevy_aurora::environment::Environment"),
        "settings only: whoever spawns the file makes it an environment:\n{text}"
    );
    assert!(
        !root.contains("Name"),
        "not the editor's name for it:\n{text}"
    );
    assert!(children.contains("#Rock"), "{text}");

    let (mut reopened, _tmp2, _) = editor_on(&text);
    let environment = self::environment(&reopened);
    assert_eq!(
        reopened.world().get::<EnvironmentExposure>(environment),
        Some(&EnvironmentExposure { ev100: 9.5 }),
    );
    assert!(matches!(
        reopened.world().get::<Sky>(environment),
        Some(Sky::Atmosphere)
    ));
    let mut set = authored(&reopened, environment);
    set.sort();
    let mut want = vec![TypeId::of::<EnvironmentExposure>(), TypeId::of::<Sky>()];
    want.sort();
    assert_eq!(set, want, "what the file set is what it authored");
    let rock = named(&mut reopened, "Rock");
    assert_eq!(
        reopened.world().get::<ChildOf>(rock).map(ChildOf::parent),
        Some(environment),
    );
}

#[test]
fn a_respawned_scene_puts_back_the_settings_it_had_and_drops_the_ones_it_did_not() {
    let (mut app, _tmp, _scene) = editor_on(TWO_ROOTS);
    let environment = environment(&app);
    app.world_mut()
        .entity_mut(environment)
        .insert(EnvironmentExposure { ev100: 4.0 });
    jackdaw::scene_io::author(
        app.world_mut(),
        environment,
        TypeId::of::<EnvironmentExposure>(),
    );

    // The text an undo snapshot respawns: the environment as it was, a sky and no exposure.
    let text = "bevy_aurora::environment::Environment\nbevy_aurora::sky::Sky::Atmosphere\nChildren [\n    #Pebble\n    bevy_transform::components::transform::Transform\n]\n";
    jackdaw::scene_io::spawn_bsn_text(app.world_mut(), text, "assets/valley.bsn")
        .expect("the text spawns");
    settle(&mut app);

    assert_eq!(
        jackdaw::scene_io::scene_environment(app.world()),
        Some(environment),
        "the tab keeps its environment entity",
    );
    assert!(matches!(
        app.world().get::<Sky>(environment),
        Some(Sky::Atmosphere)
    ));
    assert_eq!(
        app.world().get::<EnvironmentExposure>(environment),
        Some(&EnvironmentExposure::default()),
        "a setting the text leaves out goes back to its default",
    );
    let mut set = authored(&app, environment);
    set.sort();
    let mut want = vec![TypeId::of::<Environment>(), TypeId::of::<Sky>()];
    want.sort();
    assert_eq!(set, want, "what the text set, and nothing it left out");
}

#[test]
fn the_environment_selects_into_the_inspector_with_its_settings() {
    let (mut app, _tmp, _scene) = editor_on(TWO_ROOTS);
    let environment = environment(&app);
    app.world_mut()
        .operator("environment.select")
        .call()
        .expect("the operator dispatched");
    settle(&mut app);
    assert_eq!(
        app.world()
            .resource::<jackdaw::selection::Selection>()
            .primary(),
        Some(environment),
    );
    let texts: Vec<String> = app
        .world_mut()
        .query::<&Text>()
        .iter(app.world())
        .map(|text| text.0.clone())
        .collect();
    for card in ["Environment", "Gravity", "SolverConfig"] {
        assert!(
            texts.iter().any(|text| text == card),
            "the inspector shows a {card} card; it shows {texts:?}",
        );
    }
}

#[test]
fn an_environment_added_from_the_menu_nests_in_the_scene_and_saves_with_it() {
    let (mut app, _tmp, scene) = editor_on(TWO_ROOTS);
    app.world_mut()
        .operator("entity.add.environment")
        .call()
        .expect("the operator dispatched");
    settle(&mut app);
    let tab = environment(&app);
    let nested = app
        .world_mut()
        .query::<(Entity, &Name)>()
        .iter(app.world())
        .find(|(entity, name)| name.as_str() == "Environment" && *entity != tab)
        .map(|(entity, _)| entity)
        .expect("a nested environment spawned");
    assert_ne!(nested, tab, "a new entity, not the tab's own environment");
    assert!(app.world().get::<Environment>(nested).is_some());
    assert!(
        app.world()
            .get::<avian3d::environment::PhysicsEnvironment>(nested)
            .is_some(),
        "with its physics environment",
    );
    assert_eq!(
        app.world().get::<ChildOf>(nested).map(ChildOf::parent),
        Some(tab),
        "inside the scene's own environment",
    );
    let bits = app
        .world()
        .resource::<bevy_aurora::environment::RenderEnvironments>();
    assert!(bits.bit(nested).is_some(), "it renders in a bit of its own");
    assert_ne!(bits.bit(nested), bits.bit(tab));

    assert!(jackdaw::scene_io::save_scene(app.world_mut()));
    let text = std::fs::read_to_string(&scene).expect("saved");
    let (_, children) = text.split_once("Children").expect(&text);
    assert!(
        children.contains("bevy_aurora::environment::Environment"),
        "saved as one of the scene's entities:\n{text}"
    );
}

#[test]
fn a_file_opened_beside_another_tab_renders_in_that_tab() {
    let tmp = tempfile::tempdir().expect("tempdir");
    std::fs::copy(
        fixture_dir().join("jackdaw.toml"),
        tmp.path().join("jackdaw.toml"),
    )
    .expect("the manifest copies");
    let assets = tmp.path().join("assets");
    std::fs::create_dir_all(&assets).expect("an assets folder");
    let mut app = util::editor_test_app_at(&assets);
    app.world_mut()
        .insert_resource(jackdaw::project::ProjectRoot {
            root: tmp.path().to_path_buf(),
            config: default(),
        });
    app.world_mut()
        .resource_mut::<NextState<jackdaw::AppState>>()
        .set(jackdaw::AppState::Editor);
    app.update();
    settle(&mut app);
    // Beside an untitled tab, as the editor opens a project with one restored.
    let _ = app
        .world_mut()
        .operator("scene.new")
        .call()
        .expect("the operator dispatched");
    settle(&mut app);
    std::fs::write(
        assets.join("rig.bsn"),
        "#rig\nbevy_transform::components::transform::Transform\nbevy_ecs::hierarchy::Children [\n    #Bone\n    bevy_transform::components::transform::Transform\n]\n",
    )
    .expect("the rig is written");
    let character = assets.join("character.bsn");
    std::fs::write(
        &character,
        "#character\nbevy_transform::components::transform::Transform\nbevy_ecs::hierarchy::Children [\n    :\"rig.bsn\"\n    bevy_transform::components::transform::Transform\n]\n",
    )
    .expect("the character is written");
    jackdaw::scenes::operators::scene_open_system(app.world_mut(), &character);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        app.update();
        let spawned = app
            .world_mut()
            .query::<&Name>()
            .iter(app.world())
            .any(|n| n.as_str() == "Bone");
        if spawned {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    settle(&mut app);

    let scenes = app.world().resource::<jackdaw::scenes::Scenes>();
    assert_eq!(scenes.tabs.len(), 2, "opened as a tab of its own");
    let tab = environment(&app);
    let bit = app
        .world()
        .resource::<bevy_aurora::environment::RenderEnvironments>()
        .bit(tab)
        .expect("the tab has a render bit");
    assert!(bit > 0, "a tab renders apart from the editor's own bit");

    // The open document's bone: the asset browser renders a thumbnail copy of the file too.
    let bone = app
        .world_mut()
        .query_filtered::<(Entity, &Name), With<jackdaw::scene_io::SceneEntity>>()
        .iter(app.world())
        .find(|(_, n)| n.as_str() == "Bone")
        .map(|(e, _)| e)
        .expect("the tab's bone");
    assert_eq!(
        app.world()
            .get::<bevy_aurora::environment::InEnvironment>(bone)
            .map(|e| e.0),
        Some(bit),
        "the nested rig's bones render in the tab's bit",
    );
    let mut cameras = app.world_mut().query_filtered::<
        Option<&bevy::camera::visibility::RenderLayers>,
        With<jackdaw::viewport::MainViewportCamera>,
    >();
    for layers in cameras.iter(app.world()) {
        assert!(
            layers.is_some_and(
                |l| l.intersects(&bevy::camera::visibility::RenderLayers::layer(bit as usize))
            ),
            "the viewport sees the active tab: {layers:?}",
        );
    }
}

#[test]
fn a_nested_environment_saves_without_its_bit() {
    let (mut app, _tmp, scene) = editor_on(TWO_ROOTS);
    let _ = app
        .world_mut()
        .operator("entity.add.environment")
        .call()
        .expect("the operator dispatched");
    settle(&mut app);
    assert!(jackdaw::scene_io::save_scene(app.world_mut()));
    let text = std::fs::read_to_string(&scene).expect("saved");
    let line = text
        .lines()
        .find(|l| l.contains("bevy_aurora::environment::Environment"))
        .unwrap_or_else(|| panic!("the nested environment is saved:\n{text}"));
    assert_eq!(
        line.trim(),
        "bevy_aurora::environment::Environment",
        "the file asks for an environment, not a bit:\n{text}"
    );

    jackdaw::scenes::operators::scene_open_system(app.world_mut(), &scene);
    settle(&mut app);
    let tab = environment(&app);
    let reopened: Vec<Option<u8>> = app
        .world_mut()
        .query::<(Entity, &Environment)>()
        .iter(app.world())
        .filter(|(entity, _)| *entity != tab)
        .map(|(_, e)| e.0)
        .collect();
    assert!(
        reopened.iter().any(Option::is_some),
        "reopened, it takes a bit again: {reopened:?}"
    );
}
