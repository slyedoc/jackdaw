//! Clip scrubbing: while a clip's timeline is open, a character in the scene plays that clip
//! alone, posed at the timeline cursor, so event times are placed against the pose they mark.
//!
//! The rig is the selected character, else the first whose skeleton is the clip's, else any.
//! Its player is handed a one-node graph (the clip) and seeked to the cursor every frame; closing
//! the timeline hands it back its own graph.
//!
//! Over the timeline: drag the ruler to scrub, Space play/pause (loops), Left/Right a frame
//! (Shift: 0.1 s), Home/End the ends, E adds an event at the cursor (a copy of the selected one,
//! on its track), Delete removes the selected event.

use bevy::prelude::*;
use bevy_animation_graph::{
    builtin_nodes::clip_node::ClipNode,
    core::{
        animation_clip::GraphClip,
        animation_graph::{AnimationGraph, DEFAULT_OUTPUT_POSE},
        animation_graph_player::AnimationGraphPlayer,
        animation_node::AnimationNode,
        edge_data::{DataSpec, events::AnimationEvent},
        event_track::{EventTrack, TrackItem, TrackItemValue},
    },
};
use jackdaw_animation_runtime::AnimationRig;

use super::{Canvas, CanvasView, LABEL_W, PX_PER_SEC, Selected, TimelineCursor, canvas_cursor};

/// Frame step for the arrow keys: the 30 fps the clips are baked at.
const FRAME: f32 = 1.0 / 30.0;
/// Length of an event added with nothing selected to copy.
const NEW_EVENT_SPAN: f32 = 0.1;

/// Which rig is posed by which clip, and whether the cursor is running.
#[derive(Resource, Default)]
pub(super) struct Scrub {
    rig: Option<Entity>,
    clip: Option<AssetId<GraphClip>>,
    pub(super) playing: bool,
}

/// On a rig while a clip owns its player: the editor's play/pause leaves it alone.
#[derive(Component)]
pub(crate) struct Scrubbing;

/// The timeline's cursor line, moved in place rather than redrawn with the lanes.
#[derive(Component)]
pub(super) struct CursorLine;

/// The ruler strip: press or drag it to put the cursor under the pointer.
#[derive(Component)]
pub(super) struct ScrubRuler;

pub(super) fn plugin(app: &mut App) {
    app.init_resource::<Scrub>().add_systems(
        Update,
        (
            bind_rig,
            scrub_keys.run_if(super::graph_window_hovered),
            advance,
            move_cursor_line,
        )
            .chain(),
    );
}

/// Hand the timeline's clip to a rig, and the rig back its graph when the timeline closes.
fn bind_rig(
    mut scrub: ResMut<Scrub>,
    view: Res<CanvasView>,
    clips: Res<Assets<GraphClip>>,
    mut graphs: ResMut<Assets<AnimationGraph>>,
    selection: Res<crate::selection::Selection>,
    mut rigs: Query<(Entity, &AnimationRig, &mut AnimationGraphPlayer)>,
    mut commands: Commands,
) {
    let want = view.clip.as_ref().map(Handle::id);
    if scrub.clip == want {
        return;
    }
    if let Some(rig) = scrub.rig.take()
        && let Ok((_, own, mut player)) = rigs.get_mut(rig)
    {
        player.start(own.graph.clone());
        commands.entity(rig).remove::<Scrubbing>();
    }
    scrub.clip = None;
    scrub.playing = false;
    let Some(handle) = view.clip.clone() else {
        return;
    };
    let Some(clip) = clips.get(&handle) else {
        return; // still loading; try again next frame
    };
    let clip_skeleton = clip.skeleton.path().cloned();
    let selected = selection.primary().filter(|e| rigs.contains(*e));
    let same_rig = || {
        rigs.iter()
            .find(|(_, rig, _)| rig.skeleton.path().cloned() == clip_skeleton)
            .map(|(e, _, _)| e)
    };
    let Some(rig) = selected
        .or_else(same_rig)
        .or_else(|| rigs.iter().next().map(|(e, _, _)| e))
    else {
        scrub.clip = want;
        info!("timeline: no character in the scene to preview the clip on");
        return;
    };

    let mut graph = AnimationGraph::new();
    let node = AnimationNode::new("clip", ClipNode::new(handle, None, None));
    let id = node.id;
    graph.add_node(node);
    graph.add_output_data(DEFAULT_OUTPUT_POSE.into(), DataSpec::Pose);
    graph.add_output_time();
    graph.add_output_data_edge(id, ClipNode::OUT_POSE, DEFAULT_OUTPUT_POSE);
    graph.add_output_pose_edge(id);
    let graph = graphs.add(graph);

    let Ok((_, _, mut player)) = rigs.get_mut(rig) else {
        return;
    };
    player.start(graph);
    player.pause();
    commands.entity(rig).insert(Scrubbing);
    scrub.rig = Some(rig);
    scrub.clip = want;
}

