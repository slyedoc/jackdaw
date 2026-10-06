//! The viewport's overlay switches: which gizmo groups draw. Each switch turns groups on or off
//! through their `GizmoConfig::enabled`; the axis indicator, bounding boxes and hierarchy arrows
//! are flags rather than groups. One master switch hides every gizmo while keeping the choices.
//! Remembered per project in `.jackdaw/settings.json`.

use std::collections::BTreeMap;
use std::path::PathBuf;

use bevy::{
    feathers::{controls::FeathersCheckbox, theme::ThemedText},
    gizmos::config::{GizmoConfigGroup, GizmoConfigStore},
    prelude::*,
    ui::Checked,
    ui_widgets::ValueChange,
};
use jackdaw_api::prelude::*;
use jackdaw_feathers::popover::{PopoverPlacement, PopoverProps, popover, popover_content};
use serde::{Deserialize, Serialize};

use crate::project::ProjectRoot;
use crate::project_settings::{Section, load_section, store_section};

const SECTION: &str = "gizmo_overlays";

/// The master switch's key.
pub const ALL: &str = "all";

/// One overlay a user can turn on and off.
pub struct Overlay {
    pub key: &'static str,
    pub label: &'static str,
    pub section: &'static str,
    pub default: bool,
}

/// Every overlay, in the order the panel lists them.
pub const OVERLAYS: &[Overlay] = &[
    Overlay { key: "grid", label: "Grid", section: "Editor", default: true },
    Overlay { key: "axis_indicator", label: "Axis indicator", section: "Editor", default: true },
    Overlay { key: "alignment_guides", label: "Alignment guides", section: "Editor", default: true },
    Overlay { key: "icons", label: "Light and camera icons", section: "Scene", default: true },
    Overlay { key: "brush_outline", label: "Brush outlines", section: "Scene", default: true },
    Overlay { key: "brush_wireframe", label: "Brush wireframes", section: "Scene", default: false },
    Overlay { key: "face_grid", label: "Face grids", section: "Scene", default: false },
    Overlay { key: "bounding_boxes", label: "Bounding boxes", section: "Scene", default: false },
    Overlay { key: "mirror_planes", label: "Mirror planes", section: "Scene", default: true },
    Overlay { key: "colliders", label: "Colliders", section: "Physics", default: false },
    Overlay { key: "hierarchy_arrows", label: "Hierarchy arrows", section: "Physics", default: false },
];

/// Which overlays are on, and the master switch.
#[derive(Resource, Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GizmoOverlays {
    pub all: bool,
    pub on: BTreeMap<String, bool>,
}

impl Default for GizmoOverlays {
    fn default() -> Self {
        Self {
            all: true,
            on: BTreeMap::new(),
        }
    }
}

impl GizmoOverlays {
    /// Whether `key` is chosen on, ignoring the master switch.
    pub fn is_on(&self, key: &str) -> bool {
        if key == ALL {
            return self.all;
        }
        self.on.get(key).copied().unwrap_or_else(|| {
            OVERLAYS
                .iter()
                .find(|overlay| overlay.key == key)
                .is_some_and(|overlay| overlay.default)
        })
    }

    /// Whether `key` draws: chosen on, and gizmos shown at all.
    pub fn shows(&self, key: &str) -> bool {
        self.all && self.is_on(key)
    }

    pub fn set(&mut self, key: &str, on: bool) {
        if key == ALL {
            self.all = on;
        } else {
            self.on.insert(key.to_string(), on);
        }
    }
}

pub(crate) fn plugin(app: &mut App) {
    app.init_resource::<GizmoOverlays>()
        .add_systems(
            Update,
            (load_project_overlays, apply_overlays, sync_overlay_checkboxes).chain(),
        )
        .add_observer(on_overlay_checkbox);
}

fn set_group<G: GizmoConfigGroup>(store: &mut GizmoConfigStore, on: bool) {
    if let Some((config, _)) = store.get_config_mut::<G>() {
        config.enabled = on;
    }
}

