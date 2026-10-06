//! Inspector operators: per-component buttons (add / remove / revert)
//! and the small set of typed actions (`physics.enable` / `physics.disable`,
//! `animation.toggle_keyframe`).

use bevy::ecs::component::ComponentId;
use bevy::ecs::reflect::{AppTypeRegistry, ReflectComponent};
use bevy::prelude::*;
use jackdaw_api::prelude::*;

use super::physics_display::{DisablePhysics, enable_physics};
use crate::commands::{AddComponent, CommandHistory, EditorCommand, RemoveComponent};
use crate::selection::Selection;

pub(crate) fn add_to_extension(ctx: &mut ExtensionContext) {
    ctx.register_operator::<ComponentAddOp>()
        .register_operator::<ComponentRemoveOp>()
        .register_operator::<PhysicsEnableOp>()
        .register_operator::<PhysicsDisableOp>()
        .register_operator::<AnimationToggleKeyframeOp>()
        .register_operator::<FieldSetOp>()
        .register_operator::<super::bindings_card::BindingAddOp>()
        .register_operator::<super::bindings_card::BindingSetOp>()
        .register_operator::<super::brush_display::BrushFaceClearMaterialOp>()
        .register_operator::<super::brush_display::BrushFaceApplyTextureToAllOp>()
        .register_operator::<super::brush_display::BrushFaceSetUvScalePresetOp>()
        .register_operator::<super::brush_display::BrushClearAllMaterialsOp>()
        .register_operator::<InspectorCategoryOp>()
        .register_operator::<InspectorCardOp>();
}

/// Open or close one inspector card, writing the same collapse state the
/// header's disclosure toggle writes. A name no card carries still applies:
/// the entry waits until a selection brings that card up.
#[operator(
    id = "inspector.card",
    label = "Open Inspector Card",
    description = "Open or close the inspector card with this name.",
    allows_undo = false,
    params(
        name(String, doc = "Card name as the inspector shows it, e.g. \"Node\"."),
        open(bool, doc = "`true` opens the card, `false` collapses it.")
    )
)]
pub(crate) fn inspector_card(
    params: In<OperatorParameters>,
    mut collapse: ResMut<super::InspectorCollapseState>,
    selection: Res<Selection>,
    mut commands: Commands,
) -> OperatorResult {
    let Some(name) = params.as_str("name").filter(|name| !name.is_empty()) else {
        warn!("inspector.card: missing 'name' parameter");
        return OperatorResult::Cancelled;
    };
    let open = params.as_bool("open").unwrap_or(true);
    collapse.0.insert(name.to_string(), !open);
    if let Some(primary) = selection.primary() {
        commands.entity(primary).insert(super::InspectorDirty);
    }
    OperatorResult::Finished
}

/// Show one of the inspector's category tabs.
///
/// The strip's own tabs write the same resource, so a click and a scripted
/// call take the same path. An id no tab exists for still applies: the strip
/// resolves back to an applicable category on its next rebuild.
#[operator(
    id = "inspector.category",
    label = "Inspector Category",
    description = "Show one of the inspector's category tabs.",
    allows_undo = false,
    params(category(
        String,
        default = "object",
        doc = "Category id, e.g. \"object\", \"material\"."
    ))
)]
pub(crate) fn inspector_category(
    params: In<OperatorParameters>,
    mut active: ResMut<super::category_strip::ActiveInspectorCategory>,
) -> OperatorResult {
    let category = params.as_str("category").unwrap_or("object").to_string();
    active.0 = std::borrow::Cow::Owned(category);
    OperatorResult::Finished
}

/// Inspector operators all act on the inspected entity (the primary
/// selection). Buttons that dispatch them get greyed out when nothing
/// is selected.
pub(crate) fn has_primary_selection(selection: Res<Selection>) -> bool {
    selection.primary().is_some()
}

