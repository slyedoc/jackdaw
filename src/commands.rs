use std::any::TypeId;

use bevy::{
    ecs::{
        component::ComponentId,
        reflect::{AppTypeRegistry, ReflectComponent},
    },
    prelude::*,
    reflect::{GetPath, PartialReflect, TypeRegistry},
};
use serde::de::DeserializeSeed;

// Re-export the core command framework from the jackdaw_commands crate
pub use jackdaw_commands::{CommandGroup, CommandHistory, EditorCommand};

use crate::EditorEntity;
use crate::selection::{Selected, Selection};

pub struct CommandHistoryPlugin;

impl Plugin for CommandHistoryPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(CommandHistory::default())
            .init_resource::<FieldEditSessions>();
    }
}

/// One field's (or a whole component's) value, as reflect holds it.
pub type FieldValue = Box<dyn PartialReflect>;

/// A copy of a reflected value, concrete where the type can clone itself.
pub fn clone_value(value: &dyn PartialReflect) -> Option<FieldValue> {
    value
        .reflect_clone()
        .map(|v| v.into_partial_reflect())
        .or_else(|_| value.to_dynamic())
        .ok()
}

fn values_equal(a: &dyn PartialReflect, b: &dyn PartialReflect) -> bool {
    a.reflect_partial_eq(b).unwrap_or(false)
}

/// Key for an in-progress field-edit gesture session entry.
#[derive(Clone, PartialEq, Eq, Hash)]
struct FieldEditSessionKey {
    entity: Entity,
    type_path: String,
    field_path: String,
}

/// Live field values at [`field_edit_begin`], so a commit after previews undoes to the value
/// before the gesture.
#[derive(Resource, Default)]
pub(crate) struct FieldEditSessions {
    live_at_begin: std::collections::HashMap<FieldEditSessionKey, FieldValue>,
}

fn field_edit_session_targets(world: &World) -> Vec<Entity> {
    world
        .get_resource::<Selection>()
        .map(|selection| selection.entities.clone())
        .unwrap_or_default()
}

fn clear_field_edit_session(world: &mut World, type_path: &str, field_path: &str) {
    let mut sessions = world.resource_mut::<FieldEditSessions>();
    sessions
        .live_at_begin
        .retain(|key, _| key.type_path != type_path || key.field_path != field_path);
}

fn take_live_at_begin(
    world: &mut World,
    entity: Entity,
    type_path: &str,
    field_path: &str,
) -> Option<FieldValue> {
    world
        .resource_mut::<FieldEditSessions>()
        .live_at_begin
        .remove(&FieldEditSessionKey {
            entity,
            type_path: type_path.to_string(),
            field_path: field_path.to_string(),
        })
}

/// Undo baseline for one target: the value captured when the gesture began, else the live one.
fn field_edit_old_value(
    world: &mut World,
    entity: Entity,
    type_path: &str,
    field_path: &str,
) -> Option<FieldValue> {
    take_live_at_begin(world, entity, type_path, field_path)
        .or_else(|| live_field(world, entity, type_path, field_path))
}

/// Begin a field-edit gesture for the current selection, capturing each target's live value.
pub(crate) fn field_edit_begin(world: &mut World, type_path: &str, field_path: &str) {
    let targets = field_edit_session_targets(world);
    let mut to_capture = Vec::new();
    for &entity in &targets {
        let key = FieldEditSessionKey {
            entity,
            type_path: type_path.to_string(),
            field_path: field_path.to_string(),
        };
        if world.resource::<FieldEditSessions>().live_at_begin.contains_key(&key) {
            continue;
        }
        if let Some(live) = live_field(world, entity, type_path, field_path) {
            to_capture.push((key, live));
        }
    }
    let mut sessions = world.resource_mut::<FieldEditSessions>();
    for (key, live) in to_capture {
        sessions.live_at_begin.insert(key, live);
    }
}

/// Preview a field value on the selection, with no undo entry yet.
pub(crate) fn field_edit_preview(
    world: &mut World,
    type_path: &str,
    field_path: &str,
    value: &serde_json::Value,
) {
    if crate::definition_assets::preview_definition_field(world, type_path, field_path, value) {
        return;
    }
    field_edit_begin(world, type_path, field_path);
    let targets = field_edit_session_targets(world);
    for target in targets {
        if !apply_json_map_entry_to_ecs(world, target, type_path, field_path, value) {
            apply_json_field_to_ecs(world, target, type_path, field_path, value);
        }
    }
}

/// Commit a field edit on the selection as one undo entry, and end the gesture.
///
/// A target whose component the running binding preview drives is dropped: the evaluator
/// rewrites that value every frame.
pub(crate) fn field_edit_commit(
    world: &mut World,
    type_path: &str,
    field_path: &str,
    new_json: &serde_json::Value,
    group_label: &str,
) {
    if crate::definition_assets::commit_definition_field(world, type_path, field_path, new_json) {
        return;
    }
    if let Some(entity) = world
        .get_resource::<crate::selection::Selection>()
        .and_then(crate::selection::Selection::primary)
        && crate::animgraph::document::commit_field(world, entity, type_path, field_path, new_json)
    {
        return;
    }
    if let Some(cmd) = field_edit_commit_built(world, type_path, field_path, new_json, group_label) {
        world.resource_mut::<CommandHistory>().push_executed(cmd);
    }
}

/// Build, execute and return a field edit's command, without pushing it.
fn field_edit_commit_built(
    world: &mut World,
    type_path: &str,
    field_path: &str,
    new_json: &serde_json::Value,
    group_label: &str,
) -> Option<Box<dyn EditorCommand>> {
    let mut targets = field_edit_session_targets(world);
    targets.retain(|&target| {
        let previewed = crate::preview_context::preview_writes_type_path(world, target, type_path);
        if previewed {
            warn!(
                "{}: `{type_path}` on {target}",
                crate::preview_context::PREVIEW_EDIT_REFUSED
            );
        }
        !previewed
    });

    let mut sub_commands: Vec<Box<dyn EditorCommand>> = Vec::new();
    for &target in &targets {
        let Some((path, new_value)) = field_edit_value(world, target, type_path, field_path, new_json)
        else {
            continue;
        };
        let old_value = field_edit_old_value(world, target, type_path, &path);
        if old_value
            .as_deref()
            .is_some_and(|old| values_equal(old, new_value.as_ref()))
        {
            continue;
        }
        sub_commands.push(Box::new(SetField {
            entity: target,
            type_path: type_path.to_string(),
            field_path: path,
            old_value,
            new_value,
        }));
    }
    clear_field_edit_session(world, type_path, field_path);

    if sub_commands.is_empty() {
        return None;
    }
    let mut cmd: Box<dyn EditorCommand> = if sub_commands.len() == 1 {
        sub_commands.remove(0)
    } else {
        Box::new(CommandGroup {
            label: group_label.to_string(),
            commands: sub_commands,
        })
    };
    cmd.execute(world);
    Some(cmd)
}

