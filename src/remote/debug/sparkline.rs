//! Sample buffer and plot geometry for the debugger's live sparklines.
//!
//! `RingBuffer` is a pure FIFO of recent values. The plot itself is a
//! [`UiPolyline`]: aurora draws one rounded quad per segment, batched with the rest
//! of the UI, so there is no material and no shader. The old `SparklineMaterial`
//! also filled the area under the curve; the stroke is what carries the reading,
//! and a fill would need a triangulated mesh rather than a polyline.

use bevy::prelude::*;
use bevy_aurora::ui_render::UiPolyline;

/// A fixed-capacity FIFO of `f32` samples for a sparkline. Pure, no Bevy.
#[derive(Debug, Clone)]
pub struct RingBuffer {
    data: std::collections::VecDeque<f32>,
    cap: usize,
}

impl RingBuffer {
    pub fn new(cap: usize) -> Self {
        Self {
            data: std::collections::VecDeque::with_capacity(cap),
            cap,
        }
    }

    /// Append a sample, evicting the oldest once at capacity.
    pub fn push(&mut self, v: f32) {
        if self.data.len() == self.cap {
            self.data.pop_front();
        }
        self.data.push_back(v);
    }

    /// The current window, oldest first.
    pub fn samples(&self) -> Vec<f32> {
        self.data.iter().copied().collect()
    }

    /// Smallest sample in the window, or `+inf` when empty.
    pub fn min(&self) -> f32 {
        self.data.iter().copied().fold(f32::INFINITY, f32::min)
    }

    /// Largest sample in the window, or `-inf` when empty.
    pub fn max(&self) -> f32 {
        self.data.iter().copied().fold(f32::NEG_INFINITY, f32::max)
    }

    /// The most recent sample, or `0.0` when empty.
    pub fn last(&self) -> f32 {
        self.data.back().copied().unwrap_or(0.0)
    }
}

/// Number of samples a sparkline plots at once.
pub const SPARKLINE_WINDOW: usize = 64;

/// Stroke width of a sparkline, logical pixels.
const SPARKLINE_THICKNESS: f32 = 1.5;

/// A parent-filling sparkline node tinted with `color`, ready to spawn.
///
/// The points are filled in per frame by whoever owns the samples; an empty
/// polyline draws nothing, so a card looks blank rather than wrong until its
/// first reply arrives.
pub fn sparkline_node(color: Color) -> impl Bundle {
    (
        Node {
            width: percent(100),
            height: percent(100),
            ..default()
        },
        UiPolyline {
            points: Vec::new(),
            thickness: SPARKLINE_THICKNESS,
            color,
            closed: false,
        },
    )
}

/// The most recent `SPARKLINE_WINDOW` samples mapped into `size`, oldest at the left.
///
/// Values are normalized over the plotted window's own min/max, so a flat series sits
/// mid-height instead of collapsing onto an axis. `size` is the node's LOGICAL size,
/// which is the frame `UiPolyline` reads points in; y is flipped because UI y grows
/// downward and a larger sample should sit higher.
pub fn plot_points(buf: &RingBuffer, size: Vec2) -> Vec<Vec2> {
    let all = buf.samples();
    let window = &all[all.len().saturating_sub(SPARKLINE_WINDOW)..];
    if window.len() < 2 || size.x <= 0.0 || size.y <= 0.0 {
        return Vec::new();
    }
    let min = window.iter().copied().fold(f32::INFINITY, f32::min);
    let max = window.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    let span = max - min;
    let step_x = size.x / (window.len() - 1) as f32;
    window
        .iter()
        .enumerate()
        .map(|(i, &v)| {
            // A zero span means every sample is equal; halfway up reads better than
            // a divide by zero or a line pinned to the bottom.
            let t = if span > f32::EPSILON {
                (v - min) / span
            } else {
                0.5
            };
            // Inset by the stroke so the extremes are not clipped by the node's edge.
            let inset = SPARKLINE_THICKNESS;
            let usable = (size.y - inset * 2.0).max(0.0);
            Vec2::new(i as f32 * step_x, inset + (1.0 - t) * usable)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{RingBuffer, SPARKLINE_WINDOW, plot_points};
    use bevy::math::Vec2;

    #[test]
    fn push_evicts_oldest_at_capacity() {
        let mut b = RingBuffer::new(3);
        b.push(1.0);
        b.push(2.0);
        b.push(3.0);
        b.push(4.0);
        assert_eq!(b.samples(), vec![2.0, 3.0, 4.0]);
        assert_eq!(b.last(), 4.0);
    }

    #[test]
    fn min_max_over_current_window() {
        let mut b = RingBuffer::new(4);
        for v in [5.0, 1.0, 9.0, 3.0] {
            b.push(v);
        }
        assert_eq!(b.min(), 1.0);
        assert_eq!(b.max(), 9.0);
    }

    #[test]
    fn empty_buffer_is_safe() {
        let b = RingBuffer::new(4);
        assert_eq!(b.samples(), Vec::<f32>::new());
        assert_eq!(b.last(), 0.0);
    }

    #[test]
    fn plot_normalizes_over_the_window_min_and_max() {
        let mut b = RingBuffer::new(4);
        for v in [5.0, 1.0, 9.0, 3.0] {
            b.push(v);
        }
        let size = Vec2::new(30.0, 20.0);
        let pts = plot_points(&b, size);
        assert_eq!(pts.len(), 4);
        // x marches evenly left to right across the node.
        assert!((pts[0].x - 0.0).abs() < 1e-5);
        assert!((pts[3].x - size.x).abs() < 1e-5);
        // The largest sample sits highest (smallest y), the smallest lowest.
        assert!(pts[2].y < pts[0].y, "9.0 should plot above 5.0");
        assert!(pts[1].y > pts[0].y, "1.0 should plot below 5.0");
    }

    #[test]
    fn plot_keeps_only_the_most_recent_window() {
        let mut b = RingBuffer::new(SPARKLINE_WINDOW * 2);
        for i in 0..(SPARKLINE_WINDOW * 2) {
            b.push(i as f32);
        }
        let pts = plot_points(&b, Vec2::new(64.0, 10.0));
        assert_eq!(pts.len(), SPARKLINE_WINDOW);
    }

    #[test]
    fn a_flat_series_plots_mid_height_rather_than_dividing_by_zero() {
        let mut b = RingBuffer::new(4);
        for _ in 0..4 {
            b.push(7.0);
        }
        let pts = plot_points(&b, Vec2::new(10.0, 20.0));
        assert_eq!(pts.len(), 4);
        assert!(pts.iter().all(|p| (p.y - pts[0].y).abs() < 1e-5));
        assert!(pts[0].y.is_finite());
    }

    #[test]
    fn too_few_samples_plot_nothing() {
        let mut b = RingBuffer::new(4);
        b.push(1.0);
        assert!(plot_points(&b, Vec2::new(10.0, 10.0)).is_empty());
    }
}
