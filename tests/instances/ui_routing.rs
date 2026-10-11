//! A UI scene an instance brings into a world scene is drawn in the world view; a UI scene root
//! the document authored is the 2D panel's. Both have `UiSceneRoot` and no `ChildOf`, so the
//! instance's `SceneBase` is what tells them apart.

use bevy::prelude::*;
use bevy::scene::SceneBase;
use bevy::ui::UiTargetCamera;
use jackdaw::scene_io::SceneEntity;
use jackdaw::viewport::MainViewportCamera;
use jackdaw::viewport_2d::{UiSceneParkingCamera, route_ui_roots_to_cameras};
use jackdaw_scene_types::UiSceneRoot;

fn routing_app() -> App {
    let mut app = App::new();
    app.add_plugins((MinimalPlugins, bevy::asset::AssetPlugin::default()))
        .init_asset::<Image>()
        .add_systems(Update, route_ui_roots_to_cameras);
    app
}

fn parking(app: &mut App) -> Entity {
    app.world_mut()
        .query_filtered::<Entity, With<UiSceneParkingCamera>>()
        .iter(app.world())
        .next()
        .expect("a root with nowhere to go parks")
}

fn target(app: &App, root: Entity) -> Option<Entity> {
    app.world()
        .get::<UiTargetCamera>(root)
        .map(UiTargetCamera::entity)
}

#[test]
fn the_2d_panel_router_leaves_an_instanced_ui_scene_to_the_world_view() {
    let mut app = routing_app();
    let world_camera = app.world_mut().spawn((MainViewportCamera, Camera2d)).id();
    let authored = app
        .world_mut()
        .spawn((SceneEntity, UiSceneRoot::default()))
        .id();
    let instanced = app
        .world_mut()
        .spawn((
            SceneEntity,
            UiSceneRoot::default(),
            SceneBase("hud.bsn".into()),
        ))
        .id();

    app.update();

    let parking = parking(&mut app);
    assert_eq!(
        target(&app, authored),
        Some(parking),
        "with no 2D panel open the authored UI scene parks"
    );
    assert_eq!(
        target(&app, instanced),
        Some(world_camera),
        "an instanced UI scene renders in the world view, not on the 2D stage"
    );
}

/// An unrouted root falls back to `DefaultUiCamera`, the editor's own window camera, and would
/// draw the overlay over the editor chrome.
#[test]
fn an_instanced_ui_scene_parks_when_there_is_no_world_view() {
    let mut app = routing_app();
    let instanced = app
        .world_mut()
        .spawn((
            SceneEntity,
            UiSceneRoot::default(),
            SceneBase("hud.bsn".into()),
        ))
        .id();

    app.update();

    let parking = parking(&mut app);
    assert_eq!(
        target(&app, instanced),
        Some(parking),
        "the instance is parked rather than left to DefaultUiCamera"
    );
}