/// Set one field on one entity's component, as one undo entry. Whether a value was written.
pub(crate) fn field_edit_commit_on(
    world: &mut World,
    entity: Entity,
    type_path: &str,
    field_path: &str,
    new_json: &serde_json::Value,
) -> bool {
    if crate::preview_context::preview_writes_type_path(world, entity, type_path) {
        warn!(
            "{}: `{type_path}` on {entity}",
            crate::preview_context::PREVIEW_EDIT_REFUSED
        );
        return false;
    }
    let Some((path, new_value)) = field_edit_value(world, entity, type_path, field_path, new_json)
    else {
        return false;
    };
    let old_value = field_edit_old_value(world, entity, type_path, &path);
    let mut cmd: Box<dyn EditorCommand> = Box::new(SetField {
        entity,
        type_path: type_path.to_string(),
        field_path: path,
        old_value,
        new_value,
    });
    cmd.execute(world);
    world.resource_mut::<CommandHistory>().push_executed(cmd);
    true
}

pub struct SetTransform {
    pub entity: Entity,
    pub old_transform: Transform,
    pub new_transform: Transform,
}

impl EditorCommand for SetTransform {
    fn execute(&mut self, world: &mut World) {
        if let Some(mut transform) = world.get_mut::<Transform>(self.entity) {
            *transform = self.new_transform;
        }
    }

    fn undo(&mut self, world: &mut World) {
        if let Some(mut transform) = world.get_mut::<Transform>(self.entity) {
            *transform = self.old_transform;
        }
    }

    fn description(&self) -> &str {
        "Set transform"
    }
}

pub struct ReparentEntity {
    pub entity: Entity,
    pub old_parent: Option<Entity>,
    pub new_parent: Option<Entity>,
}

impl EditorCommand for ReparentEntity {
    fn execute(&mut self, world: &mut World) {
        set_hierarchy_location(
            world,
            self.entity,
            HierarchyLocation {
                parent: self.new_parent,
                index: usize::MAX,
            },
        );
    }

    fn undo(&mut self, world: &mut World) {
        set_hierarchy_location(
            world,
            self.entity,
            HierarchyLocation {
                parent: self.old_parent,
                index: usize::MAX,
            },
        );
    }

    fn description(&self) -> &str {
        "Reparent entity"
    }
}

/// Exact authored position of an entity in Jackdaw's ordered hierarchy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HierarchyLocation {
    pub parent: Option<Entity>,
    pub index: usize,
}

impl HierarchyLocation {
    /// Read an entity's current parent and sibling index.
    pub fn from_world(world: &World, entity: Entity) -> Self {
        let parent = crate::scene_io::scene_parent(world, entity);
        let index = parent
            .and_then(|parent| world.get::<Children>(parent))
            .and_then(|children| children.iter().position(|child| child == entity))
            .unwrap_or_else(|| {
                world
                    .get::<crate::scene_io::SceneRootOf>(entity)
                    .and_then(|of| world.get::<crate::scene_io::SceneRoots>(of.0))
                    .and_then(|roots| roots.entities().iter().position(|&root| root == entity))
                    .unwrap_or(0)
            });
        Self { parent, index }
    }
}

/// Undoable reparent/reorder to an exact ordered location. `ReparentEntity`
/// is the same move without a sibling index.
pub struct MoveEntity {
    pub entity: Entity,
    pub old: HierarchyLocation,
    pub new: HierarchyLocation,
}

impl MoveEntity {
    pub fn new(world: &World, entity: Entity, new: HierarchyLocation) -> Self {
        Self {
            entity,
            old: HierarchyLocation::from_world(world, entity),
            new,
        }
    }
}

impl EditorCommand for MoveEntity {
    fn execute(&mut self, world: &mut World) {
        set_hierarchy_location(world, self.entity, self.new);
    }

    fn undo(&mut self, world: &mut World) {
        set_hierarchy_location(world, self.entity, self.old);
    }

    fn description(&self) -> &str {
        "Move entity"
    }
}

/// Reparent `entity` under `parent` (or to top-level if `None`), keeping
/// the live scene document authoritative: the node's place in the document
/// hierarchy is the source of truth; the ECS `ChildOf` is mirrored from it
/// so the visual scene tracks the document. Preserves the entity's world
/// position across the move.
///
/// Any code path that needs to change an entity's parent should call
/// this (or push `ReparentEntity` through the command history) -- never
/// `world.entity_mut(e).insert(ChildOf(..))` directly. Bypassing the
/// document update leaves the node's parent stale, and later consumers
/// (prefab save, scene serialization, tab swap) read the document and
/// silently disagree with the visible hierarchy.
pub(crate) fn set_parent(world: &mut World, entity: Entity, parent: Option<Entity>) {
    set_hierarchy_location(
        world,
        entity,
        HierarchyLocation {
            parent,
            index: usize::MAX,
        },
    );
}

/// Whether a placement re-expresses the entity's world position against its
/// new parent.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum WorldTransform {
    /// The entity is already somewhere: keep it there by writing a new local
    /// `Transform` measured against the new parent.
    Keep,
    /// The entity is freshly spawned, so its `GlobalTransform` still reads
    /// identity and there is no position worth preserving.
    Unplaced,
}

/// Apply an exact ordered hierarchy location to both the live ECS and BSN
/// document while preserving an entity's world-space transform.
pub fn set_hierarchy_location(world: &mut World, entity: Entity, location: HierarchyLocation) {
    place_entity(world, entity, location, WorldTransform::Keep);
}

