//! The Landscape panel: for the selected landscape (or the one a selected layer sits on), its
//! brush tools and the three stacks, top layer first. A row selects its layer, so the
//! inspector edits its parameters; its checkbox turns it on or off, its arrows reorder it
//! within its stack, its cross removes it.

use aurora_landscape::{
    BrushKind, ErosionLayer, GeneratorLayer, HeightLayer, HeightmapLayer, Landscape,
    LandscapePalette, MaterialLayer, MaterialRuleLayer, PaintLayer, ScatterSpecies, SculptLayer,
    SplatmapLayer,
};
use bevy::feathers::controls::FeathersCheckbox;
use bevy::prelude::*;
use bevy::ui::Checked;
use bevy::ui_widgets::{SliderValue, ValueChange};
use jackdaw_api::prelude::*;
use jackdaw_feathers::{
    button::{self, ButtonOperatorCall, ButtonProps, ButtonVariant},
    slider_row::{SliderRowProps, spawn_slider_row},
    tokens,
};

use super::LandscapeBrush;
use super::ops::{
    LandscapeLayerAddOp, LandscapeLayerMoveOp, LandscapeLayerRemoveOp, LandscapeLayerToggleOp,
    LandscapeToolOp, Stack, tool_name,
};
use crate::selection::Selection;

pub(super) fn plugin(app: &mut App) {
    app.add_observer(on_brush_slider)
        .add_observer(on_layer_checkbox);
}

#[derive(Component)]
pub(crate) struct LandscapePanelBody;

/// The panel: one scrollable column the update system fills.
pub(crate) fn landscape_panel_content() -> impl Bundle {
    (
        LandscapePanelBody,
        Node {
            width: percent(100),
            height: percent(100),
            flex_direction: FlexDirection::Column,
            overflow: Overflow::scroll_y(),
            padding: UiRect::all(px(tokens::SPACING_SM)),
            row_gap: px(tokens::SPACING_SM),
            ..default()
        },
        ScrollPosition::default(),
    )
}

/// One row's worth of a layer.
#[derive(Clone, PartialEq, Debug)]
pub(crate) struct LayerRow {
    entity: Entity,
    name: String,
    kind: &'static str,
    stack: Stack,
    enabled: bool,
}

/// What the panel shows; rebuilt only when it changes.
#[derive(Clone, PartialEq, Default, Debug)]
pub(crate) struct PanelState {
    landscape: Option<Entity>,
    selected: Option<Entity>,
    rows: Vec<LayerRow>,
    tool: Option<BrushKind>,
    palette_len: usize,
}

/// Which brush setting a slider drives.
#[derive(Component, Clone, Copy)]
enum BrushField {
    Radius,
    Strength,
    Material,
}

/// A layer row's on / off checkbox.
#[derive(Component, Clone, Copy)]
struct LayerEnabled(Entity);

