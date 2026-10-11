//! The layered landscape (`aurora_landscape`) in the editor: the Add menu entry, the Landscape
//! panel (height, material and scatter stacks, brush tools), the sculpt / paint brush, and the
//! brush layers' pages saved beside the scene.
//!
//! The landscape and its layers are ordinary scene entities: their components save through
//! the scene, and the inspector edits a selected layer's parameters. What lives outside the
//! scene is the pages a brush writes: kept per layer in [`LandscapePageStore`] (by
//! `SceneNodeId`, so a snapshot undo that re-mints entities finds them again) and written to
//! `.sculpt` / `.paint` / `.erosion` files when the scene saves, beside each landscape's
//! evaluated pages (`.pages`), which a loaded scene uses instead of evaluating its stacks.
//! While the editor is up the stacks evaluate live.

mod brush;
mod ops;
mod panel;
mod save;

use aurora_landscape::{
    BrushKind, Landscape, LandscapeLive, LandscapePlugin, LandscapeTile, collider::PageCollider,
    tiles::LandscapeRoot,
};
use avian3d::prelude::DebugRender;
use bevy::prelude::*;
use jackdaw_api::prelude::*;

use crate::EditorEntity;

pub(crate) use ops::EntityAddLandscapeOp;
pub(crate) use panel::landscape_panel_content;
pub(crate) use save::export_landscape_pages;

pub(crate) struct LandscapeEditorPlugin;

impl Plugin for LandscapeEditorPlugin {
    fn build(&self, app: &mut App) {
        if !app.is_plugin_added::<LandscapePlugin>() {
            app.add_plugins(LandscapePlugin);
        }
        app.init_resource::<LandscapeBrush>()
            .init_resource::<save::LandscapePageStore>()
            .add_systems(Update, (hide_runtime_entities, sync_live))
            .add_systems(
                Update,
                (
                    save::restore_from_store,
                    panel::update_landscape_panel,
                    brush::update_brush_hit,
                    brush::paint_strokes,
                    brush::draw_brush_ring,
                )
                    .chain()
                    .run_if(crate::editor_active),
            );
        panel::plugin(app);
    }
}

pub(crate) fn add_to_extension(ctx: &mut ExtensionContext) {
    ops::add_to_extension(ctx);
}

/// The armed brush tool and its settings; `None` leaves the viewport to selection.
#[derive(Resource)]
pub(crate) struct LandscapeBrush {
    pub tool: Option<BrushKind>,
    pub radius: f32,
    pub strength: f32,
    /// Palette id the paint tool lays down.
    pub material: u8,
    /// Landscape-local point under the cursor, on the selected landscape.
    pub hit: Option<(Entity, Vec3)>,
    pub stroke: Option<brush::Stroke>,
}

impl Default for LandscapeBrush {
    fn default() -> Self {
        Self {
            tool: None,
            radius: 8.0,
            strength: 4.0,
            material: 0,
            hit: None,
            stroke: None,
        }
    }
}

impl LandscapeBrush {
    /// A tool is armed: viewport clicks paint instead of selecting.
    pub(crate) fn active(&self) -> bool {
        self.tool.is_some()
    }
}

/// The landscape the primary selection belongs to (the landscape itself, or the one its
/// layer sits on) and, when a layer is selected, that layer.
pub(crate) fn selected_landscape(
    selection: &crate::selection::Selection,
    landscapes: &Query<(), With<Landscape>>,
    parents: &Query<&ChildOf>,
) -> Option<(Entity, Option<Entity>)> {
    let primary = selection.primary()?;
    if landscapes.contains(primary) {
        return Some((primary, None));
    }
    let parent = parents.get(primary).ok()?.parent();
    landscapes
        .contains(parent)
        .then_some((parent, Some(primary)))
}

/// Landscapes evaluate their stacks while the editor is up, and load their bakes otherwise.
fn sync_live(
    state: Res<State<crate::AppState>>,
    shown: Res<crate::EditorShown>,
    mut live: ResMut<LandscapeLive>,
) {
    let up = crate::editor_active(state, shown);
    if live.0 != up {
        live.0 = up;
    }
}

/// Tiles, page trees, colliders and LOD rings are derived from the landscape: kept out of
/// the outliner, the inspector, picking and the saved scene. Page colliders are also kept out
/// of the colliders overlay (a heightfield's wireframe is millions of lines a frame).
fn hide_runtime_entities(
    mut commands: Commands,
    added: Query<
        (Entity, Has<PageCollider>),
        (
            Or<(
                Added<LandscapeTile>,
                Added<LandscapeRoot>,
                Added<PageCollider>,
                Added<aurora_landscape::LandscapeRing>,
            )>,
            Without<EditorEntity>,
        ),
    >,
) {
    for (entity, collider) in &added {
        let mut entity = commands.entity(entity);
        entity.try_insert(EditorEntity);
        if collider {
            entity.try_insert(DebugRender::none());
        }
    }
}