/// [`set_hierarchy_location`] saying whether the entity has a world position
/// worth keeping.
pub fn place_entity(
    world: &mut World,
    entity: Entity,
    location: HierarchyLocation,
    transform: WorldTransform,
) {
    // A move recorded before the scene was respawned names entities the
    // document no longer holds. Undoing it moves nothing rather than taking
    // the editor down with it.
    if world.get_entity(entity).is_err() {
        warn!("{entity} is no longer in the scene, so its move was skipped");
        return;
    }
    // The tab world is the top level.
    let location = HierarchyLocation {
        parent: location
            .parent
            .filter(|&parent| world.get::<avian3d::world::PhysicsWorld>(parent).is_none()),
        ..location
    };
    if let Some(parent) = location.parent
        && world.get_entity(parent).is_err()
    {
        warn!("{parent} is no longer in the scene, so the move under it was skipped");
        return;
    }
    let current_world = (transform == WorldTransform::Keep)
        .then(|| world.get::<GlobalTransform>(entity).copied())
        .flatten();
    let new_parent_world = location
        .parent
        .and_then(|parent| world.get::<GlobalTransform>(parent).copied());

    match location.parent {
        Some(parent) => {
            // `insert_children` removes the entity before re-inserting it,
            // so a child already under this parent shifts one slot.
            let index = world
                .get::<Children>(parent)
                .map(|children| {
                    let already = children.iter().any(|child| child == entity);
                    location.index.min(children.len() - usize::from(already))
                })
                .unwrap_or(0);
            world.entity_mut(entity).remove::<crate::scene_io::SceneRootOf>();
            world.entity_mut(parent).insert_children(index, &[entity]);
        }
        None => crate::scene_io::place_root(world, entity, location.index),
    }

    let new_transform =
        if let (Some(world_tf), Some(parent_world)) = (current_world, new_parent_world) {
            Some(Transform::from_matrix(
                (parent_world.affine().inverse() * world_tf.affine()).into(),
            ))
        } else if location.parent.is_none() {
            current_world.map(|w| Transform::from_matrix(w.affine().into()))
        } else {
            None
        };
    if let Some(new_tf) = new_transform
        && let Some(mut tf) = world.get_mut::<Transform>(entity)
    {
        *tf = new_tf;
    }
}

pub struct AddComponent {
    pub entity: Entity,
    pub type_id: TypeId,
    pub component_id: ComponentId,
    pub type_path: String,
}

impl AddComponent {
    pub fn new(
        entity: Entity,
        type_id: TypeId,
        component_id: ComponentId,
        type_path: String,
    ) -> Self {
        Self {
            entity,
            type_id,
            component_id,
            type_path,
        }
    }
}

impl EditorCommand for AddComponent {
    fn execute(&mut self, world: &mut World) {
        info!(
            "AddComponent::execute entered: type_path={}, type_id={:?}, component_id={:?}, entity={:?}",
            self.type_path, self.type_id, self.component_id, self.entity
        );
        if world.get_entity(self.entity).is_err() {
            return;
        }

        let registry = world.resource::<AppTypeRegistry>().clone();
        let registry = registry.read();

        let Some(registration) = registry.get(self.type_id) else {
            warn!(
                "AddComponent::execute: registry has no entry for type_id {:?} (type_path={})",
                self.type_id, self.type_path
            );
            return;
        };

        // `build_reflective_default` lets user components reach
        // the editor without `#[derive(Default)]` by walking
        // their fields recursively. Falls back to
        // `ReflectDefault` when the type opted in.
        let Some(default_value) =
            crate::reflect_default::build_reflective_default(self.type_id, &registry)
        else {
            warn!(
                "AddComponent::execute: type {} has no `ReflectDefault` and a field is an \
                 opaque type, list, map, or set with no default. Add `Default` to derives \
                 and `#[reflect(...)]`, or simplify the fields to reflected primitives.",
                self.type_path
            );
            return;
        };
        if registration.data::<ReflectComponent>().is_none() {
            warn!(
                "AddComponent::execute: type {} has no ReflectComponent. Add `Component` \
                 to `#[reflect(...)]`.",
                self.type_path
            );
            return;
        }
        drop(registry);

        // Insert through reflection so types without `ReflectDefault` still
        // land, and so `#[require]` companions appear the same way they do
        // on a fresh spawn. Remove/undo resync from the document; add does
        // not, because apply cannot rebuild every user type.
        info!(
            "AddComponent: inserting `{}` (type_id {:?}, component_id {:?}) on entity {:?}",
            self.type_path, self.type_id, self.component_id, self.entity
        );
        {
            let registry = world.resource::<AppTypeRegistry>().clone();
            let registry = registry.read();
            let Some(registration) = registry.get(self.type_id) else {
                return;
            };
            let Some(reflect_component) = registration.data::<ReflectComponent>() else {
                return;
            };
            reflect_component.insert(
                &mut world.entity_mut(self.entity),
                default_value.as_partial_reflect(),
                &registry,
            );
        }
        let has_after = world
            .get_entity(self.entity)
            .ok()
            .map(|e| e.archetype().components().contains(&self.component_id))
            .unwrap_or(false);
        info!(
            "AddComponent: post-insert, entity {:?} has component_id {:?}: {has_after}",
            self.entity, self.component_id
        );
    }

    fn undo(&mut self, world: &mut World) {
        remove_component_from_ecs(world, self.entity, &self.type_path);
        if let Ok(mut ec) = world.get_entity_mut(self.entity) {
            ec.insert(crate::inspector::InspectorDirty);
        }
    }

    fn description(&self) -> &str {
        "Add component"
    }
}

pub struct RemoveComponent {
    pub entity: Entity,
    pub type_id: TypeId,
    pub component_id: ComponentId,
    pub type_path: String,
    /// The value removed, for undo.
    snapshot: Option<FieldValue>,
}

impl RemoveComponent {
    pub fn from_world(
        world: &World,
        entity: Entity,
        type_id: TypeId,
        component_id: ComponentId,
        type_path: String,
    ) -> Self {
        let snapshot = live_field(world, entity, &type_path, "");
        Self {
            entity,
            type_id,
            component_id,
            type_path,
            snapshot,
        }
    }
}

impl EditorCommand for RemoveComponent {
    fn execute(&mut self, world: &mut World) {
        if let Ok(mut entity) = world.get_entity_mut(self.entity) {
            entity.remove_by_id(self.component_id);
            entity.insert(crate::inspector::InspectorDirty);
        }
    }

    fn undo(&mut self, world: &mut World) {
        if let Some(snapshot) = &self.snapshot {
            write_field(world, self.entity, &self.type_path, "", snapshot.as_ref());
        }
        if let Ok(mut ec) = world.get_entity_mut(self.entity) {
            ec.insert(crate::inspector::InspectorDirty);
        }
    }

