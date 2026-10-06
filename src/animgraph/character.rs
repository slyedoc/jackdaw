//! Characters in the editor: an entity with an `AnimationRig` plays its graph in the viewport.
//! The inspector's Animation tab carries a Playback card for it: play/pause, the graph's inputs
//! as sliders, a button per state of its state machine, and Edit graph.

use bevy::{
    feathers::{controls::FeathersSlider, theme::ThemedText},
    prelude::*,
    ui_widgets::{SliderValue, ValueChange, slider_self_update},
};
use bevy_animation_graph::{
    builtin_nodes::fsm_node::FsmNode,
    core::{
        animation_graph::AnimationGraph,
        animation_graph_player::AnimationGraphPlayer,
        edge_data::{DataValue, events::AnimationEvent},
        state_machine::high_level::StateMachine,
    },
};
use jackdaw_animation_runtime::AnimationRig;
use jackdaw_api::prelude::*;
use jackdaw_feathers::button::{ButtonOperatorCall, ButtonProps, button};

/// Whether characters play in the editor. Editor-local: not saved, not undone.
#[derive(Resource, Default)]
pub(crate) struct RigPlayback {
    pub paused: bool,
}

pub(crate) fn plugin(app: &mut App) {
    app.init_resource::<RigPlayback>().add_systems(
        Update,
        (apply_playback, fill_rig_inputs, fill_rig_states, show_rig_state),
    );
}

/// Hold every character's player to the play/pause state, including ones armed since.
fn apply_playback(playback: Res<RigPlayback>, mut players: Query<&mut AnimationGraphPlayer>) {
    for mut player in &mut players {
        if player.is_paused() != playback.paused {
            if playback.paused {
                player.pause();
            } else {
                player.resume();
            }
        }
    }
}

/// Play or pause every character in the editor.
#[operator(
    id = "animgraph.toggle_playback",
    label = "Play / Pause Characters",
    description = "Play or pause every character's animation in the editor.",
    allows_undo = false
)]
pub fn animgraph_toggle_playback(
    _: In<OperatorParameters>,
    mut playback: ResMut<RigPlayback>,
) -> OperatorResult {
    playback.paused = !playback.paused;
    OperatorResult::Finished
}

/// Open the selected character's animation graph.
#[operator(
    id = "animgraph.edit_graph",
    label = "Edit Graph",
    description = "Open the animation graph the selected character plays.",
    allows_undo = false
)]
pub fn animgraph_edit_graph(_: In<OperatorParameters>, mut commands: Commands) -> OperatorResult {
    commands.queue(|world: &mut World| {
        let Some(entity) = world.resource::<crate::selection::Selection>().primary() else {
            return;
        };
        let Some(path) = world
            .get::<AnimationRig>(entity)
            .and_then(|rig| rig.graph.path())
            .map(|path| path.path().to_path_buf())
        else {
            warn!("animgraph.edit_graph: the selection is not a character with a graph");
            return;
        };
        let Some(assets) = world.resource::<super::AnimGraphRoot>().0.clone() else {
            return;
        };
        super::open_graph(world, &assets.join(path));
    });
    OperatorResult::Finished
}

/// Ask the selected character's state machine for a state.
#[operator(
    id = "animgraph.play_state",
    label = "Play State",
    description = "Ask the selected character's state machine to go to a state.",
    allows_undo = false,
    params(state(String, doc = "Label of the state to go to."))
)]
pub fn animgraph_play_state(
    params: In<OperatorParameters>,
    selection: Res<crate::selection::Selection>,
    mut players: Query<&mut AnimationGraphPlayer>,
) -> OperatorResult {
    let Some(state) = params.as_str("state") else {
        return OperatorResult::Cancelled;
    };
    let Some(mut player) = selection.primary().and_then(|e| players.get_mut(e).ok()) else {
        warn!("animgraph.play_state: the selection is not a playing character");
        return OperatorResult::Cancelled;
    };
    player.send_event(AnimationEvent::TransitionToStateLabel(state.to_string()));
    OperatorResult::Finished
}

/// The Playback card's state buttons, for the character it was built for.
#[derive(Component)]
struct RigStatesHost {
    character: Entity,
    built: bool,
}

/// The Playback card's slider column, for the character it was built for.
#[derive(Component)]
struct RigInputsHost {
    character: Entity,
    built: bool,
}

/// The Playback card's status line.
#[derive(Component)]
struct RigStateText(Entity);

/// The Animation tab's Playback card, for a character.
pub(crate) fn inject_animation_card(
    commands: &mut Commands,
    character: Entity,
    inspector: Entity,
    icon_font: &Handle<Font>,
    collapsed: bool,
) {
    let body = crate::inspector::material_card_routing::spawn_material_card_shell(
        commands,
        inspector,
        "Playback",
        jackdaw_feathers::icons::Icon::Play,
        "animation_card::playback",
        icon_font,
        collapsed,
    );
    let row = commands
        .spawn((
            Node {
                flex_direction: FlexDirection::Row,
                column_gap: Val::Px(6.0),
                ..default()
            },
            ChildOf(body),
        ))
        .id();
    commands.spawn((
        button(ButtonProps::new("Play / Pause")),
        ButtonOperatorCall::new("animgraph.toggle_playback"),
        ChildOf(row),
    ));
    commands.spawn((
        button(ButtonProps::new("Edit Graph")),
        ButtonOperatorCall::new("animgraph.edit_graph"),
        ChildOf(row),
    ));
    commands.spawn((
        Text::new(String::new()),
        ThemedText,
        RigStateText(character),
        ChildOf(body),
    ));
    commands.spawn((
        Node {
            flex_direction: FlexDirection::Column,
            row_gap: Val::Px(4.0),
            ..default()
        },
        RigInputsHost {
            character,
            built: false,
        },
        ChildOf(body),
    ));
    commands.spawn((
        Node {
            flex_direction: FlexDirection::Row,
            flex_wrap: FlexWrap::Wrap,
            column_gap: Val::Px(4.0),
            row_gap: Val::Px(4.0),
            ..default()
        },
        RigStatesHost {
            character,
            built: false,
        },
        ChildOf(body),
    ));
}

