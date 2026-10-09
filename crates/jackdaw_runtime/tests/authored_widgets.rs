//! A game runtime renders the widgets a scene was authored with.
//!
//! Two things have to hold outside the editor: the widget markers survive the
//! load (they are built through `ReflectDefault`, which `bevy_ui_widgets` and
//! `bevy_feathers` do not register), and the value observers are attached, so
//! a checkbox the player clicks answers by checking itself.

mod support;

use bevy::prelude::*;
use bevy::ui::Checked;
use bevy::ui_widgets::{Checkbox, CheckboxPlugin, ToggleChecked};

/// A checkbox beside a themed caption, the shape the editor's UI palette
/// writes: a widget marker plus the feathers styling components the
/// always-save list carries through a round trip.
const SCENE: &str = r#"
Children [
    #Checkbox
    bevy_ui::ui_node::Node
    bevy_ui_widgets::checkbox::Checkbox
    --
    #Caption
    bevy_ui::ui_node::Node
    bevy_feathers::theme::ThemedText
    --
    #Plain
    bevy_ui::ui_node::Node
]
"#;

#[test]
fn a_checkbox_loaded_by_the_game_runtime_keeps_its_widget_and_toggles() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = support::game(dir.path());
    app.add_plugins(CheckboxPlugin);
    support::spawn_text(&mut app, dir.path(), "widgets.bsn", SCENE);

    let checkbox = support::named(app.world_mut(), "Checkbox");
    assert!(
        app.world().get::<Checkbox>(checkbox).is_some(),
        "the widget marker survives the load"
    );

    // `bevy_feathers` needs `bevy_render`, so the theme markers only register
    // in a rendering build; a headless game loads the widget without them.
    // `Plain` is the same node without the theme marker, so comparing
    // component counts stands in for naming a type that may not be compiled
    // in at all.
    let caption = support::named(app.world_mut(), "Caption");
    let plain = support::named(app.world_mut(), "Plain");
    let carried = |app: &App, entity| {
        app.world()
            .inspect_entity(entity)
            .expect("the entity is live")
            .count()
    };

    #[cfg(feature = "render")]
    {
        assert!(
            app.world()
                .get::<bevy::feathers::theme::ThemedText>(caption)
                .is_some(),
            "the authored theme marker survives the load"
        );
        assert!(
            carried(&app, caption) > carried(&app, plain),
            "the marker is what the caption carries over the plain node",
        );
    }
    #[cfg(not(feature = "render"))]
    assert_eq!(
        carried(&app, caption),
        carried(&app, plain),
        "a headless game drops the theme marker rather than loading it",
    );

    assert!(
        app.world().get::<Checked>(checkbox).is_none(),
        "a fresh checkbox loads unchecked"
    );

    app.world_mut().trigger(ToggleChecked { entity: checkbox });
    app.update();
    assert!(
        app.world().get::<Checked>(checkbox).is_some(),
        "clicking the checkbox checks it: the self-update observer is attached"
    );

    app.world_mut().trigger(ToggleChecked { entity: checkbox });
    app.update();
    assert!(
        app.world().get::<Checked>(checkbox).is_none(),
        "and clicking again clears it"
    );
}