    fn description(&self) -> &str {
        "Remove component"
    }
}

/// Scene entities spawned during the call being watched. Outside a watched
/// call nothing is recorded.
#[derive(Resource, Default)]
pub struct SpawnedEntities {
    /// Whether a caller is waiting for this list.
    watching: bool,
    entities: Vec<Entity>,
}

impl SpawnedEntities {
    /// Start recording, discarding whatever the last call left.
    pub fn watch(world: &mut World) {
        let mut spawned = world.get_resource_or_init::<Self>();
        spawned.watching = true;
        spawned.entities.clear();
    }

    /// Stop recording and take what was spawned.
    pub fn take(world: &mut World) -> Vec<Entity> {
        let mut spawned = world.get_resource_or_init::<Self>();
        spawned.watching = false;
        std::mem::take(&mut spawned.entities)
    }

    /// Record `entity` as spawned, if anyone is watching.
    pub(crate) fn record(world: &mut World, entity: Entity) {
        if let Some(mut spawned) = world.get_resource_mut::<Self>()
            && spawned.watching
        {
            spawned.entities.push(entity);
        }
    }

    /// Forget `entity`, for a command that has just taken its spawn back.
    fn forget(world: &mut World, entity: Entity) {
        if let Some(mut spawned) = world.get_resource_mut::<Self>() {
            spawned.entities.retain(|&recorded| recorded != entity);
        }
    }
}

pub struct SpawnEntity {
    /// The entity that was spawned (set after first execute).
    pub spawned: Option<Entity>,
    /// Builder function that spawns the entity and returns its Entity id.
    pub spawn_fn: Box<dyn Fn(&mut World) -> Entity + Send + Sync>,
    pub label: String,
}

impl EditorCommand for SpawnEntity {
    fn execute(&mut self, world: &mut World) {
        let entity = (self.spawn_fn)(world);
        self.spawned = Some(entity);
        SpawnedEntities::record(world, entity);
    }

    fn undo(&mut self, world: &mut World) {
        if let Some(entity) = self.spawned.take() {
            SpawnedEntities::forget(world, entity);
            deselect_entities(world, &[entity]);
            despawn_scene_entity(world, entity);
        }
    }

    fn description(&self) -> &str {
        &self.label
    }
}

pub struct DespawnEntity {
    /// The entity as it stands in the world now, rewritten by every undo to
    /// whatever id the restore minted.
    pub entity: Entity,
    /// The id the snapshot was taken under, and so the only id a restore's
    /// entity map answers to. Parts company with `entity` after a redo.
    snapshot_root: Entity,
    pub scene_snapshot: DynamicWorld,
    /// Where the entity sat before the despawn, so undo can put it back.
    pub location: HierarchyLocation,
    pub label: String,
}

impl DespawnEntity {
    /// Snapshot `entity` as it was authored. Preview writes are suspended
    /// around the read, so a running preview's values are not captured.
    pub fn from_world(world: &mut World, entity: Entity) -> Self {
        let location = HierarchyLocation::from_world(world, entity);
        let held = crate::preview_context::suspend_preview_writes(world);
        let scene = snapshot_entity(world, entity);
        crate::preview_context::resume_preview_writes(world, held);
        Self {
            entity,
            snapshot_root: entity,
            scene_snapshot: scene,
            location,
            label: format!("Despawn entity {entity}"),
        }
    }
}

impl EditorCommand for DespawnEntity {
    fn execute(&mut self, world: &mut World) {
        deselect_entities(world, &[self.entity]);
        despawn_scene_entity(world, self.entity);
    }

    fn undo(&mut self, world: &mut World) {
        // Re-build the scene from scratch and write it back
        let scene = snapshot_rebuild(&self.scene_snapshot);
        let mut entity_map = bevy::ecs::entity::hash_map::EntityHashMap::default();
        let _ = scene.write_to_world(world, &mut entity_map);
        if let Some(&new_id) = entity_map.get(&self.snapshot_root) {
            self.entity = new_id;
        }
        crate::scene_io::adopt_entity(world, self.entity);
        // A parent that has gone since leaves the entity at the top.
        let location = HierarchyLocation {
            parent: self
                .location
                .parent
                .filter(|parent| world.get_entity(*parent).is_ok()),
            index: self.location.index,
        };
        set_hierarchy_location(world, self.entity, location);
        crate::hierarchy::sync_outliner_row_order(world, location.parent);
    }

    fn description(&self) -> &str {
        &self.label
    }
}

/// Create a `DynamicWorldBuilder` that excludes computed components which become
/// stale when restored (Children references dead mesh entities, visibility flags
/// block rendering).
pub(crate) fn filtered_scene_builder<'w>(
    world: &'w World,
    type_registry: &'w bevy::reflect::TypeRegistry,
) -> DynamicWorldBuilder<'w> {
    DynamicWorldBuilder::from_world(world, type_registry)
        .deny_component::<Children>()
        .deny_component::<GlobalTransform>()
        .deny_component::<InheritedVisibility>()
        .deny_component::<ViewVisibility>()
}

/// Deselect the given entities: remove the `Selected` component and purge them
/// from the `Selection` resource. Call this before despawn so Selection does
/// not keep ids of entities that no longer exist.
pub(crate) fn deselect_entities(world: &mut World, entities: &[Entity]) {
    for &entity in entities {
        if let Ok(mut ec) = world.get_entity_mut(entity) {
            ec.remove::<Selected>();
        }
    }
    let mut selection = world.resource_mut::<Selection>();
    selection.entities.retain(|e| !entities.contains(e));
}

/// Despawn a scene entity and what is under it.
pub(crate) fn despawn_scene_entity(world: &mut World, entity: Entity) {
    if let Ok(entity_mut) = world.get_entity_mut(entity) {
        entity_mut.despawn();
    }
}

/// Create a `DynamicWorld` snapshot of a single entity and all its descendants.
pub(crate) fn snapshot_entity(world: &World, entity: Entity) -> DynamicWorld {
    let type_registry = world.resource::<AppTypeRegistry>().read();
    let mut entities = Vec::new();
    collect_entity_ids(world, entity, &mut entities);
    filtered_scene_builder(world, &type_registry)
        .extract_entities(entities.into_iter())
        .build()
}