#[allow(clippy::type_complexity)]
pub(super) fn update_landscape_panel(
    mut commands: Commands,
    selection: Res<Selection>,
    brush: Res<LandscapeBrush>,
    landscape_q: Query<(), With<Landscape>>,
    parents: Query<&ChildOf>,
    children: Query<&Children>,
    palettes: Query<&LandscapePalette>,
    names: Query<&Name>,
    layers: Query<(
        Option<&HeightLayer>,
        Option<&MaterialLayer>,
        Has<ScatterSpecies>,
        Has<HeightmapLayer>,
        Has<SculptLayer>,
        Has<GeneratorLayer>,
        Has<ErosionLayer>,
        Has<MaterialRuleLayer>,
        Has<PaintLayer>,
        Has<SplatmapLayer>,
    )>,
    body: Query<(Entity, Option<&Children>), With<LandscapePanelBody>>,
    mut last: Local<PanelState>,
) {
    let picked = super::selected_landscape(&selection, &landscape_q, &parents);
    let mut state = PanelState {
        landscape: picked.map(|(l, _)| l),
        selected: picked.and_then(|(_, s)| s),
        tool: brush.tool,
        ..default()
    };
    if let Some(landscape) = state.landscape {
        state.palette_len = palettes.get(landscape).map_or(0, |p| p.0.len());
        for child in children.get(landscape).into_iter().flatten() {
            let Ok((
                height,
                material,
                scatter,
                map,
                sculpt,
                generator,
                erosion,
                rule,
                paint,
                splat,
            )) = layers.get(*child)
            else {
                continue;
            };
            let (stack, enabled, kind) = if let Some(h) = height {
                let kind = if map {
                    "heightmap"
                } else if sculpt {
                    "sculpt"
                } else if generator {
                    "generator"
                } else if erosion {
                    "erosion"
                } else {
                    "height"
                };
                (Stack::Height, h.enabled, kind)
            } else if let Some(m) = material {
                let kind = if rule {
                    "rule"
                } else if paint {
                    "paint"
                } else if splat {
                    "splatmap"
                } else {
                    "material"
                };
                (Stack::Material, m.enabled, kind)
            } else if scatter {
                (Stack::Scatter, true, "scatter")
            } else {
                continue;
            };
            state.rows.push(LayerRow {
                entity: *child,
                name: names
                    .get(*child)
                    .map_or_else(|_| kind.to_string(), |n| n.as_str().to_string()),
                kind,
                stack,
                enabled,
            });
        }
    }
    let Ok((body, old)) = body.single() else {
        return;
    };
    if *last == state && old.is_some_and(|c| !c.is_empty()) {
        return;
    }
    *last = state.clone();
    for child in old.into_iter().flatten() {
        commands.entity(*child).despawn();
    }

    let Some(_) = state.landscape else {
        hint(
            &mut commands,
            body,
            "Select a landscape, or add one: Add > Regions > Landscape.",
        );
        return;
    };

    // Brush tools.
    heading(&mut commands, body, "Brush");
    let tools = row(&mut commands, body);
    for (label, tool) in [
        ("Raise", BrushKind::Raise),
        ("Lower", BrushKind::Lower),
        ("Smooth", BrushKind::Smooth),
        ("Flatten", BrushKind::Flatten),
        ("Paint", BrushKind::Paint),
        ("Erase", BrushKind::Erase),
        ("Hole", BrushKind::Hole),
    ] {
        let variant = if state.tool == Some(tool) {
            ButtonVariant::Active
        } else {
            ButtonVariant::Default
        };
        commands.spawn((
            button::button(ButtonProps::new(label).with_variant(variant)),
            ButtonOperatorCall::new(LandscapeToolOp::ID).with_param("tool", tool_name(Some(tool))),
            ChildOf(tools),
        ));
    }
    spawn_slider_row(
        &mut commands,
        body,
        SliderRowProps::new("Radius", brush.radius, 0.5..100.0),
        BrushField::Radius,
    );
    spawn_slider_row(
        &mut commands,
        body,
        SliderRowProps::new("Strength", brush.strength, 0.1..40.0),
        BrushField::Strength,
    );
    if state.palette_len > 1 {
        spawn_slider_row(
            &mut commands,
            body,
            SliderRowProps::new(
                "Paint material",
                brush.material as f32,
                0.0..(state.palette_len - 1) as f32,
            ),
            BrushField::Material,
        );
    }

    for (stack, title, adds) in [
        (
            Stack::Height,
            "Height",
            &[
                ("Stamp", "heightmap"),
                ("Sculpt", "sculpt"),
                ("Generator", "generator"),
                ("Erosion", "erosion"),
            ][..],
        ),
        (
            Stack::Material,
            "Materials",
            &[("Rule", "rule"), ("Paint", "paint")][..],
        ),
        (Stack::Scatter, "Scatter", &[][..]),
    ] {
        heading(&mut commands, body, title);
        if !adds.is_empty() {
            let add_row = row(&mut commands, body);
            for (label, kind) in adds {
                commands.spawn((
                    button::button(ButtonProps::new(format!("+ {label}"))),
                    ButtonOperatorCall::new(LandscapeLayerAddOp::ID).with_param("kind", *kind),
                    ChildOf(add_row),
                ));
            }
        }
        // Top of the stack first: the last child draws over the rest.
        let rows: Vec<&LayerRow> = state.rows.iter().filter(|r| r.stack == stack).collect();
        if rows.is_empty() {
            hint(&mut commands, body, "No layers.");
        }
        for layer in rows.into_iter().rev() {
            layer_row(
                &mut commands,
                body,
                layer,
                state.selected == Some(layer.entity),
            );
        }
    }
}

fn heading(commands: &mut Commands, parent: Entity, text: &str) {
    commands.spawn((
        Text::new(text),
        TextFont {
            font_size: tokens::TEXT_SIZE_SM,
            ..default()
        },
        TextColor(tokens::TEXT_BODY_COLOR.into()),
        Node {
            margin: UiRect::top(px(tokens::SPACING_SM)),
            ..default()
        },
        ChildOf(parent),
    ));
}