/// Look up `(ComponentId, TypeId)` for a type path, registering
/// the component on a throwaway entity first if the world hasn't
/// seen it yet. Without this, types only `register_type`'d
/// (never inserted) would return `None` from
/// `world.components().get_id` and the picker would silently
/// no-op on click.
fn component_id_for_path(
    world: &mut World,
    type_path: &str,
) -> Option<(ComponentId, std::any::TypeId)> {
    let registry = world.resource::<AppTypeRegistry>().clone();
    let type_id = {
        let registry_read = registry.read();
        let registration = registry_read.get_with_type_path(type_path)?;
        registration.type_id()
    };

    // Fast path: the world already knows about this component.
    if let Some(component_id) = world.components().get_id(type_id) {
        return Some((component_id, type_id));
    }

    // Slow path: insert on a throwaway entity to auto-register
    // the ComponentId. `build_reflective_default` covers types
    // without `#[derive(Default)]`.
    let (reflect_component, default_value) = {
        let registry_read = registry.read();
        let reflect_component = registry_read
            .get_with_type_path(type_path)?
            .data::<ReflectComponent>()?
            .clone();
        let default_value =
            crate::reflect_default::build_reflective_default(type_id, &registry_read)?;
        (reflect_component, default_value)
    };

    let temp = world.spawn_empty().id();
    {
        let registry_read = registry.read();
        reflect_component.insert(
            &mut world.entity_mut(temp),
            default_value.as_partial_reflect(),
            &registry_read,
        );
    }
    let component_id = world.components().get_id(type_id);
    world.despawn(temp);
    component_id.map(|id| (id, type_id))
}

/// Add a component to the target entity. Pushes a single undoable
/// history entry that recreates the component on undo.
#[operator(
    id = "component.add",
    label = "Add Component",
    description = "Add a component to the selected entity.",
    is_available = has_primary_selection,
    params(
        entity(Entity, doc = "Entity that receives the component."),
        type_path(String, doc = "Fully-qualified Bevy reflected type path of the component to add."),
    ),
)]
pub(crate) fn component_add(
    params: In<OperatorParameters>,
    mut commands: Commands,
) -> OperatorResult {
    let entity = params.as_entity("entity")?;
    let type_path = params.as_str("type_path").map(str::to_string)?;
    commands.queue(move |world: &mut World| {
        if world.get_entity(entity).is_err() {
            return;
        }
        let Some((component_id, type_id)) = component_id_for_path(world, &type_path) else {
            warn!(
                "component.add: no registration for type_path '{type_path}'. \
                 Make sure your plugin calls `register_type::<T>()` and that `T` \
                 derives `Reflect, Default` with `#[reflect(Component, Default)]`."
            );
            return;
        };
        let mut cmd = AddComponent::new(entity, type_id, component_id, type_path);
        cmd.execute(world);
        world
            .resource_mut::<CommandHistory>()
            .push_executed(Box::new(cmd));
        if let Ok(mut ec) = world.get_entity_mut(entity) {
            ec.insert(super::InspectorDirty);
        }
    });
    OperatorResult::Finished
}

/// Remove a component from the target entity.
#[operator(
    id = "component.remove",
    label = "Remove Component",
    description = "Remove a component from the selected entity.",
    is_available = has_primary_selection,
    params(
        entity(Entity, doc = "Entity that loses the component."),
        type_path(String, doc = "Fully-qualified Bevy reflected type path of the component to remove."),
    ),
)]
pub(crate) fn component_remove(
    params: In<OperatorParameters>,
    mut commands: Commands,
) -> OperatorResult {
    let entity = params.as_entity("entity")?;
    let type_path = params.as_str("type_path").map(str::to_string)?;
    commands.queue(move |world: &mut World| {
        // A running preview owns the components its evaluator writes.
        if crate::preview_context::preview_writes_type_path(world, entity, &type_path) {
            warn!(
                "{}: `{type_path}` on {entity}",
                crate::preview_context::PREVIEW_EDIT_REFUSED
            );
            return;
        }
        let Some((component_id, type_id)) = component_id_for_path(world, &type_path) else {
            return;
        };
        let mut cmd: Box<dyn EditorCommand> = Box::new(RemoveComponent::from_world(
            world,
            entity,
            type_id,
            component_id,
            type_path,
        ));
        cmd.execute(world);
        world.resource_mut::<CommandHistory>().push_executed(cmd);
    });
    OperatorResult::Finished
}

/// Read a parameter as the JSON a field edit commits. A string is parsed as
/// JSON first, so `{"Px": 120}` arrives as the enum variant it spells, and
/// falls back to being the string it is.
fn param_json(params: &OperatorParameters, key: &str) -> Option<serde_json::Value> {
    use jackdaw_scene_types::PropertyValue;
    match params.get(key)? {
        PropertyValue::Bool(value) => Some(serde_json::Value::Bool(*value)),
        PropertyValue::Int(value) => Some(serde_json::json!(value)),
        PropertyValue::Float(value) => Some(serde_json::json!(value)),
        PropertyValue::String(value) => Some(
            serde_json::from_str(value)
                .unwrap_or_else(|_| serde_json::Value::String(value.to_string())),
        ),
        _ => None,
    }
}

