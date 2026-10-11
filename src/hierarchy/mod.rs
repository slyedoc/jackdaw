use bevy_aurora::mesh::AuroraMesh3d;
use bevy::{
    input_focus::{FocusCause, InputFocus},
    light::RectLight,
    prelude::*,
    ui::ui_transform::UiGlobalTransform,
};
use bevy_enhanced_input::prelude::{Press, *};
use bevy_monitors::prelude::{Mutation, NotifyChanged};
use jackdaw_api::prelude::*;
use jackdaw_api_internal::entity_icons::EntityIconRegistry;
use jackdaw_api_internal::keymap::PresetInput;
use jackdaw_feathers::{
    button::ButtonClickEvent,
    context_menu::spawn_context_menu,
    text_edit::{self, EditorTextEdit, TextEditCommitEvent, TextEditProps, TextEditValue},
    tokens,
};
use jackdaw_widgets::context_menu::{ContextMenuAction, ContextMenuState};
use jackdaw_widgets::tree_view::{
    EntityCategory, TreeIndex, TreeNode, TreeNodeExpanded, TreeRowClicked, TreeRowContent,
    TreeRowDropped, TreeRowDroppedOnRoot, TreeRowInlineRename, TreeRowInserted, TreeRowLabel,
    TreeRowLockToggled, TreeRowRenamed, TreeRowStartRename, TreeRowVisibilityToggled,
};

use crate::scene_io::{SceneEntity, SceneRootOf};
use crate::{
    EditorEntity, EditorHidden, OP_PREFIX,
    commands::{CommandHistory, EditorCommand, ReparentEntity, SetField},
    entity_ops,
    selection::{Selected, Selection},
};
use avian3d::environment::PhysicsEnvironment;
use bevy::scene::SceneBase;
use jackdaw_feathers::dialog::{DialogActionEvent, DialogChildrenSlot};

/// Stores the default name for the prefab save dialog.
#[derive(Resource, Default)]
struct PendingPrefabDefaultName(String);

/// The entities the extract dialog writes to a file of their own.
#[derive(Resource, Default)]
pub struct PendingPrefabSave {
    pub roots: Vec<Entity>,
}

/// Marker for the prefab name text input inside the dialog.
#[derive(Component)]
struct PrefabNameInput;

/// Marker for the hierarchy panel
#[derive(Component)]
#[require(EditorEntity)]
pub struct HierarchyPanel;

/// Marker for the container that holds tree rows. Carries the
/// widget-side [`jackdaw_widgets::tree_view::TreeRoot`] so the
/// per-container `TreeIndex` knows where to file the rows that
/// descend from it. Multi-instance Outliner tabs each spawn their
/// own container; the index keys rows by `(container, source)` so
/// they don't collide.
#[derive(Component)]
#[require(EditorEntity, jackdaw_widgets::tree_view::TreeRoot)]
pub struct HierarchyTreeContainer;

/// Controls whether the hierarchy shows all entities or only named ones.
/// `false` = named only (default), `true` = all entities (minus `EditorEntity`).
#[derive(Resource, Default)]
pub struct HierarchyShowAll(pub bool);

/// Marker for the show-all toggle button in the hierarchy panel.
#[derive(Component)]
pub struct HierarchyShowAllButton;

mod view;

pub use view::{
    OutlinerFocus, OutlinerRow, OutlinerView, listed_children, outliner_keyboard_navigation,
    reveal_in_outliner,
    set_outliner_expanded, visible_row_sources,
};

pub struct HierarchyPlugin;

impl Plugin for HierarchyPlugin {
    fn build(&self, app: &mut App) {
        view::plugin(app);
        app.init_resource::<ContextMenuState>()
            .init_resource::<PendingPrefabDefaultName>()
            .init_resource::<PendingPrefabSave>()
            .init_resource::<HierarchyShowAll>()
            .init_resource::<EntityIconRegistry>()
            .init_resource::<OutlinerRangeAnchor>()
            .add_systems(Startup, setup_tree_node_expanded_watcher)
            .add_systems(OnEnter(crate::AppState::Editor), setup_name_watcher)
            .add_systems(
                Update,
                (
                    auto_focus_inline_rename,
                    populate_prefab_dialog,
                    update_show_all_button_appearance,
                )
                    .run_if(crate::editor_active),
            )
            .add_systems(
                PostUpdate,
                (
                    jackdaw_feathers::tree_view::sync_tree_drop_line,
                    jackdaw_feathers::tree_view::spring_load_tree_rows,
                    jackdaw_feathers::tree_view::auto_scroll_tree_on_drag,
                    jackdaw_feathers::tree_view::cancel_tree_drag_on_escape,
                    jackdaw_feathers::tree_view::ellipsize_tree_row_labels,
                )
                    .after(jackdaw_widgets::tree_view::maintain_tree_index),
            )
            .add_observer(hide_input_action)
            .add_observer(hide_input_binding)
            .add_observer(toggle_show_all_button)
            .add_observer(handle_inline_rename_commit)
            .add_observer(on_tree_row_clicked)
            .add_observer(on_tree_row_dropped)
            .add_observer(on_tree_row_inserted)
            .add_observer(on_tree_row_dropped_on_root)
            .add_observer(on_tree_row_start_rename)
            .add_observer(on_tree_row_renamed)
            .add_observer(on_context_menu_action)
            .add_observer(on_visibility_toggled)
            .add_observer(on_lock_toggled)
            .add_observer(on_prefab_dialog_action);
    }
}

/// True when `entity` was spawned by the glTF loader under a `GltfSource`
/// root rather than authored by the user. Authored entities parented under a
/// model keep a document node, so the absence of one is what separates the
/// loader's nodes from anything the user put there.
fn is_asset_part(world: &World, entity: Entity) -> bool {
    if world.get::<SceneEntity>(entity).is_some() {
        return false;
    }
    let mut current = entity;
    while let Some(ChildOf(parent)) = world.get::<ChildOf>(current) {
        if world
            .get::<jackdaw_scene_types::GltfSource>(*parent)
            .is_some()
        {
            return true;
        }
        current = *parent;
    }
    false
}

/// The text a row shows: the entity's name, the prefab file an unnamed
/// instance points at, or the entity itself.
pub(crate) fn row_label(world: &World, entity: Entity) -> String {
    if let Some(name) = world.get::<Name>(entity) {
        return name.as_str().to_string();
    }
    if let Some(stem) = prefab_stem_label(world, entity) {
        return stem;
    }
    format!("Entity {entity}")
}

/// The file stem an instance that inherited no name is labelled by, which is
/// all anyone has to go on until the prefab is back.
pub(crate) fn prefab_stem_label(world: &World, entity: Entity) -> Option<String> {
    if world.get::<Name>(entity).is_some() {
        return None;
    }
    let base = crate::instances::base_of(world, entity)?;
    Some(crate::bsn_files::asset_stem(&base).to_string())
}

/// Whether `entity` is a top-level row: one of the active tab's roots.
fn is_outliner_root(world: &World, entity: Entity) -> bool {
    match crate::scene_io::scene_environment(world) {
        Some(tab_environment) => world
            .get::<SceneRootOf>(entity)
            .is_some_and(|of| of.0 == tab_environment),
        None => world.get::<ChildOf>(entity).is_none(),
    }
}

