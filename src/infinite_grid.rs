//! The viewport grid, as data.
//!
//! `bevy_dev_tools::infinite_grid` draws its grid with a wgpu pipeline, so it is gone on
//! this branch along with the rest of bevy_render. What the editor actually uses of it is
//! all data -- a marker to query, settings to edit from the Snapping panel, and a
//! `Transform` that `view_ops` slides to follow the camera. Those live here, with the same
//! field names and defaults, so `snapping.rs` and `view_ops.rs` are unchanged.
//!
//! Drawn as gizmo lines, which aurora rasterizes over the traced frame and depth-tests
//! against its depth guide. A ray-plane intersection in the miss path would be sharper at
//! grazing angles; this needs no aurora-side pipeline.

use bevy::prelude::*;

use crate::viewport::MainViewportCamera;

/// Marks the entity carrying the viewport grid. Its `Transform` positions the plane.
#[derive(Component, Copy, Clone, Debug, Default, Reflect)]
#[reflect(Component, Default)]
#[require(Transform, Visibility)]
pub struct InfiniteGrid;

/// Grid appearance. On the grid entity, or on a camera that should see it differently.
#[derive(Component, Copy, Clone, Debug, Reflect)]
#[reflect(Component, Default)]
pub struct InfiniteGridSettings {
    /// The color of the X axis
    pub x_axis_color: Color,
    /// The color of the Z axis
    pub z_axis_color: Color,
    /// The color of the minor lines of the grid
    pub minor_line_color: Color,
    /// The color of the major lines of the grid
    pub major_line_color: Color,
    /// How far the grid will be visible relative to the camera
    pub fadeout_distance: f32,
    /// How quickly the grid will fadeout
    pub dot_fadeout_strength: f32,
    /// The scale of the distance between the lines. A smaller value increases the distance
    /// between the lines
    pub scale: f32,
    /// The interval at which major lines are drawn
    pub major_line_interval: u32,
}

impl Default for InfiniteGridSettings {
    fn default() -> Self {
        // Same values bevy_dev_tools used, so the grid looks unchanged once aurora draws it.
        Self {
            x_axis_color: Color::oklcha(0.5232, 0.1404, 13.84, 1.0),
            z_axis_color: Color::oklcha(0.4847, 0.1249, 253.08, 1.0),
            minor_line_color: Color::srgb(0.2, 0.2, 0.2),
            major_line_color: Color::srgb(0.25, 0.25, 0.25),
            fadeout_distance: 100.,
            dot_fadeout_strength: 0.25,
            scale: 1.0,
            major_line_interval: 10,
        }
    }
}

/// Caps the line count when the spacing is small relative to `fadeout_distance`.
const MAX_LINES_PER_AXIS: i32 = 512;

/// Emits the grid as gizmo lines on the grid entity's local XZ plane, centred on the
/// viewport camera so it reads as unbounded, with alpha falling off to `fadeout_distance`.
fn draw_infinite_grid(
    mut gizmos: Gizmos<GridGizmoGroup>,
    grids: Query<(&GlobalTransform, &InfiniteGridSettings), With<InfiniteGrid>>,
    camera: Query<&GlobalTransform, With<MainViewportCamera>>,
) {
    // `iter().next()`, not `single()`: a second viewport panel spawns a second
    // `MainViewportCamera` and the grid would vanish from both.
    let Some(camera) = camera.iter().next() else {
        return;
    };
    for (grid, settings) in &grids {
        // `scale` is lines per unit (snapping.rs writes `1.0 / grid_size`), not spacing.
        if settings.scale <= 0.0 {
            continue;
        }
        let interval = settings.major_line_interval.max(2) as f32;
        let to_local = grid.affine().inverse();
        let eye = to_local.transform_point3(camera.translation());

        // Coarsen by whole intervals until the lines are no denser than roughly 1/60 of the
        // camera's distance to the plane, or the count fits. Drawing a 0.25 m grid out to
        // 100 m is 800 lines of grey haze.
        let radius = settings.fadeout_distance;
        let mut step = 1.0 / settings.scale;
        let min_step = eye.y.abs() / 60.0;
        while step < min_step || radius / step > MAX_LINES_PER_AXIS as f32 {
            step *= interval;
        }
        // Lines sit at whole multiples of `step` in grid space, numbered from the origin, so
        // they stay put as the camera moves; only the fade window follows the eye.
        let count = (radius / step).ceil() as i64;
        let interval = interval as i64;
        let (ex, ez) = (eye.x, eye.z);
        let (kx, kz) = ((ex / step).round() as i64, (ez / step).round() as i64);

        // Alpha peaks at the point on the line nearest the eye and reaches zero at
        // `radius`, so each line is two gradient segments meeting under the camera. One
        // segment end to end would fade both its endpoints to zero and interpolate nothing.
        let alpha = |d: f32| (1.0 - d / radius).clamp(0.0, 1.0);
        let mut ray = |near: Vec3, far: Vec3, offset: f32, color: Color| {
            let at = |a: f32| color.with_alpha(color.alpha() * a);
            let (inner, outer) = (at(alpha(offset.abs())), at(alpha(radius)));
            gizmos.line_gradient(
                grid.transform_point(near),
                grid.transform_point(far),
                inner,
                outer,
            );
        };
        let color_of = |k: i64, axis: Color| {
            if k == 0 {
                axis
            } else if k.rem_euclid(interval) == 0 {
                settings.major_line_color
            } else {
                settings.minor_line_color
            }
        };

        for i in -count..=count {
            let k = kx + i;
            let x = k as f32 * step;
            let color = color_of(k, settings.z_axis_color);
            let mid = Vec3::new(x, 0.0, ez);
            ray(mid, Vec3::new(x, 0.0, ez - radius), x - ex, color);
            ray(mid, Vec3::new(x, 0.0, ez + radius), x - ex, color);

            let k = kz + i;
            let z = k as f32 * step;
            let color = color_of(k, settings.x_axis_color);
            let mid = Vec3::new(ex, 0.0, z);
            ray(mid, Vec3::new(ex - radius, 0.0, z), z - ez, color);
            ray(mid, Vec3::new(ex + radius, 0.0, z), z - ez, color);
        }
    }
}

/// The viewport grid's lines, so the overlays can turn them off on their own.
#[derive(Default, Reflect, GizmoConfigGroup)]
pub struct GridGizmoGroup;

/// Registers the grid components and their draw system.
pub struct InfiniteGridPlugin;

impl Plugin for InfiniteGridPlugin {
    fn build(&self, app: &mut App) {
        app.register_type::<InfiniteGrid>()
            .register_type::<InfiniteGridSettings>()
            .init_gizmo_group::<GridGizmoGroup>()
            .add_systems(Update, draw_infinite_grid);
    }
}