/// Set one field of one component, through the same commit the inspector's own
/// rows use.
///
/// The commit acts on the selection, so a target outside it replaces the
/// selection; a multi-entity selection takes the edit on every member as one
/// undo entry.
#[operator(
    id = "field.set",
    label = "Set Field",
    description = "Set one field on a component of the selected entity.",
    allows_undo = false,
    is_available = has_primary_selection,
    params(
        entity(Entity, doc = "Entity whose component is edited."),
        type_path(String, doc = "Fully-qualified Bevy reflected type path of the component that owns the field."),
        field(String, doc = "Dotted path to the field within the component (e.g. \"width\")."),
        value(String, doc = "New value as JSON: 12, true, text, or {\"Px\": 12}."),
    ),
)]
pub(crate) fn field_set(params: In<OperatorParameters>, mut commands: Commands) -> OperatorResult {
    let entity = params.as_entity("entity")?;
    let type_path = params.as_str("type_path").map(str::to_string)?;
    let field_path = params.as_str("field").map(str::to_string)?;
    let value = param_json(&params, "value")?;
    commands.queue(move |world: &mut World| {
        crate::selection::select_for_edit(world, entity);
        crate::commands::field_edit_commit(
            world,
            &type_path,
            &field_path,
            &value,
            "Set field on multiple entities",
        );
    });
    OperatorResult::Finished
}

/// Restore an overridden component on a prefab instance to the prefab's
/// baseline value.
/// Add `RigidBody` and `AvianCollider` to the entity so it participates
/// in the physics simulation. No-op if those components are already
/// present.
#[operator(
    id = "physics.enable",
    label = "Enable Physics",
    description = "Make the selected entity participate in the physics simulation.",
    is_available = has_primary_selection,
    params(entity(Entity, doc = "Entity to make physical.")),
)]
pub(crate) fn physics_enable(
    params: In<OperatorParameters>,
    mut commands: Commands,
) -> OperatorResult {
    let entity = params.as_entity("entity")?;
    commands.queue(move |world: &mut World| {
        enable_physics(world, entity);
        if let Ok(mut ec) = world.get_entity_mut(entity) {
            ec.insert(super::InspectorDirty);
        }
    });
    OperatorResult::Finished
}

/// Remove physics components from the entity, capturing the pre-disable
/// state so undo restores them.
#[operator(
    id = "physics.disable",
    label = "Disable Physics",
    description = "Stop the selected entity from participating in the physics simulation.",
    is_available = has_primary_selection,
    params(entity(Entity, doc = "Entity to make non-physical.")),
)]
pub(crate) fn physics_disable(
    params: In<OperatorParameters>,
    mut commands: Commands,
) -> OperatorResult {
    let entity = params.as_entity("entity")?;
    commands.queue(move |world: &mut World| {
        // A running preview owns the components its evaluator writes.
        if let Some(owned) = super::physics_display::preview_owned_physics(world, entity) {
            warn!(
                "{}: `{owned}` on {entity}",
                crate::preview_context::PREVIEW_EDIT_REFUSED
            );
            return;
        }
        let mut cmd: Box<dyn EditorCommand> = Box::new(DisablePhysics::from_world(world, entity));
        cmd.execute(world);
        world.resource_mut::<CommandHistory>().push_executed(cmd);
        if let Ok(mut ec) = world.get_entity_mut(entity) {
            ec.insert(super::InspectorDirty);
        }
    });
    OperatorResult::Finished
}

/// Spawn (or replace) a keyframe at the current timeline cursor for one
/// of the entity's animatable properties. Creates the clip and track
/// lazily if they don't exist yet.
#[operator(
    id = "animation.toggle_keyframe",
    label = "Toggle Keyframe",
    description = "Add or replace a keyframe for this property at the current timeline cursor.",
    is_available = has_primary_selection,
    params(
        entity(Entity, doc = "Source entity whose property is being animated."),
        component_type_path(String, doc = "Fully-qualified Bevy reflected type path of the component that owns the property."),
        field_path(String, doc = "Dotted path to the field within the component (e.g. \"translation\")."),
    ),
)]
pub(crate) fn animation_toggle_keyframe(
    params: In<OperatorParameters>,
    mut commands: Commands,
) -> OperatorResult {
    let entity = params.as_entity("entity")?;
    let type_path = params.as_str("component_type_path").map(str::to_string)?;
    let field_path = params.as_str("field_path").map(str::to_string)?;
    commands.queue(move |world: &mut World| {
        world
            .run_system_cached_with(
                super::anim_diamond::toggle_keyframe,
                (entity, type_path, field_path),
            )
            .ok();
    });
    OperatorResult::Finished
}