/// Whether the entity is a prefab instance, named or not.
fn names_a_prefab(world: &World, entity: Entity) -> bool {
    world.get::<SceneBase>(entity).is_some()
}

/// Classify a scene entity by its primary component for tree display.
/// Returns the underlying category (Brush mesh, Camera, Light, etc.)
/// regardless of whether the entity is inherited from a prefab. Inherited
/// status is conveyed separately via [`is_inherited_descendant`] so the
/// outliner can pair the right icon with a muted color.
fn classify_entity(world: &World, entity: Entity) -> EntityCategory {
    // Checked before the component-based arms below: a glTF leaf carries
    // `AuroraMesh3d` and would otherwise read as an ordinary authored mesh.
    if is_asset_part(world, entity) {
        return EntityCategory::AssetPart;
    }
    if world.get::<SceneBase>(entity).is_some() {
        return EntityCategory::Prefab;
    }
    if world.get::<Camera>(entity).is_some() {
        return EntityCategory::Camera;
    }
    if world.get::<PointLight>(entity).is_some()
        || world.get::<DirectionalLight>(entity).is_some()
        || world.get::<SpotLight>(entity).is_some()
        || world.get::<RectLight>(entity).is_some()
    {
        return EntityCategory::Light;
    }
    if world.get::<AuroraMesh3d>(entity).is_some() {
        return EntityCategory::Mesh;
    }
    // A UI scene root takes the same category as a 3D one, so the two sort
    // together in the outliner.
    if world
        .get::<jackdaw_scene_types::SceneRootTag>(entity)
        .is_some()
        || world
            .get::<jackdaw_scene_types::UiSceneRoot>(entity)
            .is_some()
    {
        return EntityCategory::Scene;
    }
    // `GltfSource` is the authored component and `WorldAssetRoot` the handle
    // derived from it, so match the former first: otherwise the row's icon
    // depends on whether the asset has finished loading yet.
    if world
        .get::<jackdaw_scene_types::GltfSource>(entity)
        .is_some()
        || world.get::<WorldAssetRoot>(entity).is_some()
    {
        return EntityCategory::Scene;
    }
    // An entity with no type of its own but with children reads as a grouping
    // container (a "Trees" or "Player" parent), so it gets the group icon.
    if has_visible_children(world, entity) {
        return EntityCategory::Group;
    }
    EntityCategory::Entity
}

/// True when this entity is an inherited descendant of a prefab instance
/// (`PrefabEntityId` present, `IsA` absent). The outliner mutes such
/// rows so they're visually distinguishable from authored entities.
fn is_inherited_descendant(world: &World, entity: Entity) -> bool {
    crate::instances::is_inherited(world, entity)
}

/// Check if an entity has any children that would actually produce an
/// outliner row. This mirrors the expansion filter exactly, so the expand
/// chevron only appears when expanding the row would spawn something.
fn has_visible_children(world: &World, entity: Entity) -> bool {
    world
        .get::<Children>(entity)
        .is_some_and(|children| children.iter().any(|child| is_outliner_child(world, child)))
}

/// Whether a child entity should appear in the outliner. A `Children` list can
/// still name a despawned entity (duplicating a brush copies its `Children`, and
/// the scene mapper rewrites the runtime mesh-chunk refs to dead entity ids), so
/// dead entities are rejected first: `world.get::<Marker>` returns `None` for a
/// dead entity just as it does for a live one lacking the marker, which would
/// otherwise let a dead ref pass as a real child. Editor-only entities, hidden
/// entities, and the face meshes the editor re-derives from a `Brush` (a brush
/// is one row, not a row plus a child per generated face) are also excluded.
fn is_outliner_child(world: &World, child: Entity) -> bool {
    world.get_entity(child).is_ok()
        && world.get::<EditorEntity>(child).is_none()
        && world.get::<EditorHidden>(child).is_none()
        && world
            .get::<jackdaw_scene_types::DerivedFaceMesh>(child)
            .is_none()
        && !is_generated_part(world, child)
}

/// Whether `child` is a part some widget or plugin generated under an
/// authored node rather than something the user placed: the document holds a
/// node for the parent but none for the child. "Show All" turns it off.
fn is_generated_part(world: &World, child: Entity) -> bool {
    if world
        .get_resource::<HierarchyShowAll>()
        .is_some_and(|show_all| show_all.0)
    {
        return false;
    }
    let Some(parent) = world.get::<ChildOf>(child).map(ChildOf::parent) else {
        return false;
    };
    if world.get::<SceneEntity>(child).is_some() || world.get::<SceneEntity>(parent).is_none() {
        return false;
    }
    // What a world asset spawned under an instance is an internal, not a
    // generated part: it is what opening the instance's row shows.
    !is_asset_part(world, child)
}

/// Forget the lists the Outliner built from a scene that is going away: they name entity
/// ids a later spawn will reuse.
pub fn forget_withheld_rows(world: &mut World) {
    if let Some(mut tree) = world.get_resource_mut::<view::OutlinerTree>() {
        tree.drop_all();
    }
}

/// The entities the outliner stopped waiting for a document node from. The lists are
/// rebuilt from the world whenever it changes, so nothing is ever waited for.
pub fn rows_the_outliner_gave_up_on(_world: &World) -> Vec<Entity> {
    Vec::new()
}

/// Returns true if `entity` has `PrefabEntityId` but NOT `IsA` -- meaning
/// it's an entity materialized from a prefab, not an instance root.
fn is_inherited_entity(world: &World, entity: Entity) -> bool {
    crate::instances::is_inherited(world, entity)
}

/// Walk `entity`'s parent chain until a `HierarchyTreeContainer` is
/// found, returning its [`Entity`]. Used by per-row code paths that
/// need to address the owning Outliner panel for `TreeIndex` lookups
/// keyed by `(container, source)`.
fn ancestor_hierarchy_root(world: &World, entity: Entity) -> Option<Entity> {
    let mut current = entity;
    loop {
        if world.get::<HierarchyTreeContainer>(current).is_some() {
            return Some(current);
        }
        match world.get::<ChildOf>(current) {
            Some(ChildOf(parent)) => current = *parent,
            None => return None,
        }
    }
}

/// List every panel again from the world.
pub(crate) fn rebuild_hierarchy(world: &mut World) -> Result {
    world.resource_mut::<view::OutlinerTree>().drop_all();
    Ok(())
}
/// Spawn a watcher entity that notifies us when Name is mutated in-place.
fn setup_name_watcher(mut commands: Commands) {
    commands
        .spawn((EditorEntity, NotifyChanged::<Name>::default()))
        .observe(on_name_mutated);
}

/// Pre-register the `NotifyChanged<TreeNodeExpanded>` hook during
/// Startup. `bevy_monitors`'s add-hook queues a command that calls
/// `world.schedule_scope(Update, ...)` the first time any entity with
/// `NotifyChanged<C>` spawns. If that first spawn happens while `Update`
/// is already executing (e.g. `reconcile_tree` spawning scene tree rows
/// on workspace switch), the queued command panics with "Schedule
/// Update not found". Registering a watcher entity here in Startup
/// flushes the hook before any `Update` tick runs, so subsequent spawns
/// take the `DetectingChanges<TreeNodeExpanded>` early-return branch.
fn setup_tree_node_expanded_watcher(mut commands: Commands) {
    commands.spawn(NotifyChanged::<TreeNodeExpanded>::default());
}