/// Space, arrows, Home/End, E and Delete over the timeline.
fn scrub_keys(
    keys: Res<ButtonInput<KeyCode>>,
    mut scrub: ResMut<Scrub>,
    mut cursor: ResMut<TimelineCursor>,
    mut view: ResMut<CanvasView>,
    mut selected: ResMut<Selected>,
    mut clips: ResMut<Assets<GraphClip>>,
) {
    let Some(handle) = view.clip.clone() else {
        return;
    };
    let Some(duration) = clips.get(&handle).map(|c| c.duration) else {
        return;
    };
    let shift = keys.any_pressed([KeyCode::ShiftLeft, KeyCode::ShiftRight]);
    let step = if shift { 0.1 } else { FRAME };
    if keys.just_pressed(KeyCode::Space) {
        scrub.playing = !scrub.playing;
    }
    if keys.just_pressed(KeyCode::ArrowRight) {
        scrub.playing = false;
        cursor.0 = (cursor.0 + step).min(duration);
    }
    if keys.just_pressed(KeyCode::ArrowLeft) {
        scrub.playing = false;
        cursor.0 = (cursor.0 - step).max(0.0);
    }
    if keys.just_pressed(KeyCode::Home) {
        cursor.0 = 0.0;
    }
    if keys.just_pressed(KeyCode::End) {
        cursor.0 = duration;
    }
    if keys.just_pressed(KeyCode::KeyE)
        && let Some(mut clip) = clips.get_mut(&handle)
    {
        add_event_at(&mut clip, cursor.0, &mut selected);
        view.dirty = true;
    }
    if keys.just_pressed(KeyCode::Delete)
        && let (Some(item), Some(track)) = (selected.node, selected.track.clone())
        && let Some(mut clip) = clips.get_mut(&handle)
        && let Some(track) = clip.event_tracks.get_mut(&track)
    {
        track.events.retain(|e| e.id != item);
        selected.node = None;
        selected.dirty = true;
        view.dirty = true;
    }
}

/// A copy of the selected event (same track, name and span) at `time`; with nothing selected, a
/// plain `event` on the first track, or on a new `events` track if the clip has none.
fn add_event_at(clip: &mut GraphClip, time: f32, selected: &mut Selected) {
    let template = selected.track.as_ref().zip(selected.node).and_then(|(track, id)| {
        let item = clip.event_tracks.get(track)?.events.iter().find(|e| e.id == id)?;
        Some((track.clone(), item.value.clone()))
    });
    let (track, mut value) = template.unwrap_or_else(|| {
        let mut names: Vec<&String> = clip.event_tracks.keys().collect();
        names.sort();
        let track = names.first().map_or("events".to_string(), |n| (*n).clone());
        (
            track,
            TrackItemValue {
                event: AnimationEvent::StringId("event".into()),
                start_time: 0.0,
                end_time: NEW_EVENT_SPAN,
            },
        )
    });
    let span = (value.end_time - value.start_time).max(0.0);
    value.start_time = time.clamp(0.0, (clip.duration - span).max(0.0));
    value.end_time = value.start_time + span;
    let item = TrackItem::new(value);
    let id = item.id;
    clip.event_tracks
        .entry(track.clone())
        .or_insert_with(|| EventTrack {
            name: track.clone(),
            events: Vec::new(),
        })
        .add_item_preassigned_id(item);
    selected.node = Some(id);
    selected.track = Some(track);
    selected.dirty = true;
}

