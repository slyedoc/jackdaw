//! Instances are placement only: their insides select the instance, Edit Source opens the base
//! with the picked entity selected, and saving the base updates the instances of it.

#[path = "../util/mod.rs"]
mod util;

mod ui_routing;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use bevy::prelude::*;
use bevy::scene::SceneBase;

fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/definition_project")
}

fn settle(app: &mut App) {
    for _ in 0..6 {
        app.update();
    }
}

/// Update until `done`, up to a few seconds of real time (asset loads are asynchronous).
fn until(app: &mut App, what: &str, mut done: impl FnMut(&mut App) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        app.update();
        if done(app) {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("timed out waiting for {what}");
}

fn named(app: &mut App, name: &str) -> Option<Entity> {
    app.world_mut()
        .query_filtered::<(Entity, &Name), With<jackdaw::scene_io::SceneEntity>>()
        .iter(app.world())
        .find(|(_, n)| n.as_str() == name)
        .map(|(e, _)| e)
}

const RIG: &str = "#rig\nbevy_transform::components::transform::Transform\nbevy_ecs::hierarchy::Children [\n    #Bone\n    bevy_transform::components::transform::Transform\n]\n";
const LEVEL: &str = "bevy_ecs::hierarchy::Children [\n    :\"rig.bsn\"\n    bevy_transform::components::transform::Transform\n]\n";

#[test]
fn an_instance_is_edited_through_its_source() {
    let tmp = tempfile::tempdir().expect("tempdir");
    std::fs::copy(
        fixture_dir().join("jackdaw.toml"),
        tmp.path().join("jackdaw.toml"),
    )
    .expect("the manifest copies");
    let assets = tmp.path().join("assets");
    std::fs::create_dir_all(&assets).expect("an assets folder");
    std::fs::write(assets.join("rig.bsn"), RIG).expect("rig");
    std::fs::write(assets.join("level.bsn"), LEVEL).expect("level");

    let mut app = util::editor_test_app_at(&assets);
    app.world_mut()
        .insert_resource(jackdaw::project::ProjectRoot {
            root: tmp.path().to_path_buf(),
            config: default(),
        });
    app.world_mut()
        .resource_mut::<NextState<jackdaw::AppState>>()
        .set(jackdaw::AppState::Editor);
    settle(&mut app);
    jackdaw::scenes::operators::scene_open_system(app.world_mut(), &assets.join("level.bsn"));
    until(&mut app, "the rig's bone", |app| {
        named(app, "Bone").is_some()
    });

    // Selecting inside the instance selects the instance.
    let bone = named(&mut app, "Bone").unwrap();
    let instance = app
        .world_mut()
        .query_filtered::<Entity, (With<SceneBase>, With<jackdaw::scene_io::SceneEntity>)>()
        .iter(app.world())
        .next()
        .expect("the placed rig is an instance");
    jackdaw::selection::select_only(app.world_mut(), bone);
    settle(&mut app);
    assert_eq!(
        app.world()
            .resource::<jackdaw::selection::Selection>()
            .entities,
        vec![instance],
        "a pick inside an instance selects the instance",
    );

    // Edit Source: the base in front, the bone selected.
    jackdaw::instances::edit_source(app.world_mut(), bone);
    until(&mut app, "the bone selected in rig.bsn", |app| {
        let primary = app
            .world()
            .resource::<jackdaw::selection::Selection>()
            .primary();
        primary.is_some_and(|e| {
            app.world()
                .get::<Name>(e)
                .is_some_and(|n| n.as_str() == "Bone")
                && app.world().get::<SceneBase>(e).is_none()
        })
    });
    let source_bone = app
        .world()
        .resource::<jackdaw::selection::Selection>()
        .primary()
        .unwrap();

    // Edit and save the source; back in the level, the instance follows.
    app.world_mut()
        .entity_mut(source_bone)
        .insert(Transform::from_xyz(0.0, 3.0, 0.0));
    jackdaw::scene_io::author(
        app.world_mut(),
        source_bone,
        std::any::TypeId::of::<Transform>(),
    );
    assert!(jackdaw::scene_io::save_scene(app.world_mut()));
    let saved = std::fs::read_to_string(assets.join("rig.bsn")).expect("saved");
    assert!(saved.contains("3.0"), "the source saved the edit:\n{saved}");

    jackdaw::scenes::operators::scene_open_system(app.world_mut(), &assets.join("level.bsn"));
    until(&mut app, "the instance to pick up the saved base", |app| {
        named(app, "Bone").is_some_and(|bone| {
            app.world()
                .get::<Transform>(bone)
                .is_some_and(|t| (t.translation.y - 3.0).abs() < 1e-4)
        })
    });
}
