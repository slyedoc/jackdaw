//! The viewport grid, as data.
//!
//! `bevy_dev_tools::infinite_grid` draws its grid with a wgpu pipeline, so it is gone on
//! this branch along with the rest of bevy_render. What the editor actually uses of it is
//! all data -- a marker to query, settings to edit from the Snapping panel, and a
//! `Transform` that `view_ops` slides to follow the camera. Those live here, with the same
//! field names and defaults, so `snapping.rs` and `view_ops.rs` are unchanged.
//!
//! Drawing is aurora's job: a ground-plane grid on a ray tracer is a ray-plane
//! intersection in the miss/closest-hit path, not geometry to rasterize.

use bevy::prelude::*;

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

/// Registers the grid components. No draw systems: see the module docs.
pub struct InfiniteGridPlugin;

impl Plugin for InfiniteGridPlugin {
    fn build(&self, app: &mut App) {
        app.register_type::<InfiniteGrid>()
            .register_type::<InfiniteGridSettings>();
    }
}
