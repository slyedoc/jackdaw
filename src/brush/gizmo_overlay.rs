use bevy::prelude::*;

use super::interaction::{
    BrushDragState, EdgeDragState, FaceExtrudeMode, VertexDragConstraint, VertexDragState,
};
use super::{BrushEditMode, BrushMeshCache, BrushSelection, EditMode, LoopCutPreviewLines};
use crate::default_style;
use crate::face_grid::BrushOutlineSelectedGizmoGroup;
use crate::viewport::MainViewportCamera;

/// On-screen radius of a vertex handle, in pixels. The handles are
/// camera-facing rings sized per frame so they hold this size at any zoom.
const VERTEX_HANDLE_PIXELS: f32 = 6.0;

/// Selection state of a handle or edge; indexes [`STATE_COLORS`].
#[derive(Clone, Copy)]
enum HandleState {
    Available = 0,
    Selected = 1,
    Hovered = 2,
}

const STATE_COLORS: [Color; 3] = [
    default_style::EDIT_AVAILABLE_COLOR,
    default_style::EDIT_SELECTED_COLOR,
    default_style::EDIT_HOVER_COLOR,
];

/// World size of one screen pixel at `dist` from the camera, for either
/// projection. Lets world geometry hold a constant on-screen size.
pub(crate) fn units_per_pixel(projection: &Projection, dist: f32, viewport_height: f32) -> f32 {
    match projection {
        Projection::Perspective(p) => (2.0 * dist * (p.fov * 0.5).tan()) / viewport_height,
        Projection::Orthographic(o) => o.area.height() / viewport_height,
        Projection::Custom(_) => dist * 0.002,
    }
}

/// A ring facing the camera at `world`, `pixels` in radius on screen.
pub(super) fn billboard_ring<G: GizmoConfigGroup>(
    gizmos: &mut Gizmos<G>,
    camera: (&GlobalTransform, &Projection, &Camera),
    world: Vec3,
    pixels: f32,
    color: Color,
) {
    let (cam_global, projection, cam) = camera;
    let viewport_height = cam.logical_viewport_size().map_or(1080.0, |s| s.y);
    let to_cam = cam_global.translation() - world;
    let dist = to_cam.length().max(1e-4);
    let rotation = Quat::from_rotation_arc(Vec3::Z, to_cam / dist);
    let radius = pixels * units_per_pixel(projection, dist, viewport_height);
    gizmos.circle(Isometry3d::new(world, rotation), radius, color);
}

/// Draw a ring at every vertex of every edit brush, coloured by selection
/// state. Picking stays the cache-based screen-space path; this only draws.
pub(super) fn draw_vertex_handles(
    edit_mode: Res<EditMode>,
    brush_selection: Res<BrushSelection>,
    hover: Res<super::BrushFaceHover>,
    brush_caches: Query<&BrushMeshCache>,
    brush_transforms: Query<&GlobalTransform>,
    camera: Query<(&GlobalTransform, &Projection, &Camera), With<MainViewportCamera>>,
    mut gizmos: Gizmos<BrushOutlineSelectedGizmoGroup>,
) {
    // Gather the world position and selection state for every vertex to show.
    let mut desired: Vec<(Vec3, HandleState)> = Vec::new();
    if let EditMode::BrushEdit(mode) = *edit_mode
        && matches!(mode, BrushEditMode::Vertex | BrushEditMode::Knife)
    {
        for brush_entity in brush_selection.edit_brushes() {
            let (Ok(cache), Ok(brush_global)) = (
                brush_caches.get(brush_entity),
                brush_transforms.get(brush_entity),
            ) else {
                continue;
            };
            let sub = brush_selection.sub(brush_entity);
            let hover_vi = if hover.entity == Some(brush_entity) {
                hover.vertex_index
            } else {
                None
            };
            for (vi, v) in cache.vertices.iter().enumerate() {
                // Bisect-introduced cut geometry has no authored origin and
                // draws no editable handle.
                if cache.authored_vert(vi).is_none() {
                    continue;
                }
                let selected = sub.is_some_and(|s| s.vertices.contains(&vi));
                let state = if hover_vi == Some(vi) && !selected {
                    HandleState::Hovered
                } else if selected {
                    HandleState::Selected
                } else {
                    HandleState::Available
                };
                desired.push((brush_global.transform_point(*v), state));
            }
        }
    }

    let Ok(camera) = camera.single() else {
        return;
    };
    for (world, state) in desired {
        billboard_ring(
            &mut gizmos,
            camera,
            world,
            VERTEX_HANDLE_PIXELS,
            STATE_COLORS[state as usize],
        );
    }
}

