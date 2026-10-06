//! Viewport overlay for a selected brush's live Mirror modifier: a faint
//! axis-colored grid at each enabled mirror plane (otherwise invisible,
//! especially once `offset` slides it off the brush origin) with a grab handle
//! at the plane center, drawn as a camera-facing ring like the vertex handles.

use bevy::prelude::*;

use jackdaw_geometry::{MeshMirror, ModifierStack};

use super::gizmo_overlay::{billboard_ring, units_per_pixel};
use crate::brush::Brush;
use crate::selection::Selected;
use crate::viewport::{MainViewportCamera, ViewportCursor};
use crate::{JackdawDrawSystems, default_style};

/// Gizmo group for the mirror plane preview grid.
#[derive(Default, Reflect, GizmoConfigGroup)]
pub struct MirrorPlaneGizmoGroup;

/// On-screen radius of a mirror-plane grab handle, in pixels. The handle is a
/// camera-facing ring sized per frame so it holds this size at any zoom; the
/// hovered handle draws at [`HANDLE_HOVER_PIXELS`].
const HANDLE_PIXELS: f32 = 7.0;

/// On-screen radius of the grab handle while the cursor is over it.
const HANDLE_HOVER_PIXELS: f32 = 10.0;

/// Pixel radius within which the cursor counts as over a grab handle.
const HANDLE_HIT_PIXELS: f32 = 12.0;

/// On-screen radius of the ring drawn around the vertex / edge the plane is
/// snapping to while dragging.
const SNAP_HIGHLIGHT_PIXELS: f32 = 11.0;

/// Bright cyan so a snapped element reads as "locked", distinct from the
/// axis-colored handles and the white hover state.
const SNAP_HIGHLIGHT_COLOR: Color = Color::srgb(0.2, 1.0, 1.0);

/// Which mirror-plane grab handle the cursor is over, if any. Written each
/// frame by `mirror_plane_hover`; the plane-drag operator reads it to know
/// what to grab on press.
#[derive(Resource, Default)]
pub struct MirrorPlaneHover {
    /// (brush entity, axis 0/1/2) whose handle is under the cursor, if any.
    pub target: Option<(Entity, usize)>,
}

pub struct MirrorPlaneOverlayPlugin;

impl Plugin for MirrorPlaneOverlayPlugin {
    fn build(&self, app: &mut App) {
        app.init_gizmo_group::<MirrorPlaneGizmoGroup>()
            .init_resource::<MirrorPlaneHover>()
            .add_systems(Startup, configure_gizmos)
            .add_systems(
                PostUpdate,
                (
                    mirror_plane_hover,
                    draw_mirror_planes,
                    draw_mirror_plane_handles,
                    draw_mirror_plane_snap_highlight,
                )
                    .chain()
                    .in_set(JackdawDrawSystems),
            );
    }
}

fn configure_gizmos(mut config_store: ResMut<GizmoConfigStore>) {
    let (config, _) = config_store.config_mut::<MirrorPlaneGizmoGroup>();
    // Nudge slightly forward so the grid stays crisp where it grazes a face,
    // but keep depth testing so the plane reads as 3D (occluded behind solid
    // geometry, like the brush wireframe).
    config.depth_bias = -0.0005;
    config.line.width = 1.0;
}

/// The two in-plane axis indices for a mirror axis (the axes the plane spans).
fn in_plane_axes(axis: usize) -> (usize, usize) {
    match axis {
        0 => (1, 2),
        1 => (0, 2),
        _ => (0, 1),
    }
}

fn axis_color(axis: usize) -> Color {
    let base = match axis {
        0 => default_style::AXIS_X,
        1 => default_style::AXIS_Y,
        _ => default_style::AXIS_Z,
    };
    base.with_alpha(0.4)
}

/// Full-alpha axis color for the grab handle ring (the faint grid uses the
/// dimmed [`axis_color`]).
fn handle_color(axis: usize) -> Color {
    match axis {
        0 => default_style::AXIS_X_BRIGHT,
        1 => default_style::AXIS_Y_BRIGHT,
        _ => default_style::AXIS_Z_BRIGHT,
    }
}

