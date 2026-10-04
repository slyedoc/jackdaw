//! The dockable "Animation" panel: the keyframe timeline.
//!
//! Skeletal animation (graphs, state machines, clips) has its own windows, from the animation
//! graph extension.

use bevy::prelude::*;
use jackdaw_feathers::tokens;

pub fn animation_panel_content() -> impl Bundle {
    (
        Node {
            width: percent(100),
            height: percent(100),
            flex_direction: FlexDirection::Column,
            ..default()
        },
        BackgroundColor(tokens::PANEL_BG),
        children![jackdaw_animation::timeline_panel()],
    )
}
