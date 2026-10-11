//! The inspector card for the open definition asset.
//!
//! The card is the generic component card with the definition's reflected
//! fields in its body, so scalars, enum menus and list rows behave exactly as
//! they do on a component. Its header carries the Save action and says when
//! the definition has unsaved edits, and the card opens expanded, since it is
//! all the panel has to show.

use bevy::ecs::system::SystemState;
use bevy::feathers_inspector::InspectorRoot;
use bevy::prelude::*;
use jackdaw_api::op::Operator as _;
use jackdaw_api::prelude::AssetKinds;
use jackdaw_feathers::{
    button::{ButtonOperatorCall, ButtonProps, ButtonSize, ButtonVariant, button},
    icons::{EditorFont, IconFont},
    tokens,
};
use jackdaw_widgets::collapsible::CollapsibleHeader;

use crate::definition_assets::{AssetSaveOp, DefinitionAssetEdit};

use super::component_display::{ComponentDisplaySpec, spawn_component_display};

/// Marks the card a definition puts up: it stands for a whole file, so no
/// category tab files it or hides it.
#[derive(Component)]
pub(crate) struct DefinitionCard;

/// Drop the card the inspector already holds for a definition, so filling it
/// twice in a frame leaves one card rather than two.
fn despawn_existing_card(world: &mut World, inspector: Entity) {
    let Some(children) = world.get::<Children>(inspector) else {
        return;
    };
    let cards: Vec<Entity> = children
        .iter()
        .filter(|&child| world.get::<DefinitionCard>(child).is_some())
        .collect();
    for card in cards {
        if let Ok(entity) = world.get_entity_mut(card) {
            entity.despawn();
        }
    }
}

/// Build the card for the definition `source` is editing under `inspector`.
pub(crate) fn fill_definition_card(world: &mut World, inspector: Entity, source: Entity) {
    despawn_existing_card(world, inspector);
    let Some((kind, name, type_path, path, dirty)) =
        world.get::<DefinitionAssetEdit>(source).map(|edit| {
            (
                edit.kind.clone(),
                edit.name.clone(),
                edit.type_path.clone(),
                edit.path.clone(),
                edit.dirty,
            )
        })
    else {
        return;
    };
    let registered = world
        .get_resource::<AssetKinds>()
        .and_then(|types| types.by_kind(&kind))
        .cloned();
    let label = registered
        .as_ref()
        .map_or_else(|| kind.clone(), |definition| definition.label.clone());
    let Some(root) = definition_root(world, &path, &type_path) else {
        bevy::log::warn_once!(
            "the editor has no registration for {type_path}, which {kind} files hold"
        );
        return;
    };

    let icon_font = world.resource::<IconFont>().0.clone();
    let editor_font = world.resource::<EditorFont>().0.clone();
    let mut collapse_state =
        super::InspectorCollapseState(world.resource::<super::InspectorCollapseState>().0.clone());

    let card_name = format!("{name} ({label})");
    collapse_state.0.entry(card_name.clone()).or_insert(false);
    let card = {
        let mut state: SystemState<Commands> = SystemState::new(world);
        let Ok(mut commands) = state.get_mut(world) else {
            return;
        };
        let card = spawn_component_display(
            &mut commands,
            ComponentDisplaySpec {
                name: &card_name,
                type_path: &type_path,
                entity: source,
                is_overridden: false,
                is_derived: false,
                removable: false,
                icon_font: &icon_font,
                editor_font: &editor_font,
                collapse_state: &collapse_state,
            },
        );
        commands.entity(card.section).insert(DefinitionCard);
        jackdaw_feathers::utils::attach_or_despawn(&mut commands, inspector, card.section);
        state.apply(world);
        card
    };

    bevy::feathers_inspector::build_body(world, root, card.body);

    let file = crate::asset_index::absolute_path(world, &path);
    if let Some(references) = super::file_card::spawn_references(world, inspector, &file) {
        world.entity_mut(references).insert(DefinitionCard);
    }
    spawn_save_action(world, card.section, source, dirty);
}