pub(crate) fn collect_entity_ids(world: &World, entity: Entity, out: &mut Vec<Entity>) {
    out.push(entity);
    if let Some(children) = world.get::<Children>(entity) {
        for child in children.iter() {
            // A dangling child reference (e.g. left by an older duplicate) points at a
            // despawned entity; skip it so callers never feed it to DynamicSceneBuilder.
            if world.get_entity(child).is_err() {
                continue;
            }
            // Skip editor-only entities and runtime-generated children
            // (e.g. BrushMeshChunk meshes). Including NonSerializable
            // children causes them to be restored as orphans at origin
            // after undo, while the parent regenerates its own.
            if world.get::<EditorEntity>(child).is_some()
                || world.get::<crate::NonSerializable>(child).is_some()
            {
                continue;
            }
            collect_entity_ids(world, child, out);
        }
    }
}

/// Rebuild a `DynamicWorld` by copying its entity data (since `DynamicWorld` doesn't impl Clone).
pub(crate) fn snapshot_rebuild(scene: &DynamicWorld) -> DynamicWorld {
    DynamicWorld {
        resources: scene
            .resources
            .iter()
            .filter_map(|r| r.to_dynamic().ok())
            .collect(),
        entities: scene
            .entities
            .iter()
            .map(|e| bevy::world_serialization::DynamicEntity {
                entity: e.entity,
                components: e
                    .components
                    .iter()
                    .filter_map(|c| c.to_dynamic().ok())
                    .collect(),
            })
            .collect(),
    }
}

// ============================== Field Commands ==============================

/// Set one field of an entity's component (or, with an empty `field_path`, the whole
/// component), undoably.
pub struct SetField {
    pub entity: Entity,
    pub type_path: String,
    pub field_path: String,
    /// `None` when the component was absent before; undo then removes it.
    pub old_value: Option<FieldValue>,
    pub new_value: FieldValue,
}

impl EditorCommand for SetField {
    fn execute(&mut self, world: &mut World) {
        write_field(
            world,
            self.entity,
            &self.type_path,
            &self.field_path,
            self.new_value.as_ref(),
        );
    }

    fn undo(&mut self, world: &mut World) {
        match &self.old_value {
            Some(old) => write_field(world, self.entity, &self.type_path, &self.field_path, old.as_ref()),
            None if self.field_path.is_empty() => {
                remove_component_from_ecs(world, self.entity, &self.type_path)
            }
            None => reset_ecs_field_to_default(world, self.entity, &self.type_path, &self.field_path),
        }
    }

    fn description(&self) -> &str {
        "Set component field"
    }
}

/// Reflect type path of [`Name`].
pub(crate) const NAME_TYPE_PATH: &str = "bevy_ecs::name::Name";

/// Write a reflected value into one field of a live component, or insert the whole component
/// for an empty `field_path`.
pub(crate) fn write_field(
    world: &mut World,
    entity: Entity,
    type_path: &str,
    field_path: &str,
    value: &dyn PartialReflect,
) {
    let registry = world.resource::<AppTypeRegistry>().clone();
    let registry = registry.read();
    let Some(registration) = registry.get_with_type_path(type_path) else {
        return;
    };
    let Some(reflect_component) = registration.data::<ReflectComponent>() else {
        return;
    };
    crate::scene_io::author(world, entity, registration.type_id());
    let Ok(mut entity_mut) = world.get_entity_mut(entity) else {
        return;
    };
    if field_path.is_empty() {
        reflect_component.insert(&mut entity_mut, value, &registry);
        return;
    }
    let Some(component) = reflect_component.reflect_mut(entity_mut) else {
        return;
    };
    if let Ok(field) = component.into_inner().reflect_path_mut(field_path)
        && let Err(err) = field.try_apply(value)
    {
        warn!("{type_path}.{field_path}: {err}");
    }
}

/// Apply a JSON value to an ECS component -- either full component replacement
/// (empty `field_path`) or field-level update. The live write a preview uses.
pub(crate) fn apply_json_field_to_ecs(
    world: &mut World,
    entity: Entity,
    type_path: &str,
    field_path: &str,
    value: &serde_json::Value,
) {
    let registry = world.resource::<AppTypeRegistry>().clone();
    let registry = registry.read();

    let Some(registration) = registry.get_with_type_path(type_path) else {
        return;
    };
    let Some(reflect_component) = registration.data::<ReflectComponent>() else {
        return;
    };

    if field_path.is_empty() {
        // Full component replacement via TypedReflectDeserializer.
        // Always use `insert` (not `apply`)  -- this handles:
        //  - Immutable components like RigidBody (apply panics on immutable)
        //  - Components removed externally (e.g. avian removing ColliderConstructor)
        //  - Normal mutable components (insert replaces in-place)
        let deserializer =
            bevy::reflect::serde::TypedReflectDeserializer::new(registration, &registry);
        if let Ok(reflected) = deserializer.deserialize(value) {
            reflect_component.insert(&mut world.entity_mut(entity), reflected.as_ref(), &registry);
        }
    } else {
        // Field-level update via reflect_path_mut
        let Some(reflected) = reflect_component.reflect_mut(world.entity_mut(entity)) else {
            return;
        };
        if let Ok(field) = reflected.into_inner().reflect_path_mut(field_path) {
            apply_json_to_reflect(field, value, &registry);
        }
    }
}

/// Remove a reflected component from an ECS entity by type path. A no-op when
/// the type is unregistered or the entity is gone.
/// Reset one field of a live ECS component to the type's default value:
/// the state a sparse patch resolves to when the field is not authored.
fn reset_ecs_field_to_default(
    world: &mut World,
    entity: Entity,
    type_path: &str,
    field_path: &str,
) {
    use bevy::reflect::GetPath;
    use bevy::reflect::prelude::ReflectDefault;

    let registry = world.resource::<AppTypeRegistry>().clone();
    let registry = registry.read();
    let Some(registration) = registry.get_with_type_path(type_path) else {
        return;
    };
    let Some(reflect_default) = registration.data::<ReflectDefault>() else {
        return;
    };
    let Some(reflect_component) = registration.data::<ReflectComponent>() else {
        return;
    };

    let default_instance = reflect_default.default();
    let Ok(default_field) = default_instance.reflect_path(field_path) else {
        return;
    };
    let Ok(default_field) = default_field.to_dynamic() else {
        return;
    };

    let Some(component) = reflect_component.reflect_mut(world.entity_mut(entity)) else {
        return;
    };
    if let Ok(field) = component.into_inner().reflect_path_mut(field_path) {
        field.apply(&*default_field);
    }
}

