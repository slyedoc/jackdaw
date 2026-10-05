//! Characters in the editor: an entity with an `AnimationRig` plays its graph in the viewport.
//! The inspector's Animation tab carries a Playback card for it: play/pause, the graph's inputs
//! as sliders, and Edit graph.

use bevy::{
    feathers::{controls::FeathersSlider, theme::ThemedText},
    prelude::*,
    ui_widgets::{SliderValue, ValueChange, slider_self_update},
};
use bevy_animation_graph::core::{
    animation_graph::AnimationGraph, animation_graph_player::AnimationGraphPlayer,
    edge_data::DataValue,
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
        (apply_playback, fill_rig_inputs, show_rig_state),
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
            let max = (value * 2.0).max(4.0);
            let label = commands
                .spawn((Text::new(format!("{name}  {value:.2}")), ThemedText, ChildOf(host)))
                .id();
            let pin = name.clone();
            commands
                .spawn_scene(bsn! {
                    @FeathersSlider { @min: 0.0, @max: {max} }
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
        let status = match (players.get(state.0).is_ok(), playback.paused) {
            (false, _) => "waiting for the rig",
            (true, true) => "paused",
            (true, false) => "playing",
        };
        let shown = format!("{graph}  -  {status}");
        if text.0 != shown {
            text.0 = shown;
        }
    }
}
