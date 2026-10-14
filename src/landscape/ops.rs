//! The Landscape panel's operators: add a landscape, add / remove / reorder / toggle layers,
//! arm a brush tool.

use aurora_landscape::{
    BrushKind, ErosionLayer, GeneratorLayer, HeightBlend, HeightLayer, HeightmapLayer, Landscape,
    MaterialLayer, MaterialRuleLayer, PaintLayer, ScatterSpecies, SculptLayer,
};
use bevy::{ecs::system::SystemState, prelude::*};
use jackdaw_api::prelude::*;

use super::LandscapeBrush;
use crate::commands::{
    CommandHistory, DespawnEntity, EditorCommand, HierarchyLocation, MoveEntity,
};
use crate::selection::Selection;

pub(super) fn add_to_extension(ctx: &mut ExtensionContext) {
    ctx.register_operator::<EntityAddLandscapeOp>();
    ctx.register_operator::<LandscapeLayerAddOp>();
    ctx.register_operator::<LandscapeLayerRemoveOp>();
    ctx.register_operator::<LandscapeLayerMoveOp>();
    ctx.register_operator::<LandscapeLayerToggleOp>();
    ctx.register_operator::<LandscapeToolOp>();
}

/// Which stack a layer entity belongs to (they reorder only among their own).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Stack {
    Height,
    Material,
    Scatter,
}

pub(super) fn stack_of(world: &World, entity: Entity) -> Option<Stack> {
    if world.get::<HeightLayer>(entity).is_some() {
        Some(Stack::Height)
    } else if world.get::<MaterialLayer>(entity).is_some() {
        Some(Stack::Material)
    } else if world.get::<ScatterSpecies>(entity).is_some() {
        Some(Stack::Scatter)
    } else {
        None
    }
}

fn entity_param(params: &OperatorParameters, key: &str) -> Option<Entity> {
    params
        .as_str(key)
        .and_then(|s| s.parse::<u64>().ok())
        .and_then(Entity::try_from_bits)
}

#[operator(
    id = "entity.add.landscape",
    label = "Landscape",
    description = "Add a layered landscape (512 m square) with a sculpt and a paint layer."
)]
pub(crate) fn entity_add_landscape(
    _: In<OperatorParameters>,
    mut commands: Commands,
) -> OperatorResult {
    commands.queue(|world: &mut World| {
        crate::spawn_undoable(world, "Add Landscape", move |world| {
            let mut state: SystemState<(Commands, ResMut<Selection>)> = SystemState::new(world);
            let Ok((mut commands, mut selection)) = state.get_mut(world) else {
                return Entity::PLACEHOLDER;
            };
            let half = Vec2::splat(256.0);
            let landscape = commands
                .spawn((
                    Name::new("Landscape"),
                    Landscape::covering(-half, half, 0.5),
                ))
                .id();
            commands.spawn((
                Name::new("Sculpt"),
                SculptLayer::default(),
                ChildOf(landscape),
            ));
            commands.spawn((
                Name::new("Paint"),
                PaintLayer::default(),
                ChildOf(landscape),
            ));
            selection.select_single(&mut commands, landscape);
            state.apply(world);
            crate::scene_io::adopt_entity(world, landscape);
            crate::open_window_in_default_area_if_absent(world, "jackdaw.inspector.landscape");
            landscape
        });
    });
    OperatorResult::Finished
}