/// A name edited in place can move its row among its siblings, which are ordered by name
/// under anything the document does not order.
fn on_name_mutated(trigger: On<Mutation<Name>>, mut commands: Commands) {
    let entity = trigger.mutated;
    commands.queue(move |world: &mut World| view::drop_list_of(world, entity));
}

/// Handle tree row click -> select the source entity.
///
/// Ctrl+click toggles the row in or out of the selection, Shift sweeps from
/// the anchor. A double click on a prefab instance root opens the scene it
/// inherits from, the only way to edit an imported UI scene.
fn on_tree_row_clicked(
    event: On<TreeRowClicked>,
    mut commands: Commands,
    mut selection: ResMut<Selection>,
    mut focus: ResMut<view::OutlinerFocus>,
    mut anchor: ResMut<OutlinerRangeAnchor>,
    keyboard: Res<ButtonInput<KeyCode>>,
    parent_query: Query<&ChildOf>,
    containers: Query<(), With<HierarchyTreeContainer>>,
    time: Res<Time>,
    instances: Query<(), With<SceneBase>>,
    asset_sources: Query<(), With<jackdaw_scene_types::GltfSource>>,
    document: Query<(), With<SceneEntity>>,
    mut last_click: Local<Option<(Entity, f64)>>,
) {
    // Skip remote entity proxies, handled by entity_browser observer

    // Selecting inside a world asset selects the instance: an internal has no
    // node in the document, so there is nothing about it to inspect or save.
    let selected_entity = asset_instance_for_selection(
        event.source_entity,
        &parent_query,
        &asset_sources,
        &document,
    );

    // A consumed double click resets, so a third click starts a new pair
    // rather than opening the source again.
    let now = time.elapsed_secs_f64();
    let doubled = matches!(*last_click, Some((entity, at))
        if entity == event.source_entity && now - at < DOUBLE_CLICK_SECS);
    *last_click = (!doubled).then_some((event.source_entity, now));
    if doubled {
        // On an instance or anything inside one: edit its source, with that entity picked.
        let picked = event.source_entity;
        let is_instance = instances.contains(picked);
        commands.queue(move |world: &mut World| {
            if is_instance || crate::instances::is_inherited(world, picked) {
                crate::instances::edit_source(world, picked);
            }
        });
        if is_instance {
            return;
        }
    }

    let ctrl = keyboard.any_pressed([KeyCode::ControlLeft, KeyCode::ControlRight]);
    let shift = keyboard.any_pressed([KeyCode::ShiftLeft, KeyCode::ShiftRight]);

    // A plain click on an already-selected row keeps it selected: clicking
    // the row you are working on is how a panel is brought back into focus.
    if ctrl {
        selection.toggle(&mut commands, selected_entity);
        anchor.0 = Some(selected_entity);
    } else if shift {
        let clicked = event.entity;
        let target = selected_entity;
        commands.queue(move |world: &mut World| select_row_range(world, clicked, target));
    } else {
        selection.select_single(&mut commands, selected_entity);
        anchor.0 = Some(selected_entity);
    }

    // The keyboard walk goes on from the row clicked, in the panel it was clicked in.
    let mut current = event.entity;
    while let Ok(&ChildOf(parent)) = parent_query.get(current) {
        if containers.contains(parent) {
            focus.0 = Some((parent, event.source_entity));
            break;
        }
        current = parent;
    }
}

/// The entity a click on `clicked`'s row selects: the instance for anything a
/// world asset spawned, and the entity itself otherwise. The query form of
/// `is_asset_part`.
fn asset_instance_for_selection(
    clicked: Entity,
    parents: &Query<&ChildOf>,
    asset_sources: &Query<(), With<jackdaw_scene_types::GltfSource>>,
    document: &Query<(), With<SceneEntity>>,
) -> Entity {
    if document.contains(clicked) {
        return clicked;
    }
    let mut current = clicked;
    while let Ok(&ChildOf(parent)) = parents.get(current) {
        if asset_sources.contains(parent) {
            return parent;
        }
        current = parent;
    }
    clicked
}

/// How long after a click a second one still reads as a double click, for the
/// whole editor.
pub(crate) const DOUBLE_CLICK_SECS: f64 = 0.4;

/// Where a Shift-click's range starts: the row a plain click or Ctrl-click
/// last landed on. Shift-clicks leave it alone, so a run of them sweeps the
/// range from one end rather than growing it a row at a time.
#[derive(Resource, Default)]
pub struct OutlinerRangeAnchor(pub Option<Entity>);

/// Select every visible row between the anchor and `target`.
///
/// The anchor holds still, so the click's own row ends up primary. An anchor
/// that is not on this panel's visible list selects only the clicked row and
/// becomes the new anchor.
fn select_row_range(world: &mut World, clicked: Entity, target: Entity) {
    let container = ancestor_hierarchy_root(world, clicked);
    let anchor = world.resource::<OutlinerRangeAnchor>().0;
    let rows = container
        .map(|container| visible_row_sources(world, container))
        .unwrap_or_default();
    let span = anchor.and_then(|anchor| {
        let from = rows.iter().position(|source| *source == anchor)?;
        let to = rows.iter().position(|source| *source == target)?;
        Some(if from <= to {
            rows[from..=to].to_vec()
        } else {
            rows[to..=from].iter().rev().copied().collect()
        })
    });
    match span {
        Some(span) => {
            let mut state: bevy::ecs::system::SystemState<(Commands, ResMut<Selection>)> =
                bevy::ecs::system::SystemState::new(world);
            if let Ok((mut commands, mut selection)) = state.get_mut(world) {
                selection.select_multiple(&mut commands, &span);
            }
            state.apply(world);
        }
        None => {
            crate::selection::select_only(world, target);
            world.resource_mut::<OutlinerRangeAnchor>().0 = Some(target);
        }
    }
}

/// Handle tree row dropped -> reparent the scene entity with undo support.
fn on_tree_row_dropped(
    event: On<TreeRowDropped>,
    mut commands: Commands,
    parent_query: Query<&ChildOf>,
) {
    let dragged = event.dragged_source;
    let target = event.target_source;

    if dragged == target {
        return;
    }

    // A row dropped on the parent it already has: the reparent would
    // remove the child and add it back in one command, and the row
    // observers would rebuild a row that is already where it belongs.
    if parent_query
        .get(dragged)
        .is_ok_and(|child_of| child_of.0 == target)
    {
        return;
    }

    // Cycle check: walk up from target, ensure dragged is not an ancestor
    let mut current = target;
    while let Ok(&ChildOf(parent)) = parent_query.get(current) {
        if parent == dragged {
            return;
        }
        current = parent;
    }

    commands.queue(move |world: &mut World| {
        // A base's contents are edited in the base, not moved out of an instance.
        if is_inherited_entity(world, dragged) {
            crate::status_bar::notify_error(
                world,
                "that entity comes from the file its instance inherits; edit it there".to_string(),
            );
            return;
        }

        let old_parent = world.get::<ChildOf>(dragged).map(|c| c.0);
        let mut cmd = ReparentEntity {
            entity: dragged,
            old_parent,
            new_parent: Some(target),
        };
        cmd.execute(world);
        world
            .resource_mut::<CommandHistory>()
            .push_executed(Box::new(cmd));
    });
}