/// Push the switches into the gizmo groups and the flags that stand in for groups. Only writes
/// what differs, so the resources are not flagged changed every frame.
fn apply_overlays(
    overlays: Res<GizmoOverlays>,
    mut store: ResMut<GizmoConfigStore>,
    mut settings: ResMut<crate::viewport_overlays::OverlaySettings>,
    mut physics: ResMut<jackdaw_avian_integration::PhysicsOverlayConfig>,
) {
    if overlays.is_changed() {
        let store = store.as_mut();
        for (_, config, _) in store.iter_mut() {
            config.enabled = overlays.all;
        }
        use crate::face_grid::*;
        set_group::<crate::infinite_grid::GridGizmoGroup>(store, overlays.shows("grid"));
        set_group::<crate::alignment_guides::AlignmentGuideGizmoGroup>(
            store,
            overlays.shows("alignment_guides"),
        );
        set_group::<crate::viewport_overlays::EntityGizmoGroup>(store, overlays.shows("icons"));
        set_group::<BrushOutlineUnselectedGizmoGroup>(store, overlays.shows("brush_outline"));
        set_group::<BrushWireframeUnselectedGizmoGroup>(store, overlays.shows("brush_wireframe"));
        set_group::<BrushWireframeSelectedGizmoGroup>(store, overlays.shows("brush_wireframe"));
        set_group::<FaceGridGizmoGroup>(store, overlays.shows("face_grid"));
        set_group::<crate::brush::mirror_plane_overlay::MirrorPlaneGizmoGroup>(
            store,
            overlays.shows("mirror_planes"),
        );
        set_group::<avian3d::prelude::PhysicsGizmos>(store, overlays.shows("colliders"));
    }
    // Flags: kept in step every frame, since an undo can put back an older value.
    let axis = overlays.shows("axis_indicator");
    if settings.show_coordinate_indicator != axis {
        settings.show_coordinate_indicator = axis;
    }
    let boxes = overlays.shows("bounding_boxes");
    if settings.show_bounding_boxes != boxes {
        settings.show_bounding_boxes = boxes;
    }
    let arrows = overlays.shows("hierarchy_arrows");
    if physics.show_hierarchy_arrows != arrows {
        physics.show_hierarchy_arrows = arrows;
    }
}

/// The open project's overlay choices, once per project opened.
fn load_project_overlays(
    project: Option<Res<ProjectRoot>>,
    mut overlays: ResMut<GizmoOverlays>,
    mut loaded_root: Local<Option<PathBuf>>,
) {
    let Some(project) = project else {
        return;
    };
    if loaded_root.as_ref() == Some(&project.root) {
        return;
    }
    *loaded_root = Some(project.root.clone());
    *overlays = load_section(&project.root, Section::Key(SECTION));
}

/// Turn one overlay on or off (or flip it). A preference: no history entry.
#[operator(
    id = "view.overlay",
    label = "Set Overlay",
    description = "Show or hide one kind of viewport gizmo.",
    allows_undo = false,
    params(
        name(String, doc = "Which overlay: `all`, or one of the overlays panel's keys (`grid`, `icons`, `brush_outline`, `colliders`, ...)."),
        on(bool, doc = "On or off. Omit to flip whichever way it currently is.")
    )
)]
pub(crate) fn view_overlay(
    params: In<OperatorParameters>,
    mut overlays: ResMut<GizmoOverlays>,
    project: Option<Res<ProjectRoot>>,
) -> OperatorResult {
    let Some(name) = params.as_str("name") else {
        warn!("view.overlay: no name given");
        return OperatorResult::Cancelled;
    };
    if name != ALL && !OVERLAYS.iter().any(|overlay| overlay.key == name) {
        warn!("view.overlay: no overlay named `{name}`");
        return OperatorResult::Cancelled;
    }
    let on = params.as_bool("on").unwrap_or(!overlays.is_on(name));
    overlays.set(name, on);
    if let Some(project) = project {
        store_section(&project.root, Section::Key(SECTION), &*overlays);
    }
    OperatorResult::Finished
}

