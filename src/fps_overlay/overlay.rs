//! `bevy_dev_tools::fps_overlay`, vendored and drawn without a renderer.
//!
//! Upstream's overlay is render-free apart from its frame time graph, which is a
//! `UiMaterial` reading a `ShaderBuffer` of frame times -- and that one material made
//! bevy_render a dependency of the whole thing. Here the graph is what it depicts: one
//! absolutely-positioned child `Node` per bar, sized in percentages and tinted per bar,
//! which aurora draws like any other UI node.
//!
//! The bar geometry follows upstream's shader exactly, so the graph reads the same:
//! bars are right-aligned with the newest frame at the right edge, height is `log2(dt)`
//! normalized between `dt_min` and `dt_max`, width is proportional to `dt / dt_min`, and
//! colour is green at target fps ramping to red at `min_fps`.

use core::time::Duration;

use bevy::diagnostic::{DiagnosticsStore, FrameTimeDiagnosticsPlugin};
use bevy::math::ops::log2;
use bevy::prelude::*;
use bevy::time::common_conditions::on_timer;
use bevy::text::RemSize;
use bevy::ui::widget::TextUiWriter;
use bevy::ui::ComputedUiRenderTargetInfo;

/// [`GlobalZIndex`] the overlay renders at. Slightly under `i32::MAX` so something can
/// still be put on top of it.
pub const FPS_OVERLAY_ZINDEX: i32 = i32::MAX - 32;

const MIN_SAFE_INTERVAL: Duration = Duration::from_millis(50);

// The graph is sized from the overlay's font size.
const FRAME_TIME_GRAPH_WIDTH_SCALE: f32 = 6.0;
const FRAME_TIME_GRAPH_HEIGHT_SCALE: f32 = 2.0;

/// How many bars the graph can show. Upstream's shader walked however many frame times
/// the diagnostic held; here each bar is an entity, so the pool is fixed and the oldest
/// frames fall off the left.
const MAX_BARS: usize = 128;

/// Adds the FPS overlay. Pulls in [`FrameTimeDiagnosticsPlugin`] if it is not present.
#[derive(Default)]
pub struct FpsOverlayPlugin {
    /// Starting configuration; changeable later through the [`FpsOverlayConfig`] resource.
    pub config: FpsOverlayConfig,
}

/// System sets for FPS overlay updates.
#[derive(SystemSet, Debug, Hash, PartialEq, Eq, Clone)]
pub enum FpsOverlaySystems {
    /// Applies config changes to the overlay UI.
    Customize,
    /// Updates the overlay contents.
    UpdateText,
}

impl Plugin for FpsOverlayPlugin {
    fn build(&self, app: &mut App) {
        if !app.is_plugin_added::<FrameTimeDiagnosticsPlugin>() {
            app.add_plugins(FrameTimeDiagnosticsPlugin::default());
        }

        if self.config.refresh_interval < MIN_SAFE_INTERVAL {
            warn!(
                "Low refresh interval ({:?}) may degrade performance. Min recommended: {:?}.",
                self.config.refresh_interval, MIN_SAFE_INTERVAL
            );
        }

        app.insert_resource(self.config.clone())
            .configure_sets(
                Update,
                FpsOverlaySystems::Customize.before(FpsOverlaySystems::UpdateText),
            )
            .add_systems(Startup, setup)
            .add_systems(
                Update,
                (
                    (toggle_display, customize_overlay)
                        .run_if(resource_changed::<FpsOverlayConfig>)
                        .in_set(FpsOverlaySystems::Customize),
                    (update_text, update_frame_time_graph)
                        .run_if(on_timer(self.config.refresh_interval))
                        .in_set(FpsOverlaySystems::UpdateText),
                ),
            );
    }
}

/// Configuration options for the FPS overlay.
#[derive(Resource, Clone, Reflect)]
#[reflect(Resource)]
pub struct FpsOverlayConfig {
    /// Configuration of text in the overlay.
    pub text_config: TextFont,
    /// Color of text in the overlay.
    pub text_color: Color,
    /// Displays the FPS overlay if true.
    pub enabled: bool,
    /// The period after which the FPS overlay re-renders.
    pub refresh_interval: Duration,
    /// Configuration of the frame time graph.
    pub frame_time_graph_config: FrameTimeGraphConfig,
}