/// Draw the edit-mode edge wireframe, coloured by selection state.
pub(super) fn draw_edit_edges(
    edit_mode: Res<EditMode>,
    brush_selection: Res<BrushSelection>,
    hover: Res<super::BrushFaceHover>,
    brush_caches: Query<&BrushMeshCache>,
    brush_transforms: Query<&GlobalTransform>,
    mut gizmos: Gizmos<BrushOutlineSelectedGizmoGroup>,
) {
    // The edit-mesh edges are the wireframe in every sub-mode (the object
    // wireframe stands down while editing). Edges carry selection / hover
    // colors only where edges are the selectable element; in vertex / face
    // mode they are the resting wireframe. Clip mode hides the wireframe.
    let editing = matches!(
        *edit_mode,
        EditMode::BrushEdit(
            BrushEditMode::Vertex
                | BrushEditMode::Edge
                | BrushEditMode::Face
                | BrushEditMode::Knife
        )
    );
    let edges_selectable = matches!(
        *edit_mode,
        EditMode::BrushEdit(BrushEditMode::Edge | BrushEditMode::Knife)
    );
    if !editing {
        return;
    }
    for brush_entity in brush_selection.edit_brushes() {
        let (Ok(cache), Ok(brush_global)) = (
            brush_caches.get(brush_entity),
            brush_transforms.get(brush_entity),
        ) else {
            continue;
        };
        let sub = brush_selection.sub(brush_entity);
        let hover_edge = if edges_selectable && hover.entity == Some(brush_entity) {
            hover.edge
        } else {
            None
        };
        for (a, b) in cache.unique_edges() {
            // A cut/cap edge (either endpoint is bisect geometry) has no
            // authored origin and draws no selectable edit edge.
            if cache.authored_edge((a, b)).is_none() {
                continue;
            }
            let selected = edges_selectable && sub.is_some_and(|s| s.edges.contains(&(a, b)));
            let state = if hover_edge.is_some_and(|he| he == (a, b) || he == (b, a)) && !selected {
                HandleState::Hovered
            } else if selected {
                HandleState::Selected
            } else {
                HandleState::Available
            };
            let wa = brush_global.transform_point(cache.vertices[a]);
            let wb = brush_global.transform_point(cache.vertices[b]);
            gizmos.line(wa, wb, STATE_COLORS[state as usize]);
        }
    }
}

/// Draw the outline of `face_index` from `cache`, bounds-checking both the
/// face index and every vertex index. A destructive edit (e.g. face delete)
/// shrinks the topology while a stale hover or selection index lingers for a
/// frame, so indexing has to tolerate an out-of-range value instead of
/// panicking.
fn draw_face_outline(
    gizmos: &mut Gizmos<BrushOutlineSelectedGizmoGroup>,
    brush_global: &GlobalTransform,
    cache: &BrushMeshCache,
    face_index: usize,
    color: Color,
) {
    let Some(polygon) = cache.face_polygons.get(face_index) else {
        return;
    };
    if polygon.len() < 3 {
        return;
    }
    for i in 0..polygon.len() {
        let (Some(a), Some(b)) = (
            cache.vertices.get(polygon[i]),
            cache.vertices.get(polygon[(i + 1) % polygon.len()]),
        ) else {
            continue;
        };
        gizmos.line(
            brush_global.transform_point(*a),
            brush_global.transform_point(*b),
            color,
        );
    }
}