pub(crate) fn remove_component_from_ecs(world: &mut World, entity: Entity, type_path: &str) {
    let registry = world.resource::<AppTypeRegistry>().clone();
    let registry = registry.read();
    let Some(registration) = registry.get_with_type_path(type_path) else {
        return;
    };
    let Some(reflect_component) = registration.data::<ReflectComponent>() else {
        return;
    };
    let Ok(mut entity_mut) = world.get_entity_mut(entity) else {
        return;
    };
    reflect_component.remove(&mut entity_mut);
}
/// A JSON number as a signed integer. `as_i64` is `None` for a float even
/// when it is a whole value, which is what a drag-scrub widget writes.
fn json_number_as_i64(n: &serde_json::Number) -> i64 {
    n.as_i64()
        .or_else(|| n.as_f64().map(|value| value as i64))
        .unwrap_or_default()
}

/// A JSON number as an unsigned integer. Same float case as
/// [`json_number_as_i64`].
fn json_number_as_u64(n: &serde_json::Number) -> u64 {
    n.as_u64()
        .or_else(|| {
            n.as_f64()
                .filter(|value| *value >= 0.0)
                .map(|value| value as u64)
        })
        .unwrap_or_default()
}

/// Convert a `serde_json::Value` into the matching reflect primitive and apply it.
/// Falls back to Bevy's typed deserialization for complex types (enums, structs)
/// that can't be handled by simple primitive downcasts.
pub(crate) fn apply_json_to_reflect(
    field: &mut dyn bevy::reflect::PartialReflect,
    value: &serde_json::Value,
    registry: &bevy::reflect::TypeRegistry,
) {
    match value {
        serde_json::Value::Number(n) => {
            if let Some(f) = field.try_downcast_mut::<f32>() {
                *f = n.as_f64().unwrap_or_default() as f32;
            } else if let Some(f) = field.try_downcast_mut::<f64>() {
                *f = n.as_f64().unwrap_or_default();
            } else if let Some(i) = field.try_downcast_mut::<i32>() {
                *i = json_number_as_i64(n) as i32;
            } else if let Some(i) = field.try_downcast_mut::<u32>() {
                *i = json_number_as_u64(n) as u32;
            } else if let Some(i) = field.try_downcast_mut::<usize>() {
                *i = json_number_as_u64(n) as usize;
            } else if let Some(i) = field.try_downcast_mut::<i8>() {
                *i = json_number_as_i64(n) as i8;
            } else if let Some(i) = field.try_downcast_mut::<i16>() {
                *i = json_number_as_i64(n) as i16;
            } else if let Some(i) = field.try_downcast_mut::<i64>() {
                *i = json_number_as_i64(n);
            } else if let Some(i) = field.try_downcast_mut::<u8>() {
                *i = json_number_as_u64(n) as u8;
            } else if let Some(i) = field.try_downcast_mut::<u16>() {
                *i = json_number_as_u64(n) as u16;
            } else if let Some(i) = field.try_downcast_mut::<u64>() {
                *i = json_number_as_u64(n);
            } else {
                // A number can still be the whole of an `Option<f32>` or a
                // `NonZero`, which reflect takes through serde's own paths.
                try_typed_deserialize(field, value, registry);
            }
        }
        serde_json::Value::Bool(b) => {
            if let Some(f) = field.try_downcast_mut::<bool>() {
                *f = *b;
            }
        }
        serde_json::Value::String(s) => {
            if let Some(f) = field.try_downcast_mut::<String>() {
                *f = s.clone();
                return;
            }
            // Unit enum variants serialize as a bare string  -- fall through to the
            // typed-deserializer path below.
            try_typed_deserialize(field, value, registry);
        }
        serde_json::Value::Object(_) | serde_json::Value::Array(_) => {
            // Structs, tuple structs, enum struct/tuple variants, lists, etc.
            try_typed_deserialize(field, value, registry);
        }
        serde_json::Value::Null => try_typed_deserialize(field, value, registry),
    }
}

/// Look up the field's `TypeRegistration` via its represented type info and run
/// `TypedReflectDeserializer` on the JSON, then apply the result.
fn try_typed_deserialize(
    field: &mut dyn bevy::reflect::PartialReflect,
    value: &serde_json::Value,
    registry: &bevy::reflect::TypeRegistry,
) {
    let Some(type_info) = field.get_represented_type_info() else {
        return;
    };
    let Some(registration) = registry.get(type_info.type_id()) else {
        return;
    };
    let deserializer = bevy::reflect::serde::TypedReflectDeserializer::new(registration, registry);
    if let Ok(reflected) = deserializer.deserialize(value) {
        field.apply(reflected.as_ref());
    }
}

/// The path to write a field edit at and the value to write there.
///
/// An edit reaching into a map entry writes the whole map at the map's own path: a reflect
/// path cannot name a map key.
fn field_edit_value(
    world: &World,
    entity: Entity,
    type_path: &str,
    field_path: &str,
    value: &serde_json::Value,
) -> Option<(String, FieldValue)> {
    if let Some(edit) = map_entry_edit(world, entity, type_path, field_path, value) {
        return Some(edit);
    }
    json_field_value(world, entity, type_path, field_path, value)
        .map(|new_value| (field_path.to_string(), new_value))
}

/// Where a field path steps into a map entry: the map's own path, the entry's
/// key as the inspector spells it (`"Leaves"` for a string key), and the path
/// that runs on inside the entry's value.
struct MapEntryPath<'a> {
    map: &'a str,
    key: &'a str,
    rest: &'a str,
}

/// The last `[key]` step of `field_path` that reads into a map held by
/// `component`, or `None` when no step does. A `[n]` into a list is left to
/// the reflect path, which reads one already.
fn map_entry_path<'a>(
    component: &dyn bevy::reflect::PartialReflect,
    field_path: &'a str,
) -> Option<MapEntryPath<'a>> {
    use bevy::reflect::{ReflectPath, ReflectRef};

    field_path.match_indices('[').rev().find_map(|(open, _)| {
        let close = open + field_path[open..].find(']')?;
        let map = &field_path[..open];
        let holder = if map.is_empty() {
            component
        } else {
            map.reflect_element(component).ok()?
        };
        matches!(holder.reflect_ref(), ReflectRef::Map(_)).then(|| MapEntryPath {
            map,
            key: &field_path[open + 1..close],
            rest: field_path[close + 1..].trim_start_matches('.'),
        })
    })
}