impl Default for FpsOverlayConfig {
    fn default() -> Self {
        Self {
            text_config: TextFont::from_font_size(32.),
            text_color: Color::WHITE,
            enabled: true,
            refresh_interval: Duration::from_millis(100),
            frame_time_graph_config: FrameTimeGraphConfig::target_fps(60.0),
        }
    }
}

/// Configuration of the frame time graph.
#[derive(Clone, Copy, Reflect)]
pub struct FrameTimeGraphConfig {
    /// Is the graph visible.
    pub enabled: bool,
    /// The minimum acceptable FPS. Anything below this shows a red bar.
    pub min_fps: f32,
    /// The target FPS. Anything above this shows a green bar.
    pub target_fps: f32,
}

impl FrameTimeGraphConfig {
    /// A default config for a given target fps.
    pub fn target_fps(target_fps: f32) -> Self {
        Self {
            target_fps,
            ..Self::default()
        }
    }
}

impl Default for FrameTimeGraphConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            min_fps: 30.0,
            target_fps: 60.0,
        }
    }
}

#[derive(Component)]
struct FpsText;

#[derive(Component)]
struct FrameTimeGraph;

#[derive(Component)]
struct FrameTimeBar;

fn setup(mut commands: Commands, overlay_config: Res<FpsOverlayConfig>) {
    commands
        .spawn((
            Node {
                // Must not affect the position of other UI nodes.
                position_type: PositionType::Absolute,
                flex_direction: FlexDirection::Column,
                ..default()
            },
            GlobalZIndex(FPS_OVERLAY_ZINDEX),
            Pickable::IGNORE,
        ))
        .with_children(|p| {
            p.spawn((
                Text::new("FPS: "),
                overlay_config.text_config.clone(),
                TextColor(overlay_config.text_color),
                FpsText,
                Pickable::IGNORE,
            ))
            .with_child((TextSpan::default(), overlay_config.text_config.clone()));

            let font_size = 20.;
            p.spawn((
                Node {
                    width: Val::Px(font_size * FRAME_TIME_GRAPH_WIDTH_SCALE),
                    height: Val::Px(font_size * FRAME_TIME_GRAPH_HEIGHT_SCALE),
                    display: if overlay_config.frame_time_graph_config.enabled {
                        Display::DEFAULT
                    } else {
                        Display::None
                    },
                    ..default()
                },
                Pickable::IGNORE,
                FrameTimeGraph,
            ))
            .with_children(|graph| {
                // The whole pool is spawned once; `update_frame_time_graph` shows the
                // ones it has data for and hides the rest. Spawning and despawning
                // bars every refresh would churn the UI tree for nothing.
                for _ in 0..MAX_BARS {
                    graph.spawn((
                        Node {
                            position_type: PositionType::Absolute,
                            bottom: Val::Px(0.0),
                            display: Display::None,
                            ..default()
                        },
                        BackgroundColor(Color::NONE),
                        Pickable::IGNORE,
                        FrameTimeBar,
                    ));
                }
            });
        });
}

fn update_text(
    diagnostic: Res<DiagnosticsStore>,
    query: Query<Entity, With<FpsText>>,
    mut writer: TextUiWriter,
) {
    if let Ok(entity) = query.single()
        && let Some(fps) = diagnostic.get(&FrameTimeDiagnosticsPlugin::FPS)
        && let Some(value) = fps.smoothed()
    {
        *writer.text(entity, 1) = format!("{value:.2}");
    }
}