/// A new layer of `kind` with its usual starting values.
fn spawn_layer(commands: &mut Commands, landscape: Entity, kind: &str) -> Option<Entity> {
    let mut layer = commands.spawn(ChildOf(landscape));
    match kind {
        "heightmap" => layer.insert((
            Name::new("Stamp"),
            HeightLayer {
                blend: HeightBlend::Max,
                ..default()
            },
            HeightmapLayer {
                falloff: 0.25,
                ..default()
            },
        )),
        "sculpt" => layer.insert((Name::new("Sculpt"), SculptLayer::default())),
        "generator" => layer.insert((Name::new("Generator"), GeneratorLayer::default())),
        "erosion" => layer.insert((
            Name::new("Erosion"),
            ErosionLayer {
                bake: true,
                ..default()
            },
        )),
        "rule" => layer.insert((Name::new("Rule"), MaterialRuleLayer::default())),
        "paint" => layer.insert((Name::new("Paint"), PaintLayer::default())),
        _ => {
            layer.despawn();
            return None;
        }
    };
    Some(layer.id())
}

#[operator(
    id = "landscape.layer.add",
    label = "Add Layer",
    description = "Add a layer on top of the selected landscape's stack.",
    params(kind(
        String,
        default = "sculpt",
        doc = "heightmap, sculpt, generator, erosion, rule or paint"
    )),
    allows_undo = false
)]
pub(crate) fn landscape_layer_add(
    params: In<OperatorParameters>,
    mut commands: Commands,
) -> OperatorResult {
    let kind = params.as_str("kind").unwrap_or("sculpt").to_string();
    commands.queue(move |world: &mut World| {
        let mut state: SystemState<(Res<Selection>, Query<(), With<Landscape>>, Query<&ChildOf>)> =
            SystemState::new(world);
        let Some((landscape, _)) =
            state
                .get(world)
                .ok()
                .and_then(|(selection, landscapes, parents)| {
                    super::selected_landscape(&selection, &landscapes, &parents)
                })
        else {
            return;
        };
        crate::spawn_undoable(world, "Add Landscape Layer", move |world| {
            let mut state: SystemState<(Commands, ResMut<Selection>)> = SystemState::new(world);
            let Ok((mut commands, mut selection)) = state.get_mut(world) else {
                return Entity::PLACEHOLDER;
            };
            let Some(layer) = spawn_layer(&mut commands, landscape, &kind) else {
                return Entity::PLACEHOLDER;
            };
            selection.select_single(&mut commands, layer);
            state.apply(world);
            crate::scene_io::adopt_entity(world, layer);
            layer
        });
    });
    OperatorResult::Finished
}

#[operator(
    id = "landscape.layer.remove",
    label = "Remove Layer",
    description = "Remove a layer from its landscape.",
    params(layer(String, default = "", doc = "the layer entity, as bits")),
    allows_undo = false
)]
pub(crate) fn landscape_layer_remove(
    params: In<OperatorParameters>,
    mut commands: Commands,
) -> OperatorResult {
    let Some(layer) = entity_param(&params, "layer") else {
        return OperatorResult::Cancelled;
    };
    commands.queue(move |world: &mut World| {
        if world.get_entity(layer).is_err() {
            return;
        }
        let cmd = DespawnEntity::from_world(world, layer);
        world.resource_scope(|world, mut history: Mut<CommandHistory>| {
            history.execute(Box::new(cmd), world);
        });
    });
    OperatorResult::Finished
}

#[operator(
    id = "landscape.layer.move",
    label = "Move Layer",
    description = "Move a layer one place up (later, on top) or down its stack.",
    params(
        layer(String, default = "", doc = "the layer entity, as bits"),
        dir(String, default = "up", doc = "up or down")
    ),
    allows_undo = false
)]
pub(crate) fn landscape_layer_move(
    params: In<OperatorParameters>,
    mut commands: Commands,
) -> OperatorResult {
    let Some(layer) = entity_param(&params, "layer") else {
        return OperatorResult::Cancelled;
    };
    let up = params.as_str("dir") != Some("down");
    commands.queue(move |world: &mut World| {
        let Some(stack) = stack_of(world, layer) else {
            return;
        };
        let Some(parent) = world.get::<ChildOf>(layer).map(ChildOf::parent) else {
            return;
        };
        let Some(children) = world.get::<Children>(parent) else {
            return;
        };
        // The stack's own order; the landscape's other children (tiles) don't count.
        let siblings: Vec<(usize, Entity)> = children
            .iter()
            .enumerate()
            .filter(|&(_, c)| stack_of(world, c) == Some(stack))
            .collect();
        let Some(at) = siblings.iter().position(|&(_, c)| c == layer) else {
            return;
        };
        // "Up" is later in the stack (drawn over what is below).
        let neighbour = if up {
            siblings.get(at + 1)
        } else {
            at.checked_sub(1).and_then(|i| siblings.get(i))
        };
        let Some(&(index, _)) = neighbour else {
            return;
        };
        let cmd = MoveEntity::new(
            world,
            layer,
            HierarchyLocation {
                parent: Some(parent),
                index,
            },
        );
        world.resource_scope(|world, mut history: Mut<CommandHistory>| {
            history.execute(Box::new(cmd), world);
        });
    });
    OperatorResult::Finished
}

