//! The scene data jackdaw itself defines, on the inspector. Every component an
//! entity carries gets a card, as Unity's and Godot's inspectors show them; the
//! namespace cull that keeps jackdaw's bookkeeping out of the list must not
//! take the scene's wind with it.

use crate::util;

use bevy::prelude::*;
use jackdaw::selection::Selection;
use jackdaw_scene_types::Wind;

/// A selected, document-tracked entity carrying `components`.
fn app_with(components: impl Bundle) -> util::TestApp {
    let mut app = util::editor_test_app();
    app.world_mut()
        .spawn(jackdaw::layout::inspector_components_content(default()));
    let entity = app
        .world_mut()
        .spawn((Name::new("meadow"), Transform::default(), components))
        .id();
    jackdaw::scene_io::adopt_entity(app.world_mut(), entity);
    let world = app.world_mut();
    world.resource_scope(|world, mut selection: Mut<Selection>| {
        let mut commands = world.commands();
        selection.select_single(&mut commands, entity);
    });
    world.flush();
    for _ in 0..4 {
        app.update();
    }
    app
}

#[test]
fn the_wind_gets_a_card() {
    let mut app = app_with(Wind::default());

    let cards = jackdaw::inspector::component_cards_showing(app.world_mut());
    assert!(
        cards
            .iter()
            .any(|card| card == "jackdaw_scene_types::types::Wind"),
        "the wind has no card among {cards:?}"
    );
    assert!(
        !cards.iter().any(|card| card.ends_with("SceneNodeId")),
        "the node id stays bookkeeping: {cards:?}"
    );
}