/// Lay the bars out right-to-left, newest frame at the right edge.
///
/// Everything is in fractions of the graph node, so this never needs the node's size and
/// survives a resize without recomputing. Mirrors upstream's `frame_time_graph.wesl`.
fn update_frame_time_graph(
    diagnostics: Res<DiagnosticsStore>,
    config: Res<FpsOverlayConfig>,
    graphs: Query<&Children, With<FrameTimeGraph>>,
    mut bars: Query<(&mut Node, &mut BackgroundColor), With<FrameTimeBar>>,
) {
    let Ok(children) = graphs.single() else {
        return;
    };
    if !config.frame_time_graph_config.enabled {
        return;
    }
    let Some(frame_time) = diagnostics.get(&FrameTimeDiagnosticsPlugin::FRAME_TIME) else {
        return;
    };

    // An upper limit above the target, or bars at exactly target fps would vanish.
    let dt_min = 1. / (config.frame_time_graph_config.target_fps * 1.2);
    let dt_max = 1. / config.frame_time_graph_config.min_fps;
    let dt_min_log2 = log2(dt_min);
    let dt_max_log2 = log2(dt_max);

    // Diagnostic values are milliseconds; the thresholds above are seconds.
    let frame_times: Vec<f32> = frame_time.values().map(|v| *v as f32 / 1000.0).collect();
    let len = frame_times.len();

    let mut offset = 0.0f32;
    let mut used = 0usize;
    for (i, bar_entity) in children.iter().enumerate() {
        let Ok((mut node, mut color)) = bars.get_mut(bar_entity) else {
            continue;
        };
        // Walk newest first, placing each bar to the left of the last.
        let Some(dt) = (i < len).then(|| frame_times[len - 1 - i]) else {
            node.display = Display::None;
            continue;
        };
        let width = (dt / dt_min) / len as f32;
        if offset + width > 1.0 {
            node.display = Display::None;
            continue;
        }

        let height = ((log2(dt) - dt_min_log2) / (dt_max_log2 - dt_min_log2)).clamp(0.0, 1.0);
        node.display = Display::DEFAULT;
        node.right = Val::Percent(offset * 100.0);
        node.width = Val::Percent(width * 100.0);
        node.height = Val::Percent(height * 100.0);
        color.0 = bar_color(dt, dt_max);

        offset += width;
        used += 1;
    }

    // Anything past the pool or past the left edge stays hidden.
    debug_assert!(used <= MAX_BARS);
}

/// Green at a fast frame, ramping to red as the frame time approaches `dt_max`.
fn bar_color(dt: f32, dt_max: f32) -> Color {
    let t = (dt / dt_max).clamp(0.0, 1.0);
    Color::linear_rgb(t, 1.0 - t, 0.0)
}

fn customize_overlay(
    overlay_config: Res<FpsOverlayConfig>,
    query: Query<Entity, With<FpsText>>,
    mut writer: TextUiWriter,
) {
    for entity in &query {
        writer.for_each_font(entity, |mut font| {
            *font = overlay_config.text_config.clone();
        });
        writer.for_each_color(entity, |mut color| color.0 = overlay_config.text_color);
    }
}

fn toggle_display(
    overlay_config: Res<FpsOverlayConfig>,
    mut text_node: Single<
        (&mut Node, &ComputedUiRenderTargetInfo),
        (With<FpsText>, Without<FrameTimeGraph>),
    >,
    mut graph_node: Single<&mut Node, (With<FrameTimeGraph>, Without<FpsText>)>,
    rem_size: Res<RemSize>,
) {
    text_node.0.display = if overlay_config.enabled {
        Display::DEFAULT
    } else {
        Display::None
    };

    if overlay_config.frame_time_graph_config.enabled {
        // Scale the graph from the overlay's font size, as upstream does.
        let font_size = overlay_config
            .text_config
            .font_size
            .eval(text_node.1.logical_size(), *rem_size);
        graph_node.width = Val::Px(font_size * FRAME_TIME_GRAPH_WIDTH_SCALE);
        graph_node.height = Val::Px(font_size * FRAME_TIME_GRAPH_HEIGHT_SCALE);
        graph_node.display = Display::DEFAULT;
    } else {
        graph_node.display = Display::None;
    }
}

#[cfg(test)]
mod tests {
    use super::bar_color;

    #[test]
    fn a_fast_frame_is_green_and_a_slow_one_red() {
        let dt_max = 1.0 / 30.0;
        let fast = bar_color(1.0 / 240.0, dt_max).to_linear();
        let slow = bar_color(dt_max, dt_max).to_linear();
        assert!(fast.green > fast.red, "fast frame should read green");
        assert!(slow.red > slow.green, "slow frame should read red");
    }

    #[test]
    fn a_frame_past_the_floor_clamps_instead_of_wrapping() {
        let dt_max = 1.0 / 30.0;
        let awful = bar_color(dt_max * 10.0, dt_max).to_linear();
        assert!((awful.red - 1.0).abs() < 1e-5);
        assert!(awful.green.abs() < 1e-5);
    }
}