/// `parent`'s children (the scene's roots for `None`) were reordered: every panel lists them
/// again in the scene's order. A reorder changes no row's parent, so no observer hears it.
pub fn sync_outliner_row_order(world: &mut World, parent: Option<Entity>) {
    view::drop_child_list(world, parent);
}

/// Handle a drop in the gap between two rows: reorder rather than reparent.
/// The widget reports which row the gap sits against and which side of it.
fn on_tree_row_inserted(
    event: On<TreeRowInserted>,
    mut commands: Commands,
    parent_query: Query<&ChildOf>,
    tab_environments: Query<(), With<PhysicsEnvironment>>,
) {
    let dragged = event.dragged_source;
    let after = event.index > 0;
    // The gap under the last row of a branch is also under the branch: `up` levels above
    // the row it was dropped against.
    let mut target = event.target;
    for _ in 0..event.up {
        match parent_query.get(target) {
            Ok(&ChildOf(parent)) if !tab_environments.contains(parent) => target = parent,
            _ => break,
        }
    }

    if dragged == target {
        return;
    }
    // Dropping a node into its own subtree would orphan the branch.
    let mut current = target;
    while let Ok(&ChildOf(parent)) = parent_query.get(current) {
        if parent == dragged {
            return;
        }
        current = parent;
    }

    commands.queue(move |world: &mut World| {
        insert_dragged(world, dragged, target, after);
    });
}

/// Move what the drag was carrying into the gap beside `target`.
///
/// A drag that starts on a selected row carries the whole selection, and it
/// lands in the order the tree shows it rather than the order it was clicked.
fn insert_dragged(world: &mut World, dragged: Entity, target: Entity, after: bool) {
    let landing = crate::commands::HierarchyLocation::from_world(world, target);
    let moving = dragged_group(world, dragged, target);
    if moving.is_empty() {
        return;
    }

    let mut moves: Vec<Box<dyn crate::commands::EditorCommand>> = Vec::new();
    let mut lists: Vec<Option<Entity>> = Vec::new();
    let mut slot = landing.index + usize::from(after);
    for entity in moving {
        let old = crate::commands::HierarchyLocation::from_world(world, entity);
        let mut index = slot;
        // Taking the node out of the list first shifts every later slot
        // down by one, so a move further down its own list aims one short.
        if old.parent == landing.parent && old.index < index {
            index -= 1;
        }
        if old.parent == landing.parent && old.index == index {
            // Already where it is going, but the ones behind it still
            // land after it.
            slot = index + 1;
            continue;
        }
        let mut command = crate::commands::MoveEntity::new(
            world,
            entity,
            crate::commands::HierarchyLocation {
                parent: landing.parent,
                index,
            },
        );
        command.execute(world);
        moves.push(Box::new(command));
        if !lists.contains(&old.parent) {
            lists.push(old.parent);
        }
        slot = index + 1;
    }

    let entry: Box<dyn crate::commands::EditorCommand> = match moves.len() {
        0 => return,
        1 => moves.pop().expect("one move"),
        _ => Box::new(crate::commands::CommandGroup {
            commands: moves,
            label: "Reorder entities".to_string(),
        }),
    };
    world.resource_mut::<CommandHistory>().push_executed(entry);
    if !lists.contains(&landing.parent) {
        lists.push(landing.parent);
    }
    for list in lists {
        sync_outliner_row_order(world, list);
    }
}

/// What a drag starting on `dragged` carries, in the order the tree shows it:
/// the whole selection when the dragged row is part of one, and that row alone
/// otherwise.
///
/// A node the drop would bury inside itself, and a node another member already
/// carries with its subtree, leave the group rather than refusing the gesture.
fn dragged_group(world: &mut World, dragged: Entity, target: Entity) -> Vec<Entity> {
    let selected: Vec<Entity> = world.resource::<Selection>().entities.clone();
    let mut group = if selected.contains(&dragged) && selected.len() > 1 {
        selected
    } else {
        vec![dragged]
    };
    group.retain(|&entity| {
        entity != target
            && world.get_entity(entity).is_ok()
            && world.get::<EditorEntity>(entity).is_none()
            && !is_ancestor_of(world, entity, target)
    });
    let carried = group.clone();
    group.retain(|&entity| {
        !carried
            .iter()
            .any(|&other| other != entity && is_ancestor_of(world, other, entity))
    });
    group.sort_by_key(|&entity| visual_order_key(world, entity));
    group
}

/// Whether `ancestor` is somewhere above `entity`.
fn is_ancestor_of(world: &World, ancestor: Entity, entity: Entity) -> bool {
    let mut current = entity;
    while let Some(parent) = world.get::<ChildOf>(current).map(ChildOf::parent) {
        if parent == ancestor {
            return true;
        }
        current = parent;
    }
    false
}

/// The sibling indices from the scene root down to `entity`, which sort
/// the way the tree draws its rows.
fn visual_order_key(world: &World, entity: Entity) -> Vec<usize> {
    let mut path = Vec::new();
    let mut current = entity;
    for _ in 0..64 {
        path.push(crate::commands::HierarchyLocation::from_world(world, current).index);
        let Some(parent) = world.get::<ChildOf>(current).map(ChildOf::parent) else {
            break;
        };
        current = parent;
    }
    path.reverse();
    path
}

/// Handle tree row dropped on root container -> deparent the scene entity.
fn on_tree_row_dropped_on_root(
    event: On<TreeRowDroppedOnRoot>,
    mut commands: Commands,
    parent_query: Query<&ChildOf, Without<EditorEntity>>,
    tab_environments: Query<(), With<PhysicsEnvironment>>,
) {
    let dragged = event.dragged_source;

    let old_parent = match parent_query.get(dragged) {
        Ok(child_of) if !tab_environments.contains(child_of.0) => Some(child_of.0),
        _ => return,
    };

    let mut cmd = ReparentEntity {
        entity: dragged,
        old_parent,
        new_parent: None,
    };

    commands.queue(move |world: &mut World| {
        // `unpack_child` requires a drop-target key, so a true unpack to
        // the project root has no operator yet. For now, dragging an
        // inherited entity to the empty root just deparents it in the
        // ECS; the AST instance keeps owning it, so the next scene
        // re-resolve will reanchor it under its instance root.
        cmd.execute(world);
        world
            .resource_mut::<CommandHistory>()
            .push_executed(Box::new(cmd));
    });
}

