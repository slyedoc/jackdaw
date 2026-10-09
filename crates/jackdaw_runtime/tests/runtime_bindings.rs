//! A game runtime evaluates the bindings its scenes were authored with: `JackdawPlugin` brings
//! the whole bind plugin, so a scene loaded through bevy's loader arrives already wired.

mod support;

use bevy::prelude::*;

#[derive(Component, Reflect, Default)]
#[reflect(Component, Default)]
struct Bar {
    fill: f32,
}

#[derive(Resource, Reflect, Default)]
#[reflect(Resource)]
struct Score {
    points: f32,
}

/// The source is a resource so the document needs no cross-entity reference.
const SCENE: &str = r#"
Children [
    #Bar
    runtime_bindings::Bar { fill: 0.0 }
    jackdaw_bind::types::Bindings([
        jackdaw_bind::types::Binding::Field { read: [jackdaw_bind::types::BindPath { raw: "Res(runtime_bindings::Score).points" }], via: None, write: jackdaw_bind::types::BindPath { raw: "runtime_bindings::Bar.fill" }, as_percent: false },
    ])
]
"#;

#[test]
fn a_scene_loaded_by_the_game_runtime_evaluates_its_bindings() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = support::game(dir.path());
    app.register_type::<Bar>();
    app.register_type::<Score>();
    app.init_resource::<Score>();
    support::spawn_text(&mut app, dir.path(), "scene.bsn", SCENE);

    let bar = support::named(app.world_mut(), "Bar");
    assert!(
        app.world().get::<Bar>(bar).is_some(),
        "the authored widget component loaded"
    );

    app.world_mut().resource_mut::<Score>().points = 7.5;
    app.update();
    assert_eq!(
        app.world().get::<Bar>(bar).map(|b| b.fill),
        Some(7.5),
        "the target follows its source"
    );
}