/// The definition's asset, as the inspector addresses it.
fn definition_root(
    world: &World,
    path: &std::path::Path,
    type_path: &str,
) -> Option<InspectorRoot> {
    let handle = world
        .get_resource::<crate::asset_index::AssetIndex>()?
        .get(path)?
        .value
        .handle()?;
    Some(InspectorRoot::Asset {
        asset_id: handle.id(),
        type_id: crate::definition_assets::registered_type_id(world, type_path)?,
    })
}

/// The header's unsaved-edits marker, following the definition it names.
#[derive(Component)]
pub(crate) struct UnsavedMarker(Entity);

/// Show the marker exactly while the definition it names has unsaved edits.
pub(crate) fn keep_unsaved_marker_in_step(
    mut markers: Query<(&UnsavedMarker, &mut Node)>,
    edits: Query<&DefinitionAssetEdit>,
) {
    for (marker, mut node) in &mut markers {
        let display = if edits.get(marker.0).is_ok_and(|edit| edit.dirty) {
            Display::Flex
        } else {
            Display::None
        };
        if node.display != display {
            node.display = display;
        }
    }
}

/// The header's broken-reference marker, which says how many of the card's
/// rows name a file the project does not hold.
#[derive(Component)]
pub(crate) struct BrokenReferencesMarker;

/// Keep the marker naming what the card's rows currently name. A field is
/// counted once however many rows stand for it.
pub(crate) fn keep_broken_marker_in_step(
    mut markers: Query<(&mut Text, &mut Node), With<BrokenReferencesMarker>>,
    broken: Query<
        &crate::inspector::asset_row::AssetFieldRow,
        With<crate::inspector::asset_row::AssetRowBroken>,
    >,
) {
    let fields: std::collections::BTreeSet<&str> =
        broken.iter().map(|row| row.field_path.as_str()).collect();
    let broken = fields.len();
    let wanted = match broken {
        0 => String::new(),
        1 => "1 broken reference".to_string(),
        many => format!("{many} broken references"),
    };
    for (mut text, mut node) in &mut markers {
        let display = match broken {
            0 => Display::None,
            _ => Display::Flex,
        };
        if node.display != display {
            node.display = display;
        }
        if text.0 != wanted {
            text.0 = wanted.clone();
        }
    }
}

/// Put Save, and the unsaved-edits marker, in the card's header.
fn spawn_save_action(world: &mut World, section: Entity, source: Entity, dirty: bool) {
    let Some(header) = world.get::<Children>(section).and_then(|children| {
        children
            .iter()
            .find(|&child| world.get::<CollapsibleHeader>(child).is_some())
    }) else {
        return;
    };
    let row = world
        .spawn((
            Node {
                flex_direction: FlexDirection::Row,
                align_items: AlignItems::Center,
                column_gap: Val::Px(tokens::SPACING_XS),
                ..default()
            },
            ChildOf(header),
        ))
        .id();
    world.spawn((
        Text::new("Unsaved"),
        TextFont {
            font_size: tokens::TEXT_SIZE_XS,
            ..default()
        },
        TextColor(tokens::TEXT_SECONDARY),
        Node {
            display: if dirty { Display::Flex } else { Display::None },
            ..default()
        },
        UnsavedMarker(source),
        ChildOf(row),
    ));
    world.spawn((
        Text::default(),
        TextFont {
            font_size: tokens::TEXT_SIZE_XS,
            ..default()
        },
        TextColor(tokens::TEXT_ERROR),
        Node {
            display: Display::None,
            ..default()
        },
        BrokenReferencesMarker,
        ChildOf(row),
    ));
    let save = world
        .spawn((
            button(
                ButtonProps::new("Save")
                    .with_variant(ButtonVariant::Default)
                    .with_size(ButtonSize::MD),
            ),
            ButtonOperatorCall::new(AssetSaveOp::ID),
        ))
        .id();
    world.entity_mut(save).insert(ChildOf(row));
}
