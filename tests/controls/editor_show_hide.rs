//! Showing and hiding the editor switches only its window camera's UI on and off; the cameras
//! that render into images keep the state they had.

use bevy::prelude::*;
use bevy::ui::IsDefaultUiCamera;

use crate::util;

fn default_ui_cameras(app: &mut App) -> usize {
    app.world_mut()
        .query_filtered::<(), With<IsDefaultUiCamera>>()
        .iter(app.world())
        .count()
}

fn active_image_cameras(app: &mut App) -> usize {
    app.world_mut()
        .query_filtered::<(&Camera, &bevy::camera::RenderTarget), With<Camera2d>>()
        .iter(app.world())
        .filter(|(camera, target)| camera.is_active && target.as_image().is_some())
        .count()
}

fn set_shown(app: &mut App, shown: bool) {
    app.world_mut().resource_mut::<jackdaw::EditorShown>().0 = shown;
    app.update();
    app.update();
}

#[test]
fn the_editor_has_one_ui_camera_through_a_hide_and_show() {
    let mut app = util::editor_test_app();
    app.insert_resource(jackdaw::project::ProjectRoot {
        root: tempfile::tempdir().unwrap().keep(),
        config: default(),
    });
    util::enter_editor(&mut app);
    let image_cameras = active_image_cameras(&mut app);
    assert_eq!(default_ui_cameras(&mut app), 1);

    set_shown(&mut app, false);
    assert_eq!(default_ui_cameras(&mut app), 0);
    assert_eq!(active_image_cameras(&mut app), 0, "hidden, nothing renders");

    set_shown(&mut app, true);
    assert_eq!(default_ui_cameras(&mut app), 1);
    assert_eq!(
        active_image_cameras(&mut app),
        image_cameras,
        "shown again, each image camera is back as it was"
    );
}