/// Open the hierarchy row context menu under the cursor (RMB).
#[operator(
    id = "hierarchy.open_context_menu",
    label = "Open Context Menu",
    description = "Show the context menu for the entity under the cursor.",
    allows_undo = false
)]
pub(crate) fn hierarchy_open_context_menu(
    _: In<OperatorParameters>,
    mut commands: Commands,
    mut state: ResMut<ContextMenuState>,
    cursor: crate::viewport::UiCursorPos,
    selection: Res<Selection>,
    tree_row_contents: Query<(Entity, &ChildOf), With<TreeRowContent>>,
    tree_nodes: Query<&TreeNode>,
    computed_nodes: Query<(&ComputedNode, &UiGlobalTransform), With<TreeRowContent>>,
    extension_add_entries: Query<&jackdaw_api_internal::lifecycle::RegisteredMenuEntry>,
    q_isa: Query<(), With<SceneBase>>,
) -> OperatorResult {
    let cursor_pos = cursor.get()?;

    // Close any existing context menu
    if let Some(menu) = state.menu_entity.take()
        && let Ok(mut ec) = commands.get_entity(menu)
    {
        ec.despawn();
    }

    // Find which tree row content the cursor is over by hit testing
    let mut target_source = None;
    for (content_entity, child_of) in &tree_row_contents {
        let Ok((computed, global_transform)) = computed_nodes.get(content_entity) else {
            continue;
        };
        let inv_scale = computed.inverse_scale_factor();
        let size = computed.size() * inv_scale;
        let (_, _, translation) = global_transform.to_scale_angle_translation();
        let pos = translation * inv_scale;
        let half = size / 2.0;
        let rect = Rect::from_center_half_size(pos, half);
        if rect.contains(cursor_pos)
            && let Ok(tree_node) = tree_nodes.get(child_of.0)
        {
            target_source = Some(tree_node.0);
            break;
        }
    }

    let target = target_source?;

    // If the right-clicked entity isn't selected, select it
    if !selection.is_selected(target) {
        commands.queue(move |world: &mut World| {
            let old_entities: Vec<Entity> = world.resource::<Selection>().entities.clone();
            let mut selection = world.resource_mut::<Selection>();
            selection.entities.clear();
            selection.entities.push(target);

            for &e in &old_entities {
                if e != target
                    && let Ok(mut ec) = world.get_entity_mut(e)
                {
                    ec.remove::<Selected>();
                }
            }
            if let Ok(mut ec) = world.get_entity_mut(target) {
                ec.insert(Selected);
            }
        });
    }

    // Built-in context menu items. The "Add Child ..." entries are the
    // parent-aware variant: they spawn the entity and reparent it under
    // the right-clicked target.
    let mut owned_items: Vec<(String, String)> = vec![
        (
            "hierarchy.focus".into(),
            "Focus                    F".into(),
        ),
        ("hierarchy.rename".into(), "Rename              F2".into()),
        (
            "hierarchy.duplicate".into(),
            "Duplicate        Ctrl+D".into(),
        ),
        ("hierarchy.delete".into(), "Delete             Del".into()),
        (
            format!("{OP_PREFIX}ui.group_into"),
            "Group Into Container   Ctrl+G".into(),
        ),
        (
            format!("{OP_PREFIX}ui.ungroup"),
            "Ungroup    Ctrl+Shift+G".into(),
        ),
        ("hierarchy.save_prefab".into(), "Extract to File...".into()),
        ("hierarchy.add_cube".into(), "Add Child Cube".into()),
        ("hierarchy.add_sphere".into(), "Add Child Sphere".into()),
        ("hierarchy.add_light".into(), "Add Child Light".into()),
        ("hierarchy.add_empty".into(), "Add Child Empty".into()),
    ];

    // If the right-clicked target is a prefab instance root (has IsA),
    // expose prefab-instance specific actions above the generic ones.
    if q_isa.get(target).is_ok() {
        owned_items.insert(
            0,
            (
                "hierarchy.prefab.unbundle_instance".into(),
                "Unbundle Instance".into(),
            ),
        );
    }

    // Append extension-contributed Add entries from the same source the
    // toolbar Add menu and the Add Entity picker use. One
    // `register_menu_entry` call therefore surfaces in all three places.
    let mut ext_rows: Vec<(String, String)> = extension_add_entries
        .iter()
        .filter(|entry| entry.menu == TopLevelMenu::Add)
        .map(|entry| {
            (
                format!("{OP_PREFIX}{}", entry.operator_id),
                format!("Add {}", entry.label),
            )
        })
        .collect();
    ext_rows.sort_by(|a, b| a.1.cmp(&b.1));
    owned_items.extend(ext_rows);

    let items: Vec<(&str, &str)> = owned_items
        .iter()
        .map(|(a, l)| (a.as_str(), l.as_str()))
        .collect();

    let menu = spawn_context_menu(&mut commands, cursor_pos, Some(target), &items);
    state.menu_entity = Some(menu);
    state.target_entity = Some(target);
    OperatorResult::Finished
}

/// Handle context menu actions for hierarchy operations.
fn on_context_menu_action(
    event: On<ContextMenuAction>,
    mut commands: Commands,
    global_transforms: Query<&GlobalTransform>,
    mut camera_query: Query<&mut Transform, With<jackdaw_camera::EditorCamera>>,
) {
    let target_entity = event.target_entity;

    match event.action.as_str() {
        "hierarchy.focus" => {
            if let Some(target) = target_entity
                && let Ok(global_tf) = global_transforms.get(target)
            {
                let target_pos = global_tf.translation();
                let scale = global_tf.compute_transform().scale;
                let dist = (scale.length() * 3.0).max(5.0);

                for mut transform in &mut camera_query {
                    let forward = transform.forward().as_vec3();
                    transform.translation = target_pos - forward * dist;
                    *transform = transform.looking_at(target_pos, Vec3::Y);
                }
            }
        }
        "hierarchy.rename" => {
            if let Some(target) = target_entity {
                commands
                    .operator(RenameBeginOp::ID)
                    .param("entity", target)
                    .call();
            }
        }
        "hierarchy.duplicate" => {
            commands.queue(|world: &mut World| {
                entity_ops::duplicate_selected(world);
            });
        }
        "hierarchy.delete" => {
            commands.queue(|world: &mut World| {
                entity_ops::delete_selected(world);
            });
        }
        "hierarchy.add_cube" => add_child_entity(
            &mut commands,
            target_entity,
            entity_ops::EntityTemplate::Cube,
        ),
        "hierarchy.add_sphere" => add_child_entity(
            &mut commands,
            target_entity,
            entity_ops::EntityTemplate::Sphere,
        ),
        "hierarchy.add_light" => add_child_entity(
            &mut commands,
            target_entity,
            entity_ops::EntityTemplate::PointLight,
        ),
        "hierarchy.add_empty" => add_child_entity(
            &mut commands,
            target_entity,
            entity_ops::EntityTemplate::Empty,
        ),
        "hierarchy.save_prefab" => {
            commands.queue(move |world: &mut World| {
                // Prefer the right-clicked entity. The current Selection
                // is only used when the user right-clicked an entity that
                // IS part of the selection (multi-select save). When the
                // right-click lands on something outside the selection,
                // the user expects that row to be the target -- otherwise
                // they'd silently save the wrong entity tree.
                let selection: Vec<Entity> = world
                    .resource::<crate::selection::Selection>()
                    .entities
                    .clone();
                let roots = match target_entity {
                    Some(target) if selection.contains(&target) => selection,
                    Some(target) => vec![target],
                    None => selection,
                };
                if roots.is_empty() {
                    return;
                }
                info!(
                    "hierarchy.save_prefab: target_entity={:?}, selection_len={}, roots_len={}",
                    target_entity,
                    world
                        .resource::<crate::selection::Selection>()
                        .entities
                        .len(),
                    roots.len(),
                );
                let default_name = roots
                    .first()
                    .and_then(|e| world.get::<Name>(*e).map(|n| n.as_str().to_string()))
                    .unwrap_or_else(|| "prefab".to_string());
                world.resource_mut::<PendingPrefabSave>().roots = roots;
                world.resource_mut::<PendingPrefabDefaultName>().0 = default_name;
            });
            commands.trigger(jackdaw_feathers::dialog::OpenDialogEvent::new(
                "Extract to File",
                "Save",
            ));
        }
        "hierarchy.prefab.unbundle_instance" => {
            let Some(target) = target_entity else {
                return;
            };
            commands.queue(move |world: &mut World| {
                crate::instances::unbundle_instance(world, target);
            });
        }
        action if action.starts_with(OP_PREFIX) => {
            // Extension-contributed Add entry. Dispatch through the same
            // path as the toolbar Add menu and the Add Entity picker so
            // operators behave identically regardless of which surface
            // invoked them.
            let operator_id = action.strip_prefix(OP_PREFIX).unwrap().to_string();
            commands.queue(move |world: &mut World| {
                world
                    .operator(operator_id)
                    .settings(CallOperatorSettings {
                        execution_context: ExecutionContext::Invoke,
                        creates_history_entry: true,
                    })
                    .call()
            });
        }
        _ => {}
    }
}

