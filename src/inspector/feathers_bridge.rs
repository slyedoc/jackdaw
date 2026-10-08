//! The inspector's fields are `bevy_feathers_inspector`'s; this is what the editor adds to them.
//!
//! - Edits of an inspected component become the editor's field edits: previewed live during a
//!   drag, one undo entry when it ends, applied to every selected entity.
//! - Asset fields get the asset row (pick, clear, new, drop); entity fields a link that selects.
//! - Field rows carry [`InspectorFieldRow`], which the keyframe diamonds hang off.

use std::any::TypeId;

use bevy::feathers_inspector::{
    BuildCx, DefaultInspectorWidgetsPlugin, FieldCtx, InspectorBinding, InspectorEdits, InspectorField, InspectorRoot,
    ReflectInspectorWidget,
};
use bevy::prelude::*;
use bevy::reflect::{ParsedPath, PartialReflect, TypeRegistry};
use bevy::ui_widgets::observe;
use jackdaw_feathers::{icons::IconFont, tokens};

use super::InspectorFieldRow;
use super::asset_row::{AssetFieldTarget, AssetRowProps, spawn_asset_row};
use crate::asset_index::AssetIndex;
use crate::definition_assets::{DefinitionAssetEdit, OpenDefinition};
use crate::selection::Selection;

pub(super) fn plugin(app: &mut App) {
    if !app.is_plugin_added::<DefaultInspectorWidgetsPlugin>() {
        app.add_plugins(DefaultInspectorWidgetsPlugin);
    }
    app.insert_resource(InspectorEdits(route_edit))
        .add_systems(PostStartup, register_widgets)
        .add_systems(Update, (fill_asset_slots, fill_entity_links, tag_field_rows));
}

/// The editor's spelling of a reflection path: no leading dot before a field name.
fn field_path(path: &str) -> &str {
    match path.strip_prefix('.') {
        Some(rest) if !rest.starts_with(|c: char| c.is_ascii_digit()) => rest,
        _ => path,
    }
}

fn type_path_of(registry: &TypeRegistry, type_id: TypeId) -> Option<String> {
    Some(registry.get(type_id)?.type_info().type_path().to_string())
}

fn root_type_path(world: &World, root: &InspectorRoot) -> Option<String> {
    let type_id = match root {
        InspectorRoot::Component { type_id, .. }
        | InspectorRoot::Resource { type_id }
        | InspectorRoot::Asset { type_id, .. } => *type_id,
        _ => return None,
    };
    type_path_of(&world.resource::<AppTypeRegistry>().read(), type_id)
}

/// The `(type_path, field_path)` an inspector field row or leaf widget edits, in the editor's
/// spelling of the path.
pub fn inspected_field(world: &World, entity: Entity) -> Option<(String, String)> {
    if let Some(binding) = world.get::<InspectorBinding>(entity) {
        let path = binding.path.to_string();
        return Some((
            root_type_path(world, binding.root.as_ref()?)?,
            field_path(&path).to_string(),
        ));
    }
    let field = world.get::<InspectorField>(entity)?;
    Some((
        root_type_path(world, field.root.as_ref()?)?,
        field_path(&field.path).to_string(),
    ))
}

/// The leaf widget editing `field_path` of `type_path`, if the inspector shows one.
pub fn field_widget(world: &mut World, type_path: &str, field_path: &str) -> Option<Entity> {
    let widgets: Vec<Entity> = world
        .query_filtered::<Entity, With<InspectorBinding>>()
        .iter(world)
        .collect();
    widgets.into_iter().find(|&widget| {
        inspected_field(world, widget).is_some_and(|(t, f)| t == type_path && f == field_path)
    })
}

/// The field row showing `field_path` of `type_path`, if the inspector shows one.
pub fn field_row(world: &mut World, type_path: &str, field_path: &str) -> Option<Entity> {
    let rows: Vec<Entity> = world
        .query_filtered::<Entity, With<InspectorField>>()
        .iter(world)
        .collect();
    rows.into_iter().find(|&row| {
        inspected_field(world, row).is_some_and(|(t, f)| t == type_path && f == field_path)
    })
}

