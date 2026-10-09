//! Headless load of a `.bsn` scene through bevy's loader with `JackdawPlugin`'s types registered.

mod support;

use bevy::prelude::*;

const SCENE: &str = r##"
Children [
    #Anchor
    bevy_transform::components::transform::Transform {
        translation: glam::Vec3 { x: 4.0, y: 5.0, z: 6.0 },
    }
    --
    #Follower
    bevy_transform::components::transform::Transform {
        translation: glam::Vec3 { x: 9.0, y: 0.0, z: 0.0 },
    }
]
"##;

#[test]
fn headless_bsn_scene_load() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = support::game(dir.path());
    let root = support::spawn_text(&mut app, dir.path(), "scene.bsn", SCENE);

    let anchor = support::named(app.world_mut(), "Anchor");
    assert_eq!(
        app.world().get::<ChildOf>(anchor).map(ChildOf::parent),
        Some(root)
    );
    let translation = app.world().get::<Transform>(anchor).unwrap().translation;
    assert!((translation - Vec3::new(4.0, 5.0, 6.0)).length() < 1e-5);
    support::named(app.world_mut(), "Follower");
}