/// Spawn an entity from `template` and reparent it under `parent` (if
/// provided). Goes through the AST-aware `set_parent` so the live
/// scene document stays in sync with the ECS hierarchy.
fn add_child_entity(
    commands: &mut Commands,
    parent: Option<Entity>,
    template: entity_ops::EntityTemplate,
) {
    let Some(parent) = parent else {
        return;
    };
    commands.queue(move |world: &mut World| {
        entity_ops::create_entity_in_world(world, template);
        let selection = world.resource::<Selection>();
        if let Some(new_entity) = selection.primary() {
            crate::commands::set_parent(world, new_entity, Some(parent));
        }
    });
}

/// Toggle entity visibility when the eye icon is clicked. This is an
/// editor-local view state: it sets ECS `Visibility` only and is never written
/// to the `.jsn` scene, so it does not re-apply on rebuild. The eye glyph is
/// synced to match so hidden state is always visible.
fn on_visibility_toggled(
    event: On<TreeRowVisibilityToggled>,
    mut commands: Commands,
    visibility_query: Query<&Visibility>,
) {
    let source = event.source_entity;

    let current = visibility_query
        .get(source)
        .copied()
        .unwrap_or(Visibility::Inherited);

    let new_visibility = match current {
        Visibility::Hidden => Visibility::Inherited,
        _ => Visibility::Hidden,
    };
    // The row's eye follows `Visibility` on its own.
    commands.queue(move |world: &mut World| {
        if let Ok(mut ec) = world.get_entity_mut(source) {
            ec.insert(new_visibility);
        }
    });
}

/// Lock or unlock an entity from its row, and write the answer into the
/// document. The lock is scene data rather than a view state, so it comes back
/// on a reload.
fn on_lock_toggled(event: On<TreeRowLockToggled>, mut commands: Commands) {
    let source = event.source_entity;
    commands.queue(move |world: &mut World| {
        let locked = world.get::<jackdaw_scene_types::Locked>(source).is_none();
        let mut command = SetLocked {
            entity: source,
            locked,
        };
        command.execute(world);
        world
            .resource_mut::<CommandHistory>()
            .push_executed(Box::new(command));
    });
}

/// Undoable lock or unlock of one node. The lock is document data, so it
/// belongs on the undo stack beside every other document edit.
struct SetLocked {
    entity: Entity,
    locked: bool,
}

impl EditorCommand for SetLocked {
    fn execute(&mut self, world: &mut World) {
        set_locked(world, self.entity, self.locked);
    }

    fn undo(&mut self, world: &mut World) {
        set_locked(world, self.entity, !self.locked);
    }

    fn description(&self) -> &str {
        if self.locked { "Lock" } else { "Unlock" }
    }
}

/// Put `entity` in or out of the canvas's reach, in the ECS and in the
/// document together.
pub fn set_locked(world: &mut World, entity: Entity, locked: bool) {
    if locked {
        if let Ok(mut entity_mut) = world.get_entity_mut(entity) {
            entity_mut.insert(jackdaw_scene_types::Locked);
        }
    } else {
        if let Ok(mut entity_mut) = world.get_entity_mut(entity) {
            entity_mut.remove::<jackdaw_scene_types::Locked>();
        }
    }
}

/// Move one entity under another, the way dragging its outliner row does.
/// Both targets are named rather than taken from the selection, which cannot
/// say which of the two it means.
#[operator(
    id = "entity.reparent",
    label = "Reparent Entity",
    description = "Move an entity under another entity.",
    allows_undo = false,
    params(
        child(Entity, doc = "Entity to move."),
        parent(Entity, doc = "Entity that adopts it."),
    )
)]
pub(crate) fn entity_reparent(
    params: In<OperatorParameters>,
    parents: Query<&ChildOf>,
    mut commands: Commands,
) -> OperatorResult {
    let child = params.as_entity("child")?;
    let parent = params.as_entity("parent")?;
    if is_at_or_below(&parents, child, parent) {
        // An operator call can name a parent no drag can reach; adopting an
        // own ancestor is a cycle the document has no shape for.
        warn!("entity.reparent: {parent} is inside {child}, so it cannot adopt it");
        return OperatorResult::Cancelled;
    }
    commands.queue(move |world: &mut World| {
        let old_parent = world.get::<ChildOf>(child).map(ChildOf::parent);
        let mut cmd = ReparentEntity {
            entity: child,
            old_parent,
            new_parent: Some(parent),
        };
        cmd.execute(world);
        world
            .resource_mut::<CommandHistory>()
            .push_executed(Box::new(cmd));
    });
    OperatorResult::Finished
}

/// Whether `candidate` is `ancestor` itself or sits somewhere below it.
fn is_at_or_below(parents: &Query<&ChildOf>, ancestor: Entity, candidate: Entity) -> bool {
    let mut current = candidate;
    loop {
        if current == ancestor {
            return true;
        }
        match parents.get(current) {
            Ok(parent) => current = parent.parent(),
            Err(_) => return false,
        }
    }
}

pub(crate) fn add_to_extension(ctx: &mut ExtensionContext) {
    ctx.register_operator::<EntityReparentOp>()
        .register_operator::<RenameBeginOp>()
        .register_operator::<HierarchyOpenContextMenuOp>()
        .register_operator::<PrefabSaveAsPrefabOp>();
    let ext = ctx.id();
    // Deferred: condition is not bare Press::default() (mouse button + Press).
    ctx.spawn((
        Action::<HierarchyOpenContextMenuOp>::new(),
        ActionOf::<crate::core_extension::CoreExtensionInputContext>::new(ext),
        bindings![(MouseButton::Right, Press::default())],
    ));
    ctx.bind_operator::<crate::core_extension::CoreExtensionInputContext, RenameBeginOp>([
        PresetInput::key("F2"),
    ]);
}

/// Marker for inline rename `text_edit` entity, linking back to the label entity and source entity.
#[derive(Component)]
struct InlineRenameInput {
    label_entity: Entity,
    source_entity: Entity,
}

fn on_tree_row_start_rename(event: On<TreeRowStartRename>, mut commands: Commands) {
    let target = event.source_entity;
    commands
        .operator(RenameBeginOp::ID)
        .param("entity", target)
        .call();
}