/// The entity and type path the editor's field edits address for `root`: a component's own,
/// or, for the open definition's asset, the entity that carries it.
fn inspected(world: &World, root: &InspectorRoot) -> Option<(Entity, String)> {
    match root {
        InspectorRoot::Component { entity, type_id } => {
            let registry = world.resource::<AppTypeRegistry>().read();
            Some((*entity, type_path_of(&registry, *type_id)?))
        }
        InspectorRoot::Asset { asset_id, .. } => {
            let open = world.get_resource::<OpenDefinition>()?.0?;
            let edit = world.get::<DefinitionAssetEdit>(open)?;
            let handle = world
                .get_resource::<AssetIndex>()?
                .get(&edit.path)?
                .value
                .handle()?;
            (handle.id() == *asset_id).then(|| (open, edit.type_path.clone()))
        }
        _ => None,
    }
}

/// [`InspectorEdits`]: a component edit goes through the field-edit commands. Anything else (a
/// resource, an asset) is left to the inspector's own write.
fn route_edit(
    world: &mut World,
    root: &InspectorRoot,
    path: &ParsedPath,
    value: &dyn PartialReflect,
    is_final: bool,
) -> bool {
    let Some((entity, type_path)) = inspected(world, root) else {
        return false;
    };
    let json = {
        let registry = world.resource::<AppTypeRegistry>().read();
        let Some(json) = crate::typed_values::reflect_to_json(value, &registry) else {
            warn!("inspector: {type_path}{path} does not serialize");
            return false;
        };
        json
    };
    let path = path.to_string();
    let path = field_path(&path);
    let selected = world.resource::<Selection>().entities.contains(&entity);
    match (selected, is_final) {
        (true, false) => crate::commands::field_edit_preview(world, &type_path, path, &json),
        (true, true) => crate::commands::field_edit_commit(
            world,
            &type_path,
            path,
            &json,
            "Edit field on selection",
        ),
        (false, true) => {
            crate::commands::field_edit_commit_on(world, entity, &type_path, path, &json);
        }
        (false, false) => return false,
    }
    true
}

/// Where an asset row goes, filled in by [`fill_asset_slots`] once it has spawned.
#[derive(Component, Clone, Default)]
struct AssetSlot {
    root: Option<InspectorRoot>,
    field_path: String,
    asset_type_path: String,
    filled: bool,
}

fn build_asset_slot(
    cx: &BuildCx,
    path: &str,
    value: &dyn PartialReflect,
    _field: &FieldCtx,
) -> Box<dyn bevy::scene::Scene> {
    let slot = AssetSlot {
        root: Some(cx.root),
        field_path: field_path(path).to_string(),
        asset_type_path: value
            .get_represented_type_info()
            .and_then(|info| crate::typed_values::asset_type_path(cx.registry, info.type_id()))
            .unwrap_or_default(),
        filled: false,
    };
    Box::new(bsn! {
        Node { flex_grow: 1.0, min_width: px(0) }
        slot
    })
}

/// Every handle-shaped type (a `Handle<T>`, an `Option<Handle<T>>`) gets the asset row, and an
/// `Entity` the link.
fn register_widgets(registry: Res<AppTypeRegistry>) {
    let mut registry = registry.write();
    let handles: Vec<TypeId> = registry
        .iter()
        .map(|registration| registration.type_id())
        .filter(|&id| crate::typed_values::asset_type_path(&registry, id).is_some())
        .collect();
    for id in handles {
        if let Some(registration) = registry.get_mut(id) {
            registration.insert(ReflectInspectorWidget {
                build: build_asset_slot,
            });
        }
    }
    if let Some(registration) = registry.get_mut(TypeId::of::<Entity>()) {
        registration.insert(ReflectInspectorWidget {
            build: build_entity_link,
        });
    }
}