/// World-space center of the grab handle for `axis`'s mirror plane: the
/// plane-grid center at `mirror.offset[axis]`, with the two in-plane axes at
/// the brush's local bounds midpoint. Returns `None` when the brush has no
/// geometry to bound.
pub(crate) fn plane_handle_world(
    brush: &Brush,
    global_tf: &GlobalTransform,
    mirror: &MeshMirror,
    axis: usize,
) -> Option<Vec3> {
    if brush.topology.vertices.is_empty() {
        return None;
    }
    let mut min = Vec3::splat(f32::MAX);
    let mut max = Vec3::splat(f32::MIN);
    for vert in &brush.topology.vertices {
        min = min.min(vert.position);
        max = max.max(vert.position);
    }
    let (i1, i2) = in_plane_axes(axis);
    let mut local = Vec3::ZERO;
    local[axis] = mirror.offset[axis];
    local[i1] = (min[i1] + max[i1]) * 0.5;
    local[i2] = (min[i2] + max[i2]) * 0.5;
    Some(global_tf.transform_point(local))
}

fn draw_mirror_planes(
    mut gizmos: Gizmos<MirrorPlaneGizmoGroup>,
    brushes: Query<
        (
            Entity,
            &Brush,
            &GlobalTransform,
            &ModifierStack,
            &InheritedVisibility,
        ),
        With<Selected>,
    >,
) {
    for (entity, brush, global_tf, stack, inherited_vis) in &brushes {
        if !inherited_vis.get() {
            continue;
        }
        gizmos.set_owner(Some(entity));
        let Some(mirror) = stack.first_enabled_mirror() else {
            continue;
        };
        if brush.topology.vertices.is_empty() {
            continue;
        }

        // Local-space bounds of the authored geometry, so each plane grid
        // frames the brush it mirrors.
        let mut min = Vec3::splat(f32::MAX);
        let mut max = Vec3::splat(f32::MIN);
        for vert in &brush.topology.vertices {
            min = min.min(vert.position);
            max = max.max(vert.position);
        }

        for (axis, enabled) in [mirror.mirror_x, mirror.mirror_y, mirror.mirror_z]
            .into_iter()
            .enumerate()
        {
            if !enabled {
                continue;
            }
            let (i1, i2) = in_plane_axes(axis);
            // Pad the spanned extent so the plane reaches a little past the
            // geometry on each side.
            let pad1 = ((max[i1] - min[i1]) * 0.15).max(0.1);
            let pad2 = ((max[i2] - min[i2]) * 0.15).max(0.1);
            let lo = Vec2::new(min[i1] - pad1, min[i2] - pad2);
            let hi = Vec2::new(max[i1] + pad1, max[i2] + pad2);
            draw_plane_grid(
                &mut gizmos,
                global_tf,
                axis,
                mirror.offset[axis],
                lo,
                hi,
                axis_color(axis),
            );
        }
    }
}

/// Draw the grab handle of every enabled plane of every selected mirrored
/// brush. Picking stays the screen-distance path in [`mirror_plane_hover`];
/// this only draws the handle.
fn draw_mirror_plane_handles(
    hover: Res<MirrorPlaneHover>,
    camera: Query<(&GlobalTransform, &Projection, &Camera), With<MainViewportCamera>>,
    brushes: Query<
        (
            Entity,
            &Brush,
            &GlobalTransform,
            &ModifierStack,
            &InheritedVisibility,
        ),
        With<Selected>,
    >,
    mut gizmos: Gizmos<MirrorPlaneGizmoGroup>,
) {
    let Ok(camera) = camera.single() else {
        return;
    };
    for (entity, brush, global_tf, stack, inherited_vis) in &brushes {
        if !inherited_vis.get() {
            continue;
        }
        gizmos.set_owner(Some(entity));
        let Some(mirror) = stack.first_enabled_mirror() else {
            continue;
        };
        for (axis, enabled) in [mirror.mirror_x, mirror.mirror_y, mirror.mirror_z]
            .into_iter()
            .enumerate()
        {
            if !enabled {
                continue;
            }
            let Some(world) = plane_handle_world(brush, global_tf, mirror, axis) else {
                continue;
            };
            let (pixels, color) = match hover.target == Some((entity, axis)) {
                true => (HANDLE_HOVER_PIXELS, default_style::EDIT_HOVER_COLOR),
                false => (HANDLE_PIXELS, handle_color(axis)),
            };
            billboard_ring(&mut gizmos, camera, world, pixels, color);
        }
    }
}