/// `is_available` for `hierarchy.rename_begin`: only fires when no
/// inline rename is already in progress.
fn no_rename_in_progress(rename_check: Query<(), With<InlineRenameInput>>) -> bool {
    rename_check.is_empty()
}

/// Pick the entity to rename: the explicit `entity` operator
/// parameter wins (used by the context-menu "Rename" action and the
/// `TreeRowStartRename` event), otherwise fall back to the primary
/// selection so a bare F2 press renames whatever the user has
/// highlighted in the outliner. Pulled out so the regression check
/// for the F2-without-selection path can run as a unit test.
pub(crate) fn resolve_rename_target(
    params: &OperatorParameters,
    selection: &Selection,
) -> Option<Entity> {
    params.as_entity("entity").or_else(|| selection.primary())
}

fn entity_name(names: &Query<&Name>, entity: Entity) -> String {
    names
        .get(entity)
        .map(|n| n.as_str().to_string())
        .unwrap_or_default()
}

/// The label of a scene entity's tree row, the row content holding it, and the
/// slot it occupies among that content's children. With several Outliner panels
/// mounted, this is the first match across all of them.
struct RenameTarget {
    label: Entity,
    content: Entity,
    /// Where the label sits among the row's children, so the entry can take
    /// that place rather than being appended past the lock and the eye.
    slot: usize,
}

fn find_rename_targets(
    source: Entity,
    tree_index: &TreeIndex,
    tree_nodes: &Query<&Children, With<TreeNode>>,
    content_query: &Query<(Entity, &Children), With<TreeRowContent>>,
    label_query: &Query<Entity, With<TreeRowLabel>>,
) -> Option<RenameTarget> {
    for (_container, tree_entity) in tree_index.rows_for_source(source) {
        let Ok(children) = tree_nodes.get(tree_entity) else {
            continue;
        };
        for child in children.iter() {
            if let Ok((content, content_children)) = content_query.get(child) {
                for (slot, grandchild) in content_children.iter().enumerate() {
                    if label_query.contains(grandchild) {
                        return Some(RenameTarget {
                            label: grandchild,
                            content,
                            slot,
                        });
                    }
                }
            }
        }
    }
    None
}

/// Custom command: drop the inline-rename marker from a tree-row label
/// and restore its displayed text + visibility. Issued from rename
/// commit/cancel paths so the queue boundary is explicit.
struct RestoreLabel {
    label_entity: Entity,
    text: String,
}

impl Command for RestoreLabel {
    type Out = ();

    fn apply(self, world: &mut World) -> Self::Out {
        let Ok(mut ec) = world.get_entity_mut(self.label_entity) else {
            return;
        };
        ec.remove::<TreeRowInlineRename>();
        ec.insert(Text::new(self.text));
        if let Some(mut node) = ec.get_mut::<Node>() {
            node.display = Display::Flex;
        }
    }
}

/// Begin inline rename of an entity in the hierarchy tree.
#[operator(
    id = "hierarchy.rename_begin",
    label = "Rename Entity",
    description = "Rename the selected entity in the hierarchy.",
    modal = true,
    cancel = cancel_rename_begin,
    is_available = no_rename_in_progress,
    params(entity(Entity, doc = "Scene entity to rename.")),
)]
pub fn rename_begin(
    params: In<OperatorParameters>,
    mut commands: Commands,
    tree_index: Res<TreeIndex>,
    tree_nodes: Query<&Children, With<TreeNode>>,
    content_query: Query<(Entity, &Children), With<TreeRowContent>>,
    label_query: Query<Entity, With<TreeRowLabel>>,
    names: Query<&Name>,
    rename_inputs: Query<(), With<InlineRenameInput>>,
    active: ActiveModalQuery,
    selection: Res<Selection>,
) -> OperatorResult {
    if active.is_modal_running() {
        return if rename_inputs.is_empty() {
            OperatorResult::Finished
        } else {
            OperatorResult::Running
        };
    }

    let source = resolve_rename_target(&params, &selection)?;
    let target = find_rename_targets(
        source,
        &tree_index,
        &tree_nodes,
        &content_query,
        &label_query,
    )?;
    let label_entity = target.label;

    commands.entity(label_entity).insert(TreeRowInlineRename);
    commands
        .entity(label_entity)
        .entry::<Node>()
        .and_modify(|mut node| {
            node.display = Display::None;
        });

    let name = entity_name(&names, source);
    let entry = commands
        .spawn((
            InlineRenameInput {
                label_entity,
                source_entity: source,
            },
            text_edit::text_edit(
                TextEditProps::default()
                    .with_default_value(name)
                    .select_all_on_open()
                    .allow_empty(),
            ),
        ))
        .id();
    // The label's own slot and width: an entry appended to the row lands past
    // the lock and the eye, in a box too small to read a name out of.
    commands
        .entity(entry)
        .entry::<Node>()
        .and_modify(|mut node| {
            node.min_width = px(jackdaw_feathers::tree_view::LABEL_MIN_WIDTH);
            node.margin.left = px(jackdaw_feathers::tokens::SPACING_SM);
        });
    commands
        .entity(target.content)
        .insert_children(target.slot, &[entry]);
    OperatorResult::Running
}

fn cancel_rename_begin(
    mut commands: Commands,
    rename_query: Query<(Entity, &InlineRenameInput)>,
    names: Query<&Name>,
    mut input_focus: ResMut<InputFocus>,
) {
    for (rename_entity, inline_rename) in &rename_query {
        input_focus.clear();
        let original = entity_name(&names, inline_rename.source_entity);
        commands.queue(RestoreLabel {
            label_entity: inline_rename.label_entity,
            text: original,
        });
        commands.entity(rename_entity).despawn();
    }
}

/// Auto-focus inline rename `text_edit` inputs one frame after spawn.
///
/// The name the entry opened on is already selected by then; see
/// `TextEditProps::select_all_on_open`.
fn auto_focus_inline_rename(
    rename_inputs: Query<(Entity, &InlineRenameInput, &Children)>,
    wrappers: Query<&jackdaw_feathers::text_edit::TextEditConfig>,
    wrapper_children: Query<&Children>,
    editor_text_edits: Query<Entity, With<EditorTextEdit>>,
    mut input_focus: ResMut<InputFocus>,
) {
    for (_rename_entity, _inline, children) in &rename_inputs {
        // The text_edit outer entity has children: [wrapper] which has children: [..., EditorTextEdit]
        for child in children.iter() {
            if wrappers.contains(child) {
                // this is the label/wrapper -- skip, we need the actual wrapper node
                continue;
            }
            // child might be the wrapper entity (has TextEditWrapper inside)
            if let Ok(wrapper_kids) = wrapper_children.get(child) {
                for wk in wrapper_kids.iter() {
                    if editor_text_edits.contains(wk) {
                        if input_focus.get() != Some(wk) {
                            input_focus.set(wk, FocusCause::Pressed);
                        }
                        return;
                    }
                }
            }
        }
    }
}

