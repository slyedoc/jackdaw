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
fn editor_on(document: &str) -> (App, tempfile::TempDir, PathBuf) {
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
        root.contains("bevy_aurora::environment::Environment"),
        "and names itself an environment:\n{text}"
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
    let mut want = vec![
        TypeId::of::<Environment>(),
        TypeId::of::<EnvironmentExposure>(),
        TypeId::of::<Sky>(),
    ];
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
