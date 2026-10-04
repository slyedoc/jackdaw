//! The animation graph editor: bevy_animation_graph's graphs, state machines, clips and
//! ragdolls, edited in two windows (the graph canvas and a live preview of the rig).

mod editor;

pub use editor::AnimGraphRoot;

use bevy::prelude::*;
use jackdaw_api::prelude::*;
use jackdaw_feathers::icons::Icon;

pub(crate) const GRAPH_WINDOW_ID: &str = "jackdaw.animation_graph";
pub(crate) const PREVIEW_WINDOW_ID: &str = "jackdaw.animation_preview";

pub(crate) fn plugin(app: &mut App) {
    editor::plugin(app);
    app.add_systems(Update, follow_the_project);
}

/// The editor browses, and saves under, the open project's assets.
fn follow_the_project(
    project: Option<Res<crate::project::ProjectRoot>>,
    mut root: ResMut<AnimGraphRoot>,
) {
    let assets = project
        .as_deref()
        .map(crate::project::ProjectRoot::assets_dir);
    if root.0 != assets {
        root.0 = assets;
    }
}

/// The animation graph editor's windows.
#[derive(Default)]
pub struct AnimationGraphExtension;

impl JackdawExtension for AnimationGraphExtension {
    fn id(&self) -> String {
        GRAPH_WINDOW_ID.to_string()
    }

    fn label(&self) -> String {
        "Animation Graph".to_string()
    }

    fn kind(&self) -> ExtensionKind {
        ExtensionKind::Builtin
    }

    fn register(&self, ctx: &mut ExtensionContext) {
        ctx.register_window(
            WindowDescriptor::new(GRAPH_WINDOW_ID)
                .with_name("Animation Graph")
                .with_icon(Icon::Workflow.unicode())
                .with_default_area(DefaultArea::BottomDock)
                .with_priority(2)
                .with_build(|window| {
                    window.spawn(editor::graph_window_content());
                }),
        );
        ctx.register_window(
            WindowDescriptor::new(PREVIEW_WINDOW_ID)
                .with_name("Animation Preview")
                .with_icon(Icon::PersonStanding.unicode())
                .with_default_area(DefaultArea::BottomDock)
                .with_priority(3)
                .with_build(|window| {
                    window.spawn(editor::preview_window_content());
                }),
        );
    }
}