/// Handle `TextEditCommitEvent` for inline renames.
fn handle_inline_rename_commit(
    event: On<TextEditCommitEvent>,
    rename_inputs: Query<(Entity, &InlineRenameInput)>,
    child_of_query: Query<&ChildOf>,
    mut commands: Commands,
    mut input_focus: ResMut<InputFocus>,
) {
    // Walk up from the committed entity to find if it belongs to an InlineRenameInput
    // event.entity is the inner EditorTextEdit -> parent is wrapper -> parent is text_edit outer -> parent is content
    // The InlineRenameInput is on the text_edit outer entity
    let mut current = event.entity;
    let mut found = None;
    for _ in 0..4 {
        let Ok(child_of) = child_of_query.get(current) else {
            break;
        };
        if let Ok((rename_entity, inline_rename)) = rename_inputs.get(child_of.parent()) {
            found = Some((
                rename_entity,
                inline_rename.label_entity,
                inline_rename.source_entity,
            ));
            break;
        }
        current = child_of.parent();
    }

    let Some((rename_entity, label_entity, source_entity)) = found else {
        return;
    };

    input_focus.clear();
    commands.queue(RestoreLabel {
        label_entity,
        text: event.text.clone(),
    });
    commands.entity(rename_entity).despawn();

    // Trigger the rename
    commands.trigger(TreeRowRenamed {
        entity: label_entity,
        source_entity,
        new_name: event.text.clone(),
    });
}

/// Commit inline rename: update Name with undo.
fn on_tree_row_renamed(event: On<TreeRowRenamed>, mut commands: Commands, names: Query<&Name>) {
    let source = event.source_entity;
    let new_name = event.new_name.clone();

    // Apply name change with undo
    let old_name = names
        .get(source)
        .map(|n| n.as_str().to_string())
        .unwrap_or_default();

    if old_name == new_name {
        return;
    }

    commands.queue(move |world: &mut World| {
        let old_value: Option<crate::commands::FieldValue> =
            (!old_name.is_empty()).then(|| Box::new(Name::new(old_name)) as _);
        let cmd = SetField {
            entity: source,
            type_path: crate::commands::NAME_TYPE_PATH.to_string(),
            field_path: String::new(),
            old_value,
            new_value: Box::new(Name::new(new_name)),
        };
        let mut cmd = Box::new(cmd);
        cmd.execute(world);
        let mut history = world.resource_mut::<CommandHistory>();
        history.push_executed(cmd);
    });
}

/// When the prefab dialog opens, populate its children slot with a name input.
/// The slot is spawned without a `Children` component (Bevy only adds it on
/// first parenting), so this query MUST NOT require `&Children` -- it would
/// never match a fresh slot.
fn populate_prefab_dialog(
    mut commands: Commands,
    pending: Res<PendingPrefabSave>,
    default_name: Res<PendingPrefabDefaultName>,
    slots: Query<Entity, With<DialogChildrenSlot>>,
    existing_inputs: Query<(), With<PrefabNameInput>>,
) {
    if pending.roots.is_empty() {
        return;
    }
    // Idempotent: once the input exists, subsequent ticks bail here.
    if !existing_inputs.is_empty() {
        return;
    }
    for slot_entity in &slots {
        commands.spawn((
            PrefabNameInput,
            text_edit::text_edit(
                TextEditProps::default()
                    .with_placeholder("Prefab name...")
                    .with_default_value(default_name.0.clone())
                    .allow_empty(),
            ),
            ChildOf(slot_entity),
        ));
    }
}

/// When the dialog's action button is clicked, dispatch the matching
/// prefab save operator. Routing through the operator system gives the
/// save a log entry, an extension-API surface, and a consistent
/// invocation path with the rest of the editor.
fn on_prefab_dialog_action(
    _event: On<DialogActionEvent>,
    mut commands: Commands,
    pending: Res<PendingPrefabSave>,
    name_inputs: Query<&TextEditValue, With<PrefabNameInput>>,
) {
    if pending.roots.is_empty() {
        return;
    }
    let name = name_inputs
        .iter()
        .next()
        .map(|input| input.0.trim().to_string())
        .unwrap_or_default();
    if name.is_empty() {
        warn!("save prefab cancelled: name is empty");
        return;
    }
    commands
        .operator(PrefabSaveAsPrefabOp::ID)
        .settings(CallOperatorSettings {
            creates_history_entry: true,
            ..default()
        })
        .param("name", name)
        .call();
}

/// Save the entities listed in `PendingPrefabSave` as a new prefab file
/// at `assets/prefabs/<name>.jsn` (project-relative). Clears the
/// pending state after running.
#[operator(
    id = "prefab.save_as_prefab",
    label = "Save as Prefab",
    description = "Write the pending entity roots out as a new prefab file.",
    allows_undo = true,
    params(name(String, doc = "File name (without extension)."))
)]
pub fn prefab_save_as_prefab(
    params: In<OperatorParameters>,
    mut commands: Commands,
) -> OperatorResult {
    let Some(name) = params.as_str("name").map(str::to_string) else {
        warn!("prefab.save_as_prefab: missing `name` param");
        return OperatorResult::Cancelled;
    };
    commands.queue(move |world: &mut World| {
        let roots = world.resource::<PendingPrefabSave>().roots.clone();
        if roots.is_empty() {
            warn!("prefab.save_as_prefab: no pending roots");
            return;
        }
        let target = match world.get_resource::<crate::project::ProjectRoot>() {
            Some(root) => root.root.join("assets/prefabs").join(format!("{name}.bsn")),
            None => std::path::PathBuf::from(format!("{name}.bsn")),
        };
        info!(
            "prefab.save_as_prefab: bundling {} root(s) into {}",
            roots.len(),
            target.display()
        );
        if let Err(err) = crate::instances::save_as_instance(world, &roots, &target) {
            crate::status_bar::notify_error(world, format!("{}: {err}", target.display()));
        }
        world.resource_mut::<PendingPrefabSave>().roots.clear();
    });
    OperatorResult::Finished
}

/// Toggle the show-all state when the button is clicked.
fn toggle_show_all_button(
    click: On<ButtonClickEvent>,
    buttons: Query<(), With<HierarchyShowAllButton>>,
    mut show_all: ResMut<HierarchyShowAll>,
) {
    if buttons.contains(click.entity) {
        show_all.0 = !show_all.0;
    }
}

/// Update the show-all button icon color based on active state.
fn update_show_all_button_appearance(
    show_all: Res<HierarchyShowAll>,
    buttons: Query<&Children, With<HierarchyShowAllButton>>,
    mut text_colors: Query<&mut TextColor>,
) {
    if !show_all.is_changed() {
        return;
    }
    let color = if show_all.0 {
        tokens::TEXT_PRIMARY
    } else {
        tokens::TEXT_SECONDARY
    };
    for children in &buttons {
        for child in children.iter() {
            if let Ok(mut tc) = text_colors.get_mut(child) {
                tc.0 = color;
            }
        }
    }
}

/// Despawn every Outliner panel's rows and reset the `TreeIndex`; the lists build again from
/// the world. Used by scene switches and similar full rebuilds.
pub fn clear_all_tree_rows(
    world: &mut World,
    _containers: &mut QueryState<Entity, With<HierarchyTreeContainer>>,
) {
    view::reset_views(world);
}

/// Input actions and their bindings are the host's machinery, never scene content.
fn hide_input_action(add: On<Add<ActionValue>>, mut commands: Commands) {
    commands.entity(add.entity).try_insert(EditorHidden);
}

fn hide_input_binding(add: On<Add<Binding>>, mut commands: Commands) {
    commands.entity(add.entity).try_insert(EditorHidden);
}
