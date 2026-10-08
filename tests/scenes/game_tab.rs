//! An editor over a game: the running world is the first tab, never saved or closed, and each
//! document tab sees only its own world.

use bevy::{camera::visibility::RenderLayers, prelude::*};
use jackdaw::scenes::{HostGame, Scenes, TabKind};

use crate::util;

fn host_app() -> util::TestApp {
    let mut app = util::editor_test_app();
    let project = tempfile::tempdir().unwrap().keep();
    app.insert_resource(HostGame::default());
    app.insert_resource(jackdaw::project::ProjectRoot {
        root: project,
        config: default(),
    });
    util::enter_editor(&mut app);
    app
}

fn viewport_layers(app: &mut App) -> RenderLayers {
    app.update();
    let mut cameras = app
        .world_mut()
        .query_filtered::<&RenderLayers, With<jackdaw::viewport::MainViewportCamera>>();
    cameras
        .iter(app.world())
        .next()
        .cloned()
        .expect("the viewport camera has layers")
}

#[test]
fn the_game_tab_comes_first_and_in_front() {
    let app = host_app();
    let scenes = app.world().resource::<Scenes>();
    assert_eq!(scenes.tabs[0].kind, TabKind::Game);
    assert_eq!(scenes.active, 0);
}

#[test]
fn a_document_tab_sees_its_own_world_not_the_game() {
    let mut app = host_app();
    assert_eq!(viewport_layers(&mut app), RenderLayers::layer(0));
    jackdaw::scenes::operators::scene_new_system(app.world_mut());
    let layers = viewport_layers(&mut app);
    assert!(!layers.intersects(&RenderLayers::layer(0)), "{layers:?}");
    jackdaw::scenes::operators::scene_switch_system(app.world_mut(), 0);
    assert_eq!(viewport_layers(&mut app), RenderLayers::layer(0));
}

#[test]
fn the_game_tab_is_never_closed_or_saved() {
    let mut app = host_app();
    jackdaw::scenes::operators::scene_new_system(app.world_mut());
    jackdaw::scenes::operators::scene_close_system(app.world_mut(), 0);
    assert_eq!(app.world().resource::<Scenes>().tabs.len(), 2);
    jackdaw::scenes::operators::scene_switch_system(app.world_mut(), 0);
    assert_eq!(
        jackdaw::scene_io::save_scene_with_outcome(app.world_mut()),
        jackdaw::scene_io::SaveOutcome::Failed
    );
}