fn fill_asset_slots(world: &mut World) {
    let slots: Vec<(Entity, AssetSlot)> = world
        .query::<(Entity, &AssetSlot)>()
        .iter(world)
        .filter(|(_, slot)| !slot.filled)
        .map(|(entity, slot)| (entity, slot.clone()))
        .collect();
    if slots.is_empty() {
        return;
    }
    let icon_font = world.resource::<IconFont>().0.clone();
    for (entity, slot) in slots {
        world.entity_mut(entity).get_mut::<AssetSlot>().unwrap().filled = true;
        let Some((source, type_path)) = slot.root.and_then(|root| inspected(world, &root)) else {
            continue;
        };
        let mut commands = world.commands();
        spawn_asset_row(
            &mut commands,
            entity,
            AssetRowProps {
                target: AssetFieldTarget::Inspected { source, type_path },
                field_path: slot.field_path,
                asset_type_path: slot.asset_type_path,
                label: String::new(),
                indent: 0,
            },
            &icon_font,
        );
    }
    world.flush();
}

/// An entity field: its target, shown as a link by [`fill_entity_links`].
#[derive(Component, Clone, Default)]
struct EntityLink(Option<Entity>);

fn build_entity_link(
    _cx: &BuildCx,
    _path: &str,
    value: &dyn PartialReflect,
    _field: &FieldCtx,
) -> Box<dyn bevy::scene::Scene> {
    let link = EntityLink(value.try_downcast_ref::<Entity>().copied());
    Box::new(bsn! { Node { flex_grow: 1.0 } link })
}

fn fill_entity_links(
    links: Query<(Entity, &EntityLink), Added<EntityLink>>,
    names: Query<&Name>,
    mut commands: Commands,
) {
    for (entity, EntityLink(target)) in &links {
        let Some(target) = *target else {
            continue;
        };
        let label = names
            .get(target)
            .map(|n| format!("{} ({target})", n.as_str()))
            .unwrap_or_else(|_| format!("{target}"));
        spawn_entity_link(&mut commands, entity, target, &label);
    }
}

/// Give each component field row the marker the keyframe diamonds look for.
fn tag_field_rows(
    rows: Query<(Entity, &InspectorField), Added<InspectorField>>,
    registry: Res<AppTypeRegistry>,
    mut nodes: Query<&mut Node>,
    mut commands: Commands,
) {
    let registry = registry.read();
    for (row, field) in &rows {
        let Some(InspectorRoot::Component { entity, type_id }) = &field.root else {
            continue;
        };
        let Some(type_path) = type_path_of(&registry, *type_id) else {
            continue;
        };
        if let Ok(mut node) = nodes.get_mut(row) {
            node.position_type = PositionType::Relative;
        }
        commands.entity(row).try_insert(InspectorFieldRow {
            source_entity: *entity,
            type_path,
            field_path: field_path(&field.path).to_string(),
        });
    }
}

/// Parse a string value into a reflected value, returning true on success.
fn spawn_entity_link(
    commands: &mut Commands,
    parent: Entity,
    target: Entity,
    label: &str,
) {
    commands.spawn((
        Text::new(label),
        TextFont {
            font_size: tokens::TEXT_SIZE_SM,
            ..Default::default()
        },
        TextColor(tokens::TEXT_ACCENT),
        ChildOf(parent),
        observe(
            move |_: On<PointerClick>, mut commands: Commands, mut selection: ResMut<Selection>| {
                selection.select_single(&mut commands, target);
            },
        ),
        observe(
            move |hover: On<PointerOver>, mut q: Query<&mut TextColor>| {
                if let Ok(mut c) = q.get_mut(hover.event_target()) {
                    c.0 = tokens::TEXT_ACCENT_HOVER;
                }
            },
        ),
        observe(move |out: On<PointerOut>, mut q: Query<&mut TextColor>| {
            if let Ok(mut c) = q.get_mut(out.event_target()) {
                c.0 = tokens::TEXT_ACCENT;
            }
        }),
    ));
}