/// Flip one overlay, for the View menu's and keymap's toggle operators.
pub(crate) fn toggle(commands: &mut Commands, name: &'static str) {
    commands
        .operator(ViewOverlayOp::ID)
        .param("name", name.to_string())
        .call();
}

/// Marks the toolbar button that opens the overlays panel.
#[derive(Component, Default, Clone)]
pub struct OverlaysButton;

/// The open overlays panel.
#[derive(Component)]
struct OverlaysPanel;

/// One of the panel's checkboxes: the overlay it switches.
#[derive(Component)]
struct OverlayCheckbox(&'static str);

/// Open the overlays panel under its toolbar button, or close it when open.
#[operator(
    id = "view.overlays_menu",
    label = "Overlays",
    description = "Choose which gizmos the viewport draws.",
    allows_undo = false
)]
pub(crate) fn view_overlays_menu(
    _: In<OperatorParameters>,
    mut commands: Commands,
    overlays: Res<GizmoOverlays>,
    buttons: Query<Entity, With<OverlaysButton>>,
    open: Query<Entity, With<OverlaysPanel>>,
) -> OperatorResult {
    if let Some(panel) = open.iter().next() {
        commands.entity(panel).despawn();
        return OperatorResult::Finished;
    }
    let Some(button) = buttons.iter().next() else {
        return OperatorResult::Cancelled;
    };
    let panel = commands
        .spawn((
            OverlaysPanel,
            popover(
                PopoverProps::new(button)
                    .with_placement(PopoverPlacement::BottomEnd)
                    .with_padding(0.0)
                    .with_node(Node {
                        min_width: px(220),
                        ..default()
                    }),
            ),
        ))
        .id();
    let content = commands.spawn((popover_content(), ChildOf(panel))).id();
    spawn_row(&mut commands, content, ALL, "Gizmos", overlays.is_on(ALL));
    let mut section = "";
    for overlay in OVERLAYS {
        if overlay.section != section {
            section = overlay.section;
            commands.spawn((
                Text::new(section),
                TextFont::from_font_size(11.0),
                TextColor(jackdaw_feathers::tokens::TEXT_SECONDARY),
                ChildOf(content),
            ));
        }
        spawn_row(
            &mut commands,
            content,
            overlay.key,
            overlay.label,
            overlays.is_on(overlay.key),
        );
    }
    OperatorResult::Finished
}

fn spawn_row(commands: &mut Commands, parent: Entity, key: &'static str, label: &str, on: bool) {
    let label = label.to_string();
    let mut row = commands.spawn_scene(bsn! {
        @FeathersCheckbox { @caption: bsn! { Text(label) ThemedText } }
    });
    row.insert((OverlayCheckbox(key), ChildOf(parent)));
    if on {
        row.insert(Checked);
    }
}

/// An open panel follows choices made elsewhere (View menu, keymap, a script).
fn sync_overlay_checkboxes(
    overlays: Res<GizmoOverlays>,
    boxes: Query<(Entity, &OverlayCheckbox, Has<Checked>)>,
    mut commands: Commands,
) {
    if !overlays.is_changed() {
        return;
    }
    for (entity, OverlayCheckbox(key), checked) in &boxes {
        let on = overlays.is_on(key);
        if on != checked {
            jackdaw_feathers::utils::set_marker_if_alive::<Checked>(&mut commands, entity, on);
        }
    }
}

/// A panel checkbox changed: reflect it and set the overlay.
fn on_overlay_checkbox(
    event: On<ValueChange<bool>>,
    boxes: Query<&OverlayCheckbox>,
    mut commands: Commands,
) {
    let target = event.event_target();
    let Ok(OverlayCheckbox(key)) = boxes.get(target) else {
        return;
    };
    jackdaw_feathers::utils::set_marker_if_alive::<Checked>(&mut commands, target, event.value);
    commands
        .operator(ViewOverlayOp::ID)
        .param("name", key.to_string())
        .param("on", event.value)
        .call();
}
