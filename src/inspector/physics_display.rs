//! Command + helper backing for the `physics.enable` / `physics.disable`
//! operators. The dedicated Physics inspector section was removed in
//! favour of letting users add `RigidBody`, `AvianCollider`, and friends
//! through the standard component picker; the operators stay so the
//! command palette can still toggle the canonical bundle in one shot.

use avian3d::prelude::*;
use bevy::prelude::*;
use jackdaw_avian_integration::AvianCollider;

use crate::commands::{AddComponent, CommandGroup, CommandHistory, EditorCommand};

pub(crate) const RIGID_BODY_TYPE_PATH: &str = "avian3d::dynamics::rigid_body::RigidBody";
pub(crate) const AVIAN_COLLIDER_TYPE_PATH: &str = "jackdaw_avian_integration::AvianCollider";

/// Command that disables physics on an entity. Captures authored physics
/// patches (`RigidBody`, `AvianCollider`, and any other avian overrides the
/// user pinned) so undo restores them. Runtime `#[require]` companions are
/// never in the document and are rebuilt by avian on re-enable.
pub(crate) struct DisablePhysics {
    entity: Entity,
    /// The physics components removed, for undo.
    removed: Vec<(String, crate::commands::FieldValue)>,
}

/// Whether a type path names one of the physics components this command
/// removes: the canonical pair plus any authored avian overrides.
fn is_physics_type_path(type_path: &str) -> bool {
    type_path == RIGID_BODY_TYPE_PATH
        || type_path == AVIAN_COLLIDER_TYPE_PATH
        || type_path.starts_with("avian3d::")
}

/// The physics component a running preview owns on `entity`, if it owns
/// one of the three [`DisablePhysics`] takes off.
pub(crate) fn preview_owned_physics(world: &World, entity: Entity) -> Option<&'static str> {
    [
        (std::any::TypeId::of::<RigidBody>(), "RigidBody"),
        (std::any::TypeId::of::<AvianCollider>(), "AvianCollider"),
        (std::any::TypeId::of::<Collider>(), "Collider"),
    ]
    .into_iter()
    .find(|(type_id, _)| crate::preview_context::preview_writes(world, entity, *type_id))
    .map(|(_, name)| name)
}

impl DisablePhysics {
    pub(crate) fn from_world(world: &World, entity: Entity) -> Self {
        let mut removed = Vec::new();
        if let Ok(entity_ref) = world.get_entity(entity) {
            let type_paths: Vec<String> = entity_ref
                .archetype()
                .components()
                .iter()
                .filter_map(|&id| world.components().get_info(id)?.type_id())
                .filter_map(|type_id| {
                    let registry = world.resource::<AppTypeRegistry>().read();
                    registry
                        .get(type_id)
                        .map(|r| r.type_info().type_path().to_string())
                })
                .filter(|type_path| is_physics_type_path(type_path))
                .collect();
            for type_path in type_paths {
                if let Some(value) = crate::commands::live_field(world, entity, &type_path, "") {
                    removed.push((type_path, value));
                }
            }
        }
        Self { entity, removed }
    }
}

impl EditorCommand for DisablePhysics {
    fn execute(&mut self, world: &mut World) {
        if let Ok(mut ec) = world.get_entity_mut(self.entity) {
            ec.remove::<RigidBody>();
            ec.remove::<AvianCollider>();
            ec.remove::<Collider>();
        }
        for (type_path, _) in &self.removed {
            crate::commands::remove_component_from_ecs(world, self.entity, type_path);
        }
    }

    fn undo(&mut self, world: &mut World) {
        for (type_path, value) in &self.removed {
            crate::commands::write_field(world, self.entity, type_path, "", value.as_ref());
        }
        if let Ok(mut ec) = world.get_entity_mut(self.entity) {
            ec.insert(super::InspectorDirty);
        }
    }

    fn description(&self) -> &str {
        "Disable physics"
    }
}

pub(crate) fn enable_physics(world: &mut World, entity: Entity) {
    if world.get_entity(entity).is_err() {
        return;
    }

    let rb_type_id = std::any::TypeId::of::<RigidBody>();
    let rb_component_id = world.components().get_id(rb_type_id);

    let ac_type_id = std::any::TypeId::of::<AvianCollider>();
    let ac_component_id = world.components().get_id(ac_type_id);

    let mut pending: Vec<AddComponent> = Vec::new();

    // Add AvianCollider FIRST so the Collider is built before RigidBody
    // triggers mass computation (avoids "no mass or inertia" warning).
    if let Some(ac_cid) = ac_component_id
        && !world
            .get_entity(entity)
            .is_ok_and(|e| e.contains::<AvianCollider>())
    {
        pending.push(AddComponent::new(
            entity,
            ac_type_id,
            ac_cid,
            AVIAN_COLLIDER_TYPE_PATH.to_string(),
        ));
    }

    if let Some(rb_cid) = rb_component_id
        && !world
            .get_entity(entity)
            .is_ok_and(|e| e.contains::<RigidBody>())
    {
        pending.push(AddComponent::new(
            entity,
            rb_type_id,
            rb_cid,
            RIGID_BODY_TYPE_PATH.to_string(),
        ));
    }

    let mut commands: Vec<Box<dyn EditorCommand>> = Vec::new();
    for mut cmd in pending {
        cmd.execute(world);
        commands.push(Box::new(cmd));
    }
    if commands.is_empty() {
        return;
    }

    let cmd = if commands.len() == 1 {
        commands.remove(0)
    } else {
        Box::new(CommandGroup {
            label: "Enable physics".to_string(),
            commands,
        })
    };
    world.resource_mut::<CommandHistory>().push_executed(cmd);
}
