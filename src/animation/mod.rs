//! The Animation panel: the keyframe timeline and its operators.

pub mod panel;
pub mod timeline_glue;
pub mod timeline_ops;

pub use panel::animation_panel_content;

use bevy::prelude::*;
use jackdaw_api::prelude::*;

pub(crate) fn plugin(app: &mut App) {
    app.add_plugins(timeline_glue::plugin);
}

pub(crate) fn add_to_extension(ctx: &mut ExtensionContext) {
    timeline_ops::add_to_extension(ctx);
}
