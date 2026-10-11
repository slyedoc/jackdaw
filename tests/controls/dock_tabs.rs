//! Clicking a dock tab, with the mouse, brings its panel to the front of its area.

use crate::util;
use crate::util::OperatorResultExt as _;

use bevy::{
    prelude::*,
    ui::UiGlobalTransform,
    window::{PrimaryWindow, WindowResolution},
};
use jackdaw::test_input::SyntheticInput;
use jackdaw_panels::{
    DockTab,
    tree::{DockNode, DockTree, TabId},
};

fn settle(app: &mut App) {
    for _ in 0..8 {
        app.update();
    }
}

fn click(app: &mut App, at: Vec2) {
    jackdaw::boot_ops::run_op_clause(
        app.world_mut(),
        &format!("input.pointer x={} y={} action=click", at.x, at.y),
    )
    .expect("the clause dispatches")
    .assert_finished();
    for _ in 0..300 {
        app.update();
        if app.world().resource::<SyntheticInput>().is_idle() {
            break;
        }
    }
    settle(app);
}

fn centre_of(app: &App, entity: Entity) -> Vec2 {
    let transform = app.world().get::<UiGlobalTransform>(entity).unwrap();
    let computed = app.world().get::<ComputedNode>(entity).unwrap();
    transform.translation * computed.inverse_scale_factor() * app.world().resource::<UiScale>().0
}

fn is_active(app: &App, tab: TabId) -> bool {
    let tree = app.world().resource::<DockTree>();
    tree.find_leaf_for_tab(tab)
        .and_then(|leaf| tree.get(leaf))
        .is_some_and(|node| matches!(node, DockNode::Leaf(leaf) if leaf.active == Some(tab)))
}

#[test]
fn a_click_on_a_background_tab_brings_it_forward() {
    let mut app = util::editor_test_app();
    {
        let mut windows = app
            .world_mut()
            .query_filtered::<&mut Window, With<PrimaryWindow>>();
        windows.single_mut(app.world_mut()).unwrap().resolution = WindowResolution::new(1600, 1000);
    }
    let project = tempfile::tempdir().unwrap().keep();
    app.insert_resource(jackdaw::project::ProjectRoot {
        root: project,
        config: default(),
    });
    util::enter_editor(&mut app);
    settle(&mut app);

    let tabs: Vec<(Entity, TabId)> = app
        .world_mut()
        .query::<(Entity, &DockTab)>()
        .iter(app.world())
        .map(|(entity, tab)| (entity, tab.tab_id))
        .collect();
    let (entity, tab) = tabs
        .into_iter()
        .find(|(_, tab)| !is_active(&app, *tab))
        .expect("some area has a tab behind another");
    let at = centre_of(&app, entity);
    click(&mut app, at);
    assert!(is_active(&app, tab), "the clicked tab is in front");
}