/// The value `field_path` names under `root`, reading a `[key]` step into a
/// map as the entry the inspector spells with that key.
pub(crate) fn read_field_path<'a>(
    root: &'a dyn bevy::reflect::PartialReflect,
    field_path: &str,
) -> Option<&'a dyn bevy::reflect::PartialReflect> {
    use bevy::reflect::{ReflectPath, ReflectRef};

    if let Ok(field) = field_path.reflect_element(root) {
        return Some(field);
    }
    let entry = map_entry_path(root, field_path)?;
    let holder = if entry.map.is_empty() {
        root
    } else {
        entry.map.reflect_element(root).ok()?
    };
    let ReflectRef::Map(map) = holder.reflect_ref() else {
        return None;
    };
    let (_, value) = map.iter().find(|(key, _)| {
        crate::inspector::reflect_fields::format_partial_reflect_value(*key) == entry.key
    })?;
    if entry.rest.is_empty() {
        Some(value)
    } else {
        read_field_path(value, entry.rest)
    }
}

/// Write `value` into the map entry `field_path` names, on a copy of the
/// entity's component, and return the map's own path with the whole map as
/// it then stands. `None` when the path steps into no map entry, or names a
/// key the map does not hold.
fn map_entry_edit(
    world: &World,
    entity: Entity,
    type_path: &str,
    field_path: &str,
    value: &serde_json::Value,
) -> Option<(String, FieldValue)> {
    if !field_path.contains('[') {
        return None;
    }
    let registry = world.resource::<AppTypeRegistry>().clone();
    let registry = registry.read();
    let registration = registry.get_with_type_path(type_path)?;
    let component = registration
        .data::<ReflectComponent>()?
        .reflect(world.get_entity(entity).ok()?)?;
    let mut merged: Box<dyn Reflect> = registration
        .data::<bevy::reflect::ReflectFromReflect>()?
        .from_reflect(component.as_partial_reflect())?;
    let map_path = write_map_entry(
        merged.as_partial_reflect_mut(),
        field_path,
        value,
        &registry,
    )?;
    let map = if map_path.is_empty() {
        merged.as_partial_reflect()
    } else {
        merged.reflect_path(map_path.as_str()).ok()?
    };
    Some((map_path.clone(), clone_value(map)?))
}

/// Write `value` into the live map entry `field_path` names. Whether the path
/// named one the component holds.
fn apply_json_map_entry_to_ecs(
    world: &mut World,
    entity: Entity,
    type_path: &str,
    field_path: &str,
    value: &serde_json::Value,
) -> bool {
    if !field_path.contains('[') {
        return false;
    }
    let registry = world.resource::<AppTypeRegistry>().clone();
    let registry = registry.read();
    let Some(reflect_component) = registry
        .get_with_type_path(type_path)
        .and_then(|registration| registration.data::<ReflectComponent>())
    else {
        return false;
    };
    let Some(live) = reflect_component.reflect_mut(world.entity_mut(entity)) else {
        return false;
    };
    write_map_entry(
        live.into_inner().as_partial_reflect_mut(),
        field_path,
        value,
        &registry,
    )
    .is_some()
}

/// Write `value` into the map entry `field_path` names under `root`, returning
/// the map's own path, or `None` when the path steps into no entry `root`
/// holds.
fn write_map_entry(
    root: &mut dyn bevy::reflect::PartialReflect,
    field_path: &str,
    value: &serde_json::Value,
    registry: &bevy::reflect::TypeRegistry,
) -> Option<String> {
    use bevy::reflect::{ReflectMut, ReflectPath};

    let (map_path, key, rest) = {
        let entry = map_entry_path(root, field_path)?;
        (
            entry.map.to_string(),
            entry.key.to_string(),
            entry.rest.to_string(),
        )
    };
    let holder: &mut dyn bevy::reflect::PartialReflect = if map_path.is_empty() {
        root
    } else {
        map_path.as_str().reflect_element_mut(root).ok()?
    };
    let ReflectMut::Map(map) = holder.reflect_mut() else {
        return None;
    };
    let mut written = false;
    map.retain(&mut |entry_key, entry_value| {
        if !written
            && crate::inspector::reflect_fields::format_partial_reflect_value(entry_key) == key
        {
            let target = if rest.is_empty() {
                Some(entry_value)
            } else {
                rest.as_str().reflect_element_mut(entry_value).ok()
            };
            if let Some(target) = target {
                apply_json_to_reflect(target, value, registry);
                written = true;
            }
        }
        true
    });
    written.then_some(map_path)
}

/// The value one field edit given as reflect-format JSON stands for. Field-level edits merge
/// the JSON into a copy of the entity's current component so nested values convert with their
/// concrete types; a string for an asset handle loads that path.
pub(crate) fn json_field_value(
    world: &World,
    entity: Entity,
    type_path: &str,
    field_path: &str,
    value: &serde_json::Value,
) -> Option<FieldValue> {
    let registry = world.resource::<AppTypeRegistry>().clone();
    let registry = registry.read();
    let registration = registry.get_with_type_path(type_path)?;
    if field_path.is_empty() {
        let deserializer =
            bevy::reflect::serde::TypedReflectDeserializer::new(registration, &registry);
        return deserializer.deserialize(value).ok();
    }
    let reflect_component = registration.data::<ReflectComponent>()?;
    let component = reflect_component.reflect(world.get_entity(entity).ok()?)?;
    let mut merged: Box<dyn Reflect> = registration
        .data::<bevy::reflect::ReflectFromReflect>()?
        .from_reflect(component.as_partial_reflect())?;
    if let Some(text) = value.as_str()
        && let Ok(field) = merged.reflect_path(field_path)
        && let Some(handle) = handle_for_path(world, &registry, field, text)
    {
        return Some(handle);
    }
    if let Ok(field) = merged.reflect_path_mut(field_path) {
        apply_json_to_reflect(field, value, &registry);
    }
    clone_value(merged.reflect_path(field_path).ok()?)
}

/// A handle of the type `field` holds, loading `path`; `None` when `field` is no asset handle.
fn handle_for_path(
    world: &World,
    registry: &TypeRegistry,
    field: &dyn PartialReflect,
    path: &str,
) -> Option<FieldValue> {
    let reflect_handle = registry
        .get_type_data::<bevy::asset::ReflectHandle>(field.get_represented_type_info()?.type_id())?;
    let untyped = world
        .resource::<AssetServer>()
        .load_builder()
        .load_erased(reflect_handle.asset_type_id(), path.to_string());
    Some(reflect_handle.typed(untyped).into_partial_reflect())
}