pub(super) fn draw_brush_edit_gizmos(
    edit_mode: Res<EditMode>,
    brush_selection: Res<BrushSelection>,
    brush_caches: Query<&BrushMeshCache>,
    brush_transforms: Query<&GlobalTransform>,
    vertex_drag: Res<VertexDragState>,
    edge_drag: Res<EdgeDragState>,
    face_drag: Res<BrushDragState>,
    hover: Res<super::BrushFaceHover>,
    mut gizmos: Gizmos<BrushOutlineSelectedGizmoGroup>,
) {
    // Draw hover face outline (works in both Object and Edit modes).
    // In edit mode the hover entity may be any edit brush, not just the active one.
    if let (Some(hover_entity), Some(hover_face)) = (hover.entity, hover.face_index)
        && let Ok(cache) = brush_caches.get(hover_entity)
        && let Ok(brush_global) = brush_transforms.get(hover_entity)
    {
        // Skip if face is already selected (avoid double highlight).
        let is_selected = brush_selection
            .sub(hover_entity)
            .is_some_and(|s| s.faces.contains(&hover_face));
        if !is_selected {
            draw_face_outline(
                &mut gizmos,
                brush_global,
                cache,
                hover_face,
                default_style::EDIT_HOVER_COLOR,
            );
        }
    }

    let EditMode::BrushEdit(mode) = *edit_mode else {
        return;
    };

    // Collect edit brushes to avoid holding an immutable borrow on
    // brush_selection while we call sub() below.
    let edit_brushes: Vec<Entity> = brush_selection.edit_brushes().collect();
    let active_brush = brush_selection.active_brush;

    if edit_brushes.is_empty() {
        return;
    }

    // Draw handles on every edit brush. All selected brushes are equally
    // editable, so their handles share one resting color.
    for &brush_entity in &edit_brushes {
        let Ok(cache) = brush_caches.get(brush_entity) else {
            continue;
        };
        let Ok(brush_global) = brush_transforms.get(brush_entity) else {
            continue;
        };

        let sub = brush_selection.sub(brush_entity);

        // Vertex handles and edge wireframes are drawn as meshes by
        // `update_vertex_handles` and `update_edge_overlay`, not as
        // immediate-mode gizmos.

        // Highlight selected faces.
        if mode == BrushEditMode::Face {
            let faces = sub.map(|s| s.faces.as_slice()).unwrap_or(&[]);
            for &face_idx in faces {
                draw_face_outline(
                    &mut gizmos,
                    brush_global,
                    cache,
                    face_idx,
                    default_style::EDIT_SELECTED_COLOR,
                );
            }
        }
    }

    // Drag constraint line and extend preview use the active brush's transform.
    // If there is no active brush, skip these overlays.
    let Some(active_entity) = active_brush else {
        return;
    };
    let Ok(active_global) = brush_transforms.get(active_entity) else {
        return;
    };

    // Draw extend mode wireframe preview
    if face_drag.active && face_drag.extrude_mode == FaceExtrudeMode::Extend {
        let polygon = &face_drag.extend_face_polygon;
        let depth = face_drag.extend_depth;
        let normal = face_drag.extend_face_normal;
        let offset = normal * depth;
        let preview_color = default_style::FACE_EXTRUDE_PREVIEW;

        if polygon.len() >= 3 {
            // Base polygon edges
            for i in 0..polygon.len() {
                let a = polygon[i];
                let b = polygon[(i + 1) % polygon.len()];
                gizmos.line(a, b, preview_color);
            }
            // Top polygon edges (base + offset)
            for i in 0..polygon.len() {
                let a = polygon[i] + offset;
                let b = polygon[(i + 1) % polygon.len()] + offset;
                gizmos.line(a, b, preview_color);
            }
            // Connecting edges
            for &v in polygon {
                gizmos.line(v, v + offset, preview_color);
            }
        }
    }

    // Draw drag constraint line (vertex or edge drag)
    let active_constraint = if vertex_drag.active {
        Some(vertex_drag.constraint)
    } else if edge_drag.active {
        Some(edge_drag.constraint)
    } else {
        None
    };
    if let Some(constraint) = active_constraint
        && constraint != VertexDragConstraint::Free
    {
        let (axis_dir, color) = match constraint {
            VertexDragConstraint::AxisX => (Vec3::X, default_style::AXIS_X),
            VertexDragConstraint::AxisY => (Vec3::Y, default_style::AXIS_Y),
            VertexDragConstraint::AxisZ => (Vec3::Z, default_style::AXIS_Z),
            VertexDragConstraint::Free => unreachable!(),
        };
        let (_, brush_rot, _) = active_global.to_scale_rotation_translation();
        let world_axis = brush_rot * axis_dir;
        let center = active_global.translation();
        gizmos.line(
            center - world_axis * 50.0,
            center + world_axis * 50.0,
            color,
        );
    }
}

/// Draw cyan line segments for the loop cut preview, sourced from `LoopCutPreviewLines`.
pub(super) fn draw_loop_cut_preview(
    preview_lines: Res<LoopCutPreviewLines>,
    mut gizmos: Gizmos<BrushOutlineSelectedGizmoGroup>,
) {
    for &(a, b) in &preview_lines.lines {
        gizmos.line(a, b, Color::srgb(0.3, 0.85, 1.0));
    }
}
