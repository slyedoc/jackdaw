//! Scrolling containers.
//!
//! A scrolling pair is `ScrollArea` beside `Scrollbar`: the wheel and the
//! thumb are the widgets', not the editor's.

use crate::util;

use bevy::camera::{NormalizedRenderTarget, RenderTarget};
use bevy::picking::{
    backend::HitData,
    events::{Pointer, PointerScroll},
    pointer::{Location, PointerId},
};
use bevy::prelude::*;
use bevy::ui_widgets::{ScrollArea, Scrollbar, ScrollbarThumb};
use bevy::window::{PrimaryWindow, WindowRef};

use jackdaw_feathers::scroll::{scroll_area, scrollbar};

fn window_target(app: &mut App) -> NormalizedRenderTarget {
    let window = app
        .world_mut()
        .query_filtered::<Entity, With<PrimaryWindow>>()
        .single(app.world())
        .expect("headless apps still have a primary window");
    RenderTarget::Window(WindowRef::Primary)
        .normalize(Some(window))
        .expect("the primary window normalizes")
}

/// The bar beside a scrolling container is the scrollbar widget, aimed
/// at that container, with the widget's own draggable thumb inside it.
#[test]
fn a_scrollbar_is_the_widget_aimed_at_its_container() {
    let mut app = util::editor_test_app();

    let container = app
        .world_mut()
        .spawn((
            Node {
                overflow: Overflow::scroll_y(),
                ..default()
            },
            scroll_area(),
        ))
        .id();
    let bar = app.world_mut().spawn(scrollbar(container)).id();
    app.update();

    let widget = app
        .world()
        .get::<Scrollbar>(bar)
        .expect("the bar is the scrollbar widget");
    assert_eq!(widget.target, container, "and it scrolls its container");
    let thumb = app
        .world()
        .get::<Children>(bar)
        .expect("a scrollbar holds a thumb")
        .iter()
        .next()
        .expect("a scrollbar holds a thumb");
    assert!(
        app.world().get::<ScrollbarThumb>(thumb).is_some(),
        "the thumb is the widget's, so it can be dragged",
    );
    assert!(
        app.world().get::<ScrollArea>(container).is_some(),
        "and the wheel over the container is the widget's too",
    );
    assert_eq!(
        app.world().get::<Node>(bar).map(|node| node.position_type),
        Some(PositionType::Relative),
        "the bar is in-flow, so a row can sit it beside the container",
    );
}

/// A wheel over the container moves it, through the scroll area rather
/// than through the editor's own handler.
#[test]
fn scrolling_a_scroll_area_moves_its_scroll_position() {
    let mut app = util::editor_test_app();

    let container = app
        .world_mut()
        .spawn((
            Node {
                width: px(100.0),
                height: px(100.0),
                overflow: Overflow::scroll_y(),
                ..default()
            },
            scroll_area(),
            children![Node {
                width: px(100.0),
                height: px(1000.0),
                ..default()
            }],
        ))
        .id();
    app.update();
    app.update();

    let target = window_target(&mut app);
    app.world_mut().trigger(PointerScroll {
        entity: container,
        pointer: Pointer::new(
            PointerId::Mouse,
            Location {
                target,
                position: Vec2::ZERO,
            },
        ),
        unit: bevy::input::mouse::MouseScrollUnit::Pixel,
        x: 0.0,
        y: -40.0,
        hit: HitData::new(Entity::PLACEHOLDER, 0.0, None, None),
        phase: bevy::input::touch::TouchPhase::Moved,
    });
    app.update();

    let position = app
        .world()
        .get::<ScrollPosition>(container)
        .expect("a scroll area carries its position");
    assert!(
        position.y > 0.0,
        "the wheel moved the scroll area, got {}",
        position.y,
    );
}