/// Record an authored layout edit a live gesture already applied to the ECS,
/// as one history entry. A gesture on a `Node` the running binding preview
/// drives is refused and the live value put back.
pub fn push_layout_edit(world: &mut World, entity: Entity, before: Node, after: Node) {
    push_layout_edits(world, vec![(entity, before, after)]);
}

/// Undo label a layout gesture on more than one node lands under.
const LAYOUT_GROUP_LABEL: &str = "Edit UI layout";

/// [`push_layout_edit`] for a gesture that moved a whole selection, still as
/// one history entry. Nodes the gesture left where they were drop out.
pub fn push_layout_edits(world: &mut World, edits: Vec<(Entity, Node, Node)>) {
    let mut commands: Vec<Box<dyn EditorCommand>> = Vec::new();
    for (entity, before, after) in edits {
        if before == after {
            continue;
        }
        if crate::preview_context::preview_writes(world, entity, std::any::TypeId::of::<Node>()) {
            warn!(
                "{}: `Node` on {entity}",
                crate::preview_context::PREVIEW_EDIT_REFUSED
            );
            if let Some(mut node) = world.get_mut::<Node>(entity) {
                *node = before;
            }
            continue;
        }
        let command = SetUiNode {
            entity,
            before,
            after,
        };
        commands.push(Box::new(command));
    }
    let entry: Box<dyn EditorCommand> = match commands.len() {
        0 => return,
        1 => commands.pop().expect("one command"),
        _ => Box::new(CommandGroup {
            commands,
            label: LAYOUT_GROUP_LABEL.to_string(),
        }),
    };
    world.resource_mut::<CommandHistory>().push_executed(entry);
}

/// Undoable edit of one authored UI [`Node`].
pub struct SetUiNode {
    pub entity: Entity,
    pub before: Node,
    pub after: Node,
}

impl SetUiNode {
    fn apply(&self, world: &mut World, value: &Node) {
        if let Some(mut node) = world.get_mut::<Node>(self.entity) {
            *node = value.clone();
        }
    }
}

impl EditorCommand for SetUiNode {
    fn execute(&mut self, world: &mut World) {
        let after = self.after.clone();
        self.apply(world, &after);
    }

    fn undo(&mut self, world: &mut World) {
        let before = self.before.clone();
        self.apply(world, &before);
    }

    fn description(&self) -> &str {
        "Edit UI layout"
    }
}

/// Undoable edit of one UI scene root's [`jackdaw_scene_types::CanvasGuides`].
/// `None` on either side means the component is absent, so the first guide
/// inserts it and the last one takes it off again.
pub struct SetCanvasGuides {
    pub root: Entity,
    pub before: Option<jackdaw_scene_types::CanvasGuides>,
    pub after: Option<jackdaw_scene_types::CanvasGuides>,
}

impl SetCanvasGuides {
    fn apply(&self, world: &mut World, value: &Option<jackdaw_scene_types::CanvasGuides>) {
        match value {
            Some(guides) => {
                if let Ok(mut entity) = world.get_entity_mut(self.root) {
                    entity.insert(guides.clone());
                }
            }
            None => {
                if let Ok(mut entity) = world.get_entity_mut(self.root) {
                    entity.remove::<jackdaw_scene_types::CanvasGuides>();
                }
            }
        }
        if let Ok(mut entity) = world.get_entity_mut(self.root) {
            entity.insert(crate::inspector::InspectorDirty);
        }
    }
}

impl EditorCommand for SetCanvasGuides {
    fn execute(&mut self, world: &mut World) {
        let after = self.after.clone();
        self.apply(world, &after);
    }

    fn undo(&mut self, world: &mut World) {
        let before = self.before.clone();
        self.apply(world, &before);
    }

    fn description(&self) -> &str {
        "Edit canvas guides"
    }
}

/// A copy of one live field (or, for an empty `field_path`, the whole component).
pub(crate) fn live_field(
    world: &World,
    entity: Entity,
    type_path: &str,
    field_path: &str,
) -> Option<FieldValue> {
    let registry = world.resource::<AppTypeRegistry>().clone();
    let registry = registry.read();
    let reflect_component = registry
        .get_with_type_path(type_path)?
        .data::<ReflectComponent>()?;
    let component = reflect_component.reflect(world.get_entity(entity).ok()?)?;
    if field_path.is_empty() {
        return clone_value(component.as_partial_reflect());
    }
    clone_value(component.reflect_path(field_path).ok()?)
}

#[cfg(test)]
mod spawned_entities_tests {
    use super::*;

    /// The list belongs to the call that opened it: outside one nothing is
    /// recorded, so a session driven from the menus never grows a list
    /// nobody is going to read.
    #[test]
    fn nothing_is_recorded_while_no_call_is_watching() {
        let mut world = World::new();
        let entity = world.spawn_empty().id();
        SpawnedEntities::record(&mut world, entity);
        assert!(SpawnedEntities::take(&mut world).is_empty());
    }

    /// Taking the list ends the watch, so what the next spawn does is not
    /// reported against the call that has already answered.
    #[test]
    fn a_spawn_after_the_call_took_its_list_is_not_recorded() {
        let mut world = World::new();
        SpawnedEntities::watch(&mut world);
        let first = world.spawn_empty().id();
        SpawnedEntities::record(&mut world, first);
        assert_eq!(SpawnedEntities::take(&mut world), vec![first]);

        let second = world.spawn_empty().id();
        SpawnedEntities::record(&mut world, second);
        assert!(SpawnedEntities::take(&mut world).is_empty());
    }

    /// An undo inside a watched call takes its spawn back, and a caller
    /// told about an entity that is no longer there cannot act on it.
    #[test]
    fn a_spawn_taken_back_is_not_reported() {
        let mut world = World::new();
        SpawnedEntities::watch(&mut world);
        let kept = world.spawn_empty().id();
        let undone = world.spawn_empty().id();
        SpawnedEntities::record(&mut world, kept);
        SpawnedEntities::record(&mut world, undone);
        SpawnedEntities::forget(&mut world, undone);
        assert_eq!(SpawnedEntities::take(&mut world), vec![kept]);
    }
}