/// One button per state of every state machine the character's graph runs.
fn fill_rig_states(
    mut commands: Commands,
    mut hosts: Query<(Entity, &mut RigStatesHost)>,
    rigs: Query<&AnimationRig>,
    graphs: Res<Assets<AnimationGraph>>,
    fsms: Res<Assets<StateMachine>>,
) {
    for (host, mut states) in &mut hosts {
        if states.built {
            continue;
        }
        let Some(graph) = rigs
            .get(states.character)
            .ok()
            .and_then(|rig| graphs.get(&rig.graph))
        else {
            continue;
        };
        let machines: Vec<&FsmNode> = graph
            .nodes
            .values()
            .filter_map(|node| node.try_inner_downcast_ref::<FsmNode>())
            .collect();
        let Some(loaded) = machines
            .iter()
            .map(|node| fsms.get(&node.fsm))
            .collect::<Option<Vec<_>>>()
        else {
            continue; // a machine is still loading
        };
        states.built = true;
        let mut labels: Vec<&str> = loaded
            .iter()
            .flat_map(|fsm| fsm.states.values().map(|state| state.label.as_str()))
            .collect();
        labels.sort_unstable();
        labels.dedup();
        for label in labels {
            commands.spawn((
                button(ButtonProps::new(label.to_string())),
                ButtonOperatorCall::new("animgraph.play_state").with_param("state", label.to_string()),
                ChildOf(host),
            ));
        }
    }
}

/// One slider per number input of the character's graph, driving its player.
fn fill_rig_inputs(
    mut commands: Commands,
    mut hosts: Query<(Entity, &mut RigInputsHost)>,
    players: Query<&AnimationGraphPlayer>,
    rigs: Query<&AnimationRig>,
    graphs: Res<Assets<AnimationGraph>>,
) {
    for (host, mut inputs) in &mut hosts {
        if inputs.built || players.get(inputs.character).is_err() {
            continue;
        }
        let Some(graph) = rigs
            .get(inputs.character)
            .ok()
            .and_then(|rig| graphs.get(&rig.graph))
        else {
            continue;
        };
        inputs.built = true;
        let mut values: Vec<(String, f32)> = graph
            .default_data
            .iter()
            .filter_map(|(pin, value)| match value {
                DataValue::F32(v) => Some((super::pin_label(pin), *v)),
                _ => None,
            })
            .collect();
        values.sort_by(|a, b| a.0.cmp(&b.0));
        let character = inputs.character;
        for (name, value) in values {
            // Signed: a direction input (strafe, backwards) runs both ways.
            let max = (value.abs() * 2.0).max(9.0);
            let label = commands
                .spawn((Text::new(format!("{name}  {value:.2}")), ThemedText, ChildOf(host)))
                .id();
            let pin = name.clone();
            commands
                .spawn_scene(bsn! {
                    @FeathersSlider { @min: {-max}, @max: {max} }
                    SliderValue({value})
                    on(slider_self_update)
                })
                .insert(ChildOf(host))
                .observe(
                    move |change: On<ValueChange<f32>>,
                          mut players: Query<&mut AnimationGraphPlayer>,
                          mut texts: Query<&mut Text>| {
                        if let Ok(mut player) = players.get_mut(character) {
                            player.set_input_data(pin.clone(), DataValue::F32(change.value));
                        }
                        if let Ok(mut text) = texts.get_mut(label) {
                            text.0 = format!("{pin}  {:.2}", change.value);
                        }
                    },
                );
        }
    }
}

/// Keep the status line saying what the character is doing.
fn show_rig_state(
    playback: Res<RigPlayback>,
    rigs: Query<&AnimationRig>,
    players: Query<&AnimationGraphPlayer>,
    mut texts: Query<(&mut Text, &RigStateText)>,
) {
    for (mut text, state) in &mut texts {
        let graph = rigs
            .get(state.0)
            .ok()
            .and_then(|rig| rig.graph.path().map(|p| p.to_string()))
            .unwrap_or_else(|| "no graph".into());
        let player = players.get(state.0).ok();
        let status = match (player, playback.paused) {
            (None, _) => "waiting for the rig".to_string(),
            (Some(player), _) if let Some(err) = player.get_error() => format!("error: {err:?}"),
            (Some(_), true) => "paused".to_string(),
            (Some(_), false) => "playing".to_string(),
        };
        let shown = format!("{graph}  -  {status}");
        if text.0 != shown {
            if status.starts_with("error") {
                warn!("{shown}");
            }
            text.0 = shown;
        }
    }
}