/// Run the cursor while playing (looping), and pose the rig at it.
fn advance(
    time: Res<Time>,
    scrub: Res<Scrub>,
    view: Res<CanvasView>,
    clips: Res<Assets<GraphClip>>,
    mut cursor: ResMut<TimelineCursor>,
    mut players: Query<&mut AnimationGraphPlayer, With<Scrubbing>>,
) {
    let Some(duration) = view.clip.as_ref().and_then(|h| clips.get(h)).map(|c| c.duration) else {
        return;
    };
    if scrub.playing && duration > 0.0 {
        cursor.0 = (cursor.0 + time.delta_secs()).rem_euclid(duration);
    }
    if let Some(mut player) = scrub.rig.and_then(|rig| players.get_mut(rig).ok()) {
        if !player.is_paused() {
            player.pause();
        }
        player.seek(cursor.0);
    }
}

fn move_cursor_line(
    cursor: Res<TimelineCursor>,
    view: Res<CanvasView>,
    mut lines: Query<&mut Node, With<CursorLine>>,
) {
    if !cursor.is_changed() && !view.is_changed() {
        return;
    }
    let left = Val::Px(LABEL_W + cursor.0 * PX_PER_SEC * view.zoom + view.pan.x);
    for mut node in &mut lines {
        if node.left != left {
            node.left = left;
        }
    }
}

/// The time under the pointer, on the open timeline.
fn time_under_pointer(
    view: &CanvasView,
    duration: f32,
    windows: &Query<&Window>,
    canvases: &Query<(&ComputedNode, &bevy::ui::UiGlobalTransform), With<Canvas>>,
) -> Option<f32> {
    let x = canvas_cursor(windows, canvases)?.x;
    Some(((x - LABEL_W - view.pan.x) / (PX_PER_SEC * view.zoom)).clamp(0.0, duration))
}

/// Ruler press: the cursor jumps under the pointer.
pub(super) fn ruler_press(
    mut press: On<PointerPress>,
    view: Res<CanvasView>,
    clips: Res<Assets<GraphClip>>,
    windows: Query<&Window>,
    canvases: Query<(&ComputedNode, &bevy::ui::UiGlobalTransform), With<Canvas>>,
    mut cursor: ResMut<TimelineCursor>,
    mut scrub: ResMut<Scrub>,
) {
    press.propagate(false);
    let Some(duration) = view.clip.as_ref().and_then(|h| clips.get(h)).map(|c| c.duration) else {
        return;
    };
    if let Some(t) = time_under_pointer(&view, duration, &windows, &canvases) {
        cursor.0 = t;
        scrub.playing = false;
    }
}

/// Ruler drag: the cursor follows the pointer.
pub(super) fn ruler_drag(
    mut drag: On<PointerDrag>,
    view: Res<CanvasView>,
    clips: Res<Assets<GraphClip>>,
    windows: Query<&Window>,
    canvases: Query<(&ComputedNode, &bevy::ui::UiGlobalTransform), With<Canvas>>,
    mut cursor: ResMut<TimelineCursor>,
) {
    drag.propagate(false);
    let Some(duration) = view.clip.as_ref().and_then(|h| clips.get(h)).map(|c| c.duration) else {
        return;
    };
    if let Some(t) = time_under_pointer(&view, duration, &windows, &canvases) {
        cursor.0 = t;
    }
}