/// Flips a layer's `enabled`, undoably.
struct ToggleLayer {
    layer: Entity,
}

impl ToggleLayer {
    fn flip(&self, world: &mut World) {
        if let Some(mut h) = world.get_mut::<HeightLayer>(self.layer) {
            h.enabled = !h.enabled;
        } else if let Some(mut m) = world.get_mut::<MaterialLayer>(self.layer) {
            m.enabled = !m.enabled;
        }
    }
}

impl EditorCommand for ToggleLayer {
    fn execute(&mut self, world: &mut World) {
        self.flip(world);
    }

    fn undo(&mut self, world: &mut World) {
        self.flip(world);
    }

    fn description(&self) -> &str {
        "Toggle Landscape Layer"
    }
}

#[operator(
    id = "landscape.layer.toggle",
    label = "Toggle Layer",
    description = "Turn a layer on or off.",
    params(layer(String, default = "", doc = "the layer entity, as bits")),
    allows_undo = false
)]
pub(crate) fn landscape_layer_toggle(
    params: In<OperatorParameters>,
    mut commands: Commands,
) -> OperatorResult {
    let Some(layer) = entity_param(&params, "layer") else {
        return OperatorResult::Cancelled;
    };
    commands.queue(move |world: &mut World| {
        world.resource_scope(|world, mut history: Mut<CommandHistory>| {
            history.execute(Box::new(ToggleLayer { layer }), world);
        });
    });
    OperatorResult::Finished
}

pub(super) fn tool_name(tool: Option<BrushKind>) -> &'static str {
    match tool {
        None => "none",
        Some(BrushKind::Raise) => "raise",
        Some(BrushKind::Lower) => "lower",
        Some(BrushKind::Smooth) => "smooth",
        Some(BrushKind::Flatten) => "flatten",
        Some(BrushKind::Paint) => "paint",
        Some(BrushKind::Erase) => "erase",
        Some(BrushKind::Hole) => "hole",
    }
}

#[operator(
    id = "landscape.tool",
    label = "Landscape Tool",
    description = "Arm a landscape brush (again to disarm).",
    params(tool(
        String,
        default = "none",
        doc = "raise, lower, smooth, flatten, paint, erase or none"
    )),
    allows_undo = false
)]
pub(crate) fn landscape_tool(
    params: In<OperatorParameters>,
    mut brush: ResMut<LandscapeBrush>,
) -> OperatorResult {
    let tool = match params.as_str("tool").unwrap_or("none") {
        "raise" => Some(BrushKind::Raise),
        "lower" => Some(BrushKind::Lower),
        "smooth" => Some(BrushKind::Smooth),
        "flatten" => Some(BrushKind::Flatten),
        "paint" => Some(BrushKind::Paint),
        "erase" => Some(BrushKind::Erase),
        "hole" => Some(BrushKind::Hole),
        _ => None,
    };
    brush.tool = if brush.tool == tool { None } else { tool };
    OperatorResult::Finished
}