/// Draw a grid (border included) on the plane perpendicular to `axis` at
/// coordinate `plane`, spanning `lo..hi` in the two in-plane axes.
fn draw_plane_grid(
    gizmos: &mut Gizmos<MirrorPlaneGizmoGroup>,
    global_tf: &GlobalTransform,
    axis: usize,
    plane: f32,
    lo: Vec2,
    hi: Vec2,
    color: Color,
) {
    const DIVISIONS: usize = 4;
    let (i1, i2) = in_plane_axes(axis);
    let point = |u: f32, v: f32| {
        let mut local = Vec3::ZERO;
        local[axis] = plane;
        local[i1] = u;
        local[i2] = v;
        global_tf.transform_point(local)
    };

    for k in 0..=DIVISIONS {
        let t = k as f32 / DIVISIONS as f32;
        let u = lo.x + (hi.x - lo.x) * t;
        gizmos.line(point(u, lo.y), point(u, hi.y), color);
        let v = lo.y + (hi.y - lo.y) * t;
        gizmos.line(point(lo.x, v), point(hi.x, v), color);
    }
}

/// While dragging a mirror plane, ring the geometry element it has locked onto
/// (a vertex or edge midpoint) so the snap reads clearly. Drawn as a
/// camera-facing circle at constant on-screen size.
fn draw_mirror_plane_snap_highlight(
    mut gizmos: Gizmos<MirrorPlaneGizmoGroup>,
    drag: Res<crate::brush::mirror_plane_ops::MirrorPlaneDragState>,
    camera: Query<
        (&Camera, &GlobalTransform, &Projection),
        With<crate::viewport::MainViewportCamera>,
    >,
    transforms: Query<&GlobalTransform>,
) {
    if !drag.active {
        return;
    }
    let (Some(entity), Some(local)) = (drag.entity, drag.snap_target) else {
        return;
    };
    let Ok((camera, cam_tf, projection)) = camera.single() else {
        return;
    };
    let Ok(brush_global) = transforms.get(entity) else {
        return;
    };
    let world = brush_global.transform_point(local);
    let viewport_height = camera.logical_viewport_size().map_or(1080.0, |s| s.y);
    let to_cam = cam_tf.translation() - world;
    let dist = to_cam.length().max(1e-4);
    let dir = to_cam / dist;
    let radius = SNAP_HIGHLIGHT_PIXELS * units_per_pixel(projection, dist, viewport_height);
    let rotation = Quat::from_rotation_arc(Vec3::Z, dir);
    gizmos.circle(
        Isometry3d::new(world, rotation),
        radius,
        SNAP_HIGHLIGHT_COLOR,
    );
}

/// Track which grab handle, if any, the cursor is over, writing it to
/// [`MirrorPlaneHover`]. Mirrors the brush vertex-hover path: the cursor and
/// each handle are compared in the hovered viewport's camera space, so the
/// hit test stays correct under multi-viewport and UI scaling.
fn mirror_plane_hover(
    vp: ViewportCursor,
    brushes: Query<(Entity, &Brush, &GlobalTransform, &ModifierStack), With<Selected>>,
    mut hover: ResMut<MirrorPlaneHover>,
) {
    let (Some(cursor_pos), Some(camera_entity), Some(viewport_entity)) =
        (vp.cursor(), vp.camera_entity(), vp.viewport_entity())
    else {
        hover.target = None;
        return;
    };
    let Some((camera, cam_tf)) = vp.camera_for(camera_entity) else {
        hover.target = None;
        return;
    };
    let Some(viewport_cursor) = vp.viewport_cursor_for(camera, viewport_entity, cursor_pos) else {
        hover.target = None;
        return;
    };

    let mut best: Option<(Entity, usize)> = None;
    let mut best_dist = HANDLE_HIT_PIXELS;
    for (entity, brush, global_tf, stack) in &brushes {
        let Some(mirror) = stack.first_enabled_mirror() else {
            continue;
        };
        for (axis, enabled) in [mirror.mirror_x, mirror.mirror_y, mirror.mirror_z]
            .into_iter()
            .enumerate()
        {
            if !enabled {
                continue;
            }
            let Some(center) = plane_handle_world(brush, global_tf, mirror, axis) else {
                continue;
            };
            let Ok(screen) = camera.world_to_viewport(cam_tf, center) else {
                continue;
            };
            let dist = (screen - viewport_cursor).length();
            if dist < best_dist {
                best_dist = dist;
                best = Some((entity, axis));
            }
        }
    }

    hover.target = best;
}