fn hint(commands: &mut Commands, parent: Entity, text: &str) {
    commands.spawn((
        Text::new(text),
        TextFont {
            font_size: tokens::TEXT_SIZE_SM,
            ..default()
        },
        TextColor(tokens::TEXT_SECONDARY),
        ChildOf(parent),
    ));
}

fn row(commands: &mut Commands, parent: Entity) -> Entity {
    commands
        .spawn((
            Node {
                width: percent(100),
                flex_direction: FlexDirection::Row,
                flex_wrap: FlexWrap::Wrap,
                column_gap: px(tokens::SPACING_XS),
                row_gap: px(tokens::SPACING_XS),
                ..default()
            },
            ChildOf(parent),
        ))
        .id()
}

fn layer_row(commands: &mut Commands, parent: Entity, layer: &LayerRow, selected: bool) {
    let row = commands
        .spawn((
            Node {
                flex_direction: FlexDirection::Row,
                align_items: AlignItems::Center,
                column_gap: px(tokens::SPACING_SM),
                width: Val::Percent(100.0),
                padding: UiRect::all(px(tokens::SPACING_XS)),
                ..default()
            },
            BackgroundColor(if selected {
                tokens::ACCENT_BLUE
            } else {
                Color::NONE
            }),
            ChildOf(parent),
        ))
        .id();
    if layer.stack != Stack::Scatter {
        let mut cb = commands.spawn_scene(bsn! { @FeathersCheckbox });
        cb.insert((LayerEnabled(layer.entity), ChildOf(row)));
        if layer.enabled {
            cb.insert(Checked);
        }
    }
    // The name gives way (clipped) before the buttons do in a narrow dock.
    let label = commands
        .spawn((
            Node {
                flex_grow: 1.0,
                flex_shrink: 1.0,
                min_width: px(0),
                overflow: Overflow::clip(),
                ..default()
            },
            ChildOf(row),
        ))
        .id();
    commands.spawn((
        Text::new(format!("{}  ({})", layer.name, layer.kind)),
        TextFont {
            font_size: tokens::TEXT_SIZE_SM,
            ..default()
        },
        TextColor(if selected || layer.enabled {
            tokens::TEXT_BODY_COLOR.into()
        } else {
            tokens::TEXT_SECONDARY
        }),
        ChildOf(label),
    ));
    let entity = layer.entity;
    commands.entity(label).observe(
        move |_: On<PointerClick>, mut commands: Commands, mut selection: ResMut<Selection>| {
            selection.select_single(&mut commands, entity);
        },
    );
    let bits = entity.to_bits().to_string();
    for (text, op, dir) in [
        ("^", LandscapeLayerMoveOp::ID, Some("up")),
        ("v", LandscapeLayerMoveOp::ID, Some("down")),
        ("x", LandscapeLayerRemoveOp::ID, None),
    ] {
        if layer.stack == Stack::Scatter && dir.is_some() {
            continue;
        }
        let mut call = ButtonOperatorCall::new(op).with_param("layer", bits.clone());
        if let Some(dir) = dir {
            call = call.with_param("dir", dir);
        }
        commands.spawn((
            button::button(ButtonProps::new(text).with_variant(ButtonVariant::Ghost)),
            call,
            ChildOf(row),
        ));
    }
}

fn on_brush_slider(
    event: On<ValueChange<f32>>,
    fields: Query<&BrushField>,
    mut brush: ResMut<LandscapeBrush>,
    mut commands: Commands,
) {
    let Ok(field) = fields.get(event.event_target()) else {
        return;
    };
    let value = match field {
        BrushField::Radius => {
            brush.radius = event.value;
            event.value
        }
        BrushField::Strength => {
            brush.strength = event.value;
            event.value
        }
        BrushField::Material => {
            brush.material = event.value.round().clamp(0.0, 255.0) as u8;
            brush.material as f32
        }
    };
    commands
        .entity(event.event_target())
        .insert(SliderValue(value));
}

fn on_layer_checkbox(
    event: On<ValueChange<bool>>,
    boxes: Query<&LayerEnabled>,
    mut commands: Commands,
) {
    let target = event.event_target();
    let Ok(&LayerEnabled(layer)) = boxes.get(target) else {
        return;
    };
    jackdaw_feathers::utils::set_marker_if_alive::<Checked>(&mut commands, target, event.value);
    commands
        .operator(LandscapeLayerToggleOp::ID)
        .param("layer", layer.to_bits().to_string())
        .settings(CallOperatorSettings {
            creates_history_entry: false,
            execution_context: ExecutionContext::Invoke,
        })
        .call();
}
