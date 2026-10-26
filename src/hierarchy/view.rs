//! The Outliner's rows as a virtual list.
//!
//! Each panel ([`HierarchyTreeContainer`]) keeps the rows it shows as a flat list
//! ([`OutlinerView::rows`]): every root, and under every open row its children, each with
//! its depth. Only the rows in view (and a few either side) exist as UI: a pool of
//! [`tree_row`]s absolutely placed in the panel's body, rebound to other entities as the
//! panel scrolls. The panel scrolls itself: layout only clips it, the offset is its
//! `ScrollPosition` (so reveal and drag auto-scroll write it as they would any panel's), and
//! the bar on its right edge is drawn here from the same figures the rows are placed by. A node with fifty thousand children is fifty thousand
//! entries in a `Vec`, not fifty thousand rows of UI.
//!
//! The child lists come from [`OutlinerTree`], a cache of each open node's ordered,
//! filtered children, dropped by the observers in this file whenever something that decides
//! one changes. Each frame, panels whose lists went stale flatten again, and every bound row
//! is synced from its entity (label, glyph, selection, lock, eye), writing only what differs.

use std::collections::{HashMap, HashSet};

use avian3d::environment::PhysicsEnvironment;
use bevy::{input_focus::InputFocus, prelude::*, ui::UiGlobalTransform};
use jackdaw_api_internal::entity_icons::registered_icon;
use jackdaw_feathers::{
    icons::{Icon, IconFont},
    tokens,
    tree_view::{
        INDENT_STEP, LOCK_IDLE_ALPHA, ROW_BG, ROW_HEIGHT, TreeRowLabelFit, TreeRowStyle,
        category_color, category_icon, set_row_expand_toggle, tree_row,
    },
};
use jackdaw_scene_types::{Locked, SceneRootTag, UiSceneRoot};
use jackdaw_widgets::tree_view::{
    TreeIndex, TreeNode, TreeNodeExpanded, TreeRowContent, TreeRowDepth, TreeRowDot,
    TreeRowInlineRename, TreeRowLabel, TreeRowLockToggle, TreeRowSelected,
    TreeRowVisibilityToggle,
};

use super::{
    HierarchyShowAll, HierarchyTreeContainer, InlineRenameInput, classify_entity,
    is_inherited_descendant, is_outliner_child, is_outliner_root, names_a_prefab, row_label,
};
use crate::{
    EditorEntity, EditorHidden,
    scene_io::{SceneEntity, SceneRootOf},
    selection::Selection,
};

/// Rows bound beyond each edge of the view, so a scroll of a few rows shows rows already
/// laid out.
const OVERSCAN: usize = 8;

/// How tall a panel is taken to be before it has been laid out (and in a headless app,
/// where it never is): enough rows to work with.
const UNLAID_VIEW_HEIGHT: f32 = 1200.0;

/// One row of a panel's list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OutlinerRow {
    pub source: Entity,
    pub depth: usize,
    /// Whether it has children the Outliner would show (its disclosure).
    pub has_children: bool,
    /// Whether its children are listed under it.
    pub open: bool,
}

/// A panel's list and the row widgets drawing the part of it in view.
#[derive(Component, Default)]
pub struct OutlinerView {
    expanded: HashSet<Entity>,
    rows: Vec<OutlinerRow>,
    position: HashMap<Entity, usize>,
    /// The area the rows are drawn in (the panel less the bar's gutter); their parent.
    spacer: Option<Entity>,
    /// The scrollbar's track and thumb.
    bar: Option<(Entity, Entity)>,
    /// Row widgets and the entity each is bound to.
    pool: Vec<(Entity, Option<Entity>)>,
    /// The panel's filter text, lowercased; empty lists the tree.
    filter: String,
    /// Scroll this entity's row into view once it is listed (its branches are opened).
    reveal: Option<Entity>,
    stale: bool,
}

impl OutlinerView {
    /// Every row of the list, top to bottom, whether in view or not.
    pub fn rows(&self) -> &[OutlinerRow] {
        &self.rows
    }

    /// Where `source` is in the list.
    pub fn position(&self, source: Entity) -> Option<usize> {
        self.position.get(&source).copied()
    }

    pub fn is_expanded(&self, source: Entity) -> bool {
        self.expanded.contains(&source)
    }
}

/// The ordered, filtered children of every node a panel lists open, and the disclosure of
/// every node a panel lists, shared by all panels. An entry is dropped by whatever changes
/// it, and only nodes some panel shows are kept, so the observers only need to watch those.
#[derive(Resource, Default)]
pub struct OutlinerTree {
    children: HashMap<Entity, Vec<Entity>>,
    has_children: HashMap<Entity, bool>,
    roots: Option<Vec<Entity>>,
    /// Sources listed in any panel.
    shown: HashSet<Entity>,
    /// Sources listed open in any panel.
    open: HashSet<Entity>,
    /// Some panel must flatten again.
    dirty: bool,
}

impl OutlinerTree {
    fn watches(&self, entity: Entity) -> bool {
        self.shown.contains(&entity) || self.open.contains(&entity)
    }

    /// `parent`'s child list (the roots for `None`) is out of date.
    fn drop_list(&mut self, parent: Option<Entity>) {
        match parent {
            Some(parent) => {
                self.children.remove(&parent);
                self.has_children.remove(&parent);
            }
            None => self.roots = None,
        }
        self.dirty = true;
    }

    /// Everything is out of date.
    pub(crate) fn drop_all(&mut self) {
        self.children.clear();
        self.has_children.clear();
        self.roots = None;
        self.dirty = true;
    }
}

/// `entity` changed in a way its siblings' list (its place in it, or whether it is in it)
/// may depend on: drop the list it is in, when a panel shows it or its parent is open.
pub(super) fn drop_list_of(world: &mut World, entity: Entity) {
    let parent = world.get::<ChildOf>(entity).map(ChildOf::parent);
    let root = parent.is_none_or(|parent| world.get::<PhysicsEnvironment>(parent).is_some())
        || world.get::<SceneRootOf>(entity).is_some();
    let mut tree = world.resource_mut::<OutlinerTree>();
    if root {
        tree.drop_list(None);
    }
    if let Some(parent) = parent
        && (tree.watches(parent) || tree.shown.contains(&entity))
    {
        tree.drop_list(Some(parent));
    }
}

/// `parent`'s child list (the roots for `None`) is out of date in every panel.
pub(super) fn drop_child_list(world: &mut World, parent: Option<Entity>) {
    if let Some(mut tree) = world.get_resource_mut::<OutlinerTree>() {
        tree.drop_list(parent);
    }
}

pub(crate) fn plugin(app: &mut App) {
    app.init_resource::<OutlinerTree>()
        .init_resource::<OutlinerFocus>()
        .add_observer(on_container_added)
        .add_observer(scroll_on_wheel)
        .add_observer(on_parent_set)
        .add_observer(on_parent_removed)
        .add_observer(|add: On<Add<Name>>, mut commands: Commands| {
            let entity = add.event_target();
            commands.queue(move |world: &mut World| drop_list_of(world, entity));
        })
        .add_observer(on_name_removed)
        .add_observer(|add: On<Add<Transform>>, children: Query<(), With<ChildOf>>, mut tree: ResMut<OutlinerTree>| {
            if !children.contains(add.event_target()) {
                tree.drop_list(None);
            }
        })
        .add_observer(|_: On<Add<UiSceneRoot>>, mut tree: ResMut<OutlinerTree>| {
            tree.drop_list(None);
        })
        .add_observer(|_: On<Insert<SceneRootOf>>, mut tree: ResMut<OutlinerTree>| {
            tree.drop_list(None);
        })
        .add_observer(|_: On<Remove<SceneRootOf>>, mut tree: ResMut<OutlinerTree>| {
            tree.drop_list(None);
        })
        .add_observer(drops_list_on_add::<EditorHidden>)
        .add_observer(drops_list_on_add::<EditorEntity>)
        .add_observer(drops_list_on_add::<SceneEntity>)
        .add_observer(drops_list_on_add::<bevy::scene::SceneBase>)
        .add_observer(drops_list_on_add::<SceneRootTag>)
        .add_observer(drops_list_on_add::<Camera>)
        .add_observer(drops_list_on_add::<PointLight>)
        .add_observer(drops_list_on_add::<DirectionalLight>)
        .add_observer(drops_list_on_add::<SpotLight>)
        .add_observer(drops_list_on_add::<aurora::mesh::AuroraMesh3d>)
        .add_systems(
            PostUpdate,
            (
                watch_filter,
                watch_show_all,
                reveal_selection,
                flatten_views,
                bind_rows,
            )
                .chain()
                .after(jackdaw_widgets::tree_view::maintain_tree_index)
                .before(bevy::ui::UiSystems::Layout),
        )
        .add_systems(
            Update,
            outliner_keyboard_navigation.run_if(crate::editor_active),
        );
}

fn drops_list_on_add<C: Component>(add: On<Add<C>>, mut commands: Commands) {
    let entity = add.event_target();
    commands.queue(move |world: &mut World| drop_list_of(world, entity));
}

/// The scrollbar's width, and the gutter on the panel's right it sits in.
const BAR_WIDTH: f32 = 6.0;
const BAR_GUTTER: f32 = BAR_WIDTH + 4.0;
const BAR_MIN_THUMB: f32 = 24.0;

/// A panel's scrollbar track; the thumb is its child.
#[derive(Component)]
struct OutlinerBar {
    container: Entity,
}

/// A panel gets its body (where the row widgets are drawn) and its scrollbar.
fn on_container_added(
    add: On<Add<HierarchyTreeContainer>>,
    mut commands: Commands,
    mut tree: ResMut<OutlinerTree>,
) {
    use bevy::feathers::{theme::ThemeBackgroundColor, tokens as feathers_tokens};
    let container = add.event_target();
    let body = commands
        .spawn((
            Node {
                position_type: PositionType::Absolute,
                left: px(0.0),
                top: px(0.0),
                bottom: px(0.0),
                right: px(BAR_GUTTER),
                overflow: Overflow::clip(),
                ..default()
            },
            Pickable::IGNORE,
            ChildOf(container),
        ))
        .id();
    let track = commands
        .spawn((
            OutlinerBar { container },
            Node {
                position_type: PositionType::Absolute,
                top: px(2.0),
                bottom: px(2.0),
                right: px(2.0),
                width: px(BAR_WIDTH),
                border_radius: BorderRadius::all(px(BAR_WIDTH / 2.0)),
                ..default()
            },
            ThemeBackgroundColor(feathers_tokens::SCROLLBAR_BG),
            Visibility::Hidden,
            ChildOf(container),
        ))
        .observe(press_bar_track)
        .id();
    let thumb = commands
        .spawn((
            Node {
                position_type: PositionType::Absolute,
                left: px(0.0),
                right: px(0.0),
                top: px(0.0),
                height: px(BAR_MIN_THUMB),
                border_radius: BorderRadius::all(px(BAR_WIDTH / 2.0)),
                ..default()
            },
            ThemeBackgroundColor(feathers_tokens::SCROLLBAR_THUMB),
            ChildOf(track),
        ))
        .observe(drag_bar_thumb)
        .id();
    commands.entity(container).insert((
        OutlinerView {
            spacer: Some(body),
            bar: Some((track, thumb)),
            stale: true,
            ..default()
        },
        jackdaw_widgets::tree_view::TreeScrollExtent::default(),
    ));
    tree.dirty = true;
}

/// The wheel over a panel scrolls its list.
fn scroll_on_wheel(mut wheel: On<crate::Scroll>, mut panels: Query<&mut ScrollPosition, With<OutlinerView>>) {
    let Ok(mut scroll) = panels.get_mut(wheel.event_target()) else {
        return;
    };
    if wheel.delta.y == 0.0 {
        return;
    }
    scroll.y = (scroll.y + wheel.delta.y).max(0.0);
    wheel.delta.y = 0.0;
    if wheel.delta == Vec2::ZERO {
        wheel.propagate(false);
    }
}

/// Dragging the thumb moves the view by as much of the list as the thumb moves of the track.
fn drag_bar_thumb(
    mut drag: On<PointerDrag>,
    parents: Query<&ChildOf>,
    bars: Query<&OutlinerBar>,
    computed: Query<&ComputedNode>,
    mut panels: Query<(&OutlinerView, &mut ScrollPosition)>,
) {
    drag.propagate(false);
    let Ok(&ChildOf(track)) = parents.get(drag.event_target()) else {
        return;
    };
    let (Ok(bar), Ok(track_node)) = (bars.get(track), computed.get(track)) else {
        return;
    };
    let Ok((view, mut scroll)) = panels.get_mut(bar.container) else {
        return;
    };
    let track_length = track_node.size().y * track_node.inverse_scale_factor();
    let total = view.rows.len() as f32 * ROW_HEIGHT;
    if track_length <= 0.0 {
        return;
    }
    scroll.y = (scroll.y + drag.delta.y * total / track_length).max(0.0);
}

/// A press on the track (not the thumb) centres the view on the part of the list there.
fn press_bar_track(
    mut press: On<PointerPress>,
    bars: Query<(&OutlinerBar, &ComputedNode, &UiGlobalTransform)>,
    mut panels: Query<(&OutlinerView, &mut ScrollPosition, &ComputedNode)>,
) {
    press.propagate(false);
    let Ok((bar, track, transform)) = bars.get(press.event_target()) else {
        return;
    };
    let Ok((view, mut scroll, panel)) = panels.get_mut(bar.container) else {
        return;
    };
    let scale = track.inverse_scale_factor();
    let top = (transform.translation.y - track.size().y / 2.0) * scale;
    let length = track.size().y * scale;
    if length <= 0.0 {
        return;
    }
    let at = ((press.pointer.position.y - top) / length).clamp(0.0, 1.0);
    let total = view.rows.len() as f32 * ROW_HEIGHT;
    let view_height = panel.size().y * panel.inverse_scale_factor();
    scroll.y = (at * total - view_height / 2.0).max(0.0);
}

fn on_parent_set(
    insert: On<Insert<ChildOf>>,
    parents: Query<&ChildOf>,
    rows: Query<(), With<TreeNode>>,
    mut tree: ResMut<OutlinerTree>,
) {
    let entity = insert.event_target();
    if rows.contains(entity) {
        return;
    }
    let Ok(&ChildOf(parent)) = parents.get(entity) else {
        return;
    };
    if tree.shown.contains(&entity) {
        // It may have left the roots, or another open list.
        tree.drop_list(None);
        tree.dirty = true;
    }
    if tree.watches(parent) {
        tree.drop_list(Some(parent));
    }
}

fn on_parent_removed(
    remove: On<Discard<ChildOf>>,
    parents: Query<&ChildOf>,
    rows: Query<(), With<TreeNode>>,
    mut tree: ResMut<OutlinerTree>,
) {
    let entity = remove.event_target();
    if rows.contains(entity) {
        return;
    }
    let Ok(&ChildOf(parent)) = parents.get(entity) else {
        return;
    };
    if tree.watches(parent) {
        tree.drop_list(Some(parent));
    }
    if tree.shown.contains(&entity) {
        tree.drop_list(None);
    }
}

fn on_name_removed(
    remove: On<Remove<Name>>,
    parents: Query<&ChildOf>,
    mut tree: ResMut<OutlinerTree>,
) {
    let entity = remove.event_target();
    if !tree.shown.contains(&entity) {
        return;
    }
    tree.drop_list(None);
    if let Ok(&ChildOf(parent)) = parents.get(entity) {
        tree.drop_list(Some(parent));
    }
}

/// Open or close `source` in `view`; whether that changed anything.
fn set_expanded(view: &mut OutlinerView, source: Entity, open: bool) -> bool {
    let changed = if open {
        view.expanded.insert(source)
    } else {
        view.expanded.remove(&source)
    };
    if changed {
        view.stale = true;
    }
    changed
}

/// Open or close `source` in `container`'s panel.
pub fn set_outliner_expanded(world: &mut World, container: Entity, source: Entity, open: bool) {
    let Some(mut view) = world.get_mut::<OutlinerView>(container) else {
        return;
    };
    if set_expanded(&mut view, source, open) {
        world.resource_mut::<OutlinerTree>().dirty = true;
    }
}

fn watch_filter(
    inputs: Query<
        &jackdaw_feathers::text_edit::TextEditValue,
        (
            With<crate::layout::HierarchyFilter>,
            Changed<jackdaw_feathers::text_edit::TextEditValue>,
        ),
    >,
    mut views: Query<&mut OutlinerView>,
    mut tree: ResMut<OutlinerTree>,
) {
    let Ok(value) = inputs.single() else {
        return;
    };
    let filter = value.0.trim().to_lowercase();
    for mut view in &mut views {
        if view.filter != filter {
            view.filter.clone_from(&filter);
            view.stale = true;
            tree.dirty = true;
        }
    }
}

fn watch_show_all(show_all: Res<HierarchyShowAll>, mut tree: ResMut<OutlinerTree>) {
    if show_all.is_changed() && !show_all.is_added() {
        tree.drop_all();
    }
}

/// The primary selection is revealed in every panel: its branches opened, its row
/// scrolled into view.
fn reveal_selection(
    selection: Res<Selection>,
    mut views: Query<&mut OutlinerView>,
    mut tree: ResMut<OutlinerTree>,
) {
    if !selection.is_changed() {
        return;
    }
    let Some(primary) = selection.primary() else {
        return;
    };
    for mut view in &mut views {
        view.reveal = Some(primary);
    }
    tree.dirty = true;
}

/// Ask every panel to bring `source`'s row into view, opening the branches above it.
pub fn reveal_in_outliner(world: &mut World, source: Entity) {
    let mut views = world.query::<&mut OutlinerView>();
    for mut view in views.iter_mut(world) {
        view.reveal = Some(source);
    }
    world.resource_mut::<OutlinerTree>().dirty = true;
}

/// Whether `child` is listed under its parent: an Outliner child with a name to show
/// (or any, with Show All; an instance that inherited no name still shows).
fn is_listed_child(world: &World, child: Entity) -> bool {
    is_outliner_child(world, child)
        && (world.resource::<HierarchyShowAll>().0
            || world.get::<Name>(child).is_some()
            || names_a_prefab(world, child))
}

/// The children `entity`'s row lists, in the order they are drawn: the document's order
/// under an authored node, and by kind then name under anything else.
fn child_list(world: &World, entity: Entity) -> Vec<Entity> {
    let mut children: Vec<Entity> = world
        .get::<Children>(entity)
        .map(|children| {
            children
                .iter()
                .filter(|&child| is_listed_child(world, child))
                .collect()
        })
        .unwrap_or_default();
    if world.get::<SceneEntity>(entity).is_none() {
        sort_by_kind_and_name(world, &mut children);
    }
    children
}

fn sort_by_kind_and_name(world: &World, entities: &mut [Entity]) {
    let mut keyed: Vec<(_, String, Entity)> = entities
        .iter()
        .map(|&entity| (classify_entity(world, entity), row_label(world, entity), entity))
        .collect();
    keyed.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
    for (slot, (_, _, entity)) in entities.iter_mut().zip(keyed) {
        *slot = entity;
    }
}

/// The active tab's top-level rows: the document's roots in its order, then anything else
/// at the top by kind and name. Unnamed roots only with Show All (an instance that
/// inherited no name still shows).
fn root_list(world: &mut World) -> Vec<Entity> {
    let show_all = world.resource::<HierarchyShowAll>().0;
    let mut candidates: Vec<Entity> = match crate::scene_io::scene_environment(world) {
        Some(environment) => world
            .get::<crate::scene_io::SceneRoots>(environment)
            .map(|roots| roots.entities().to_vec())
            .unwrap_or_default(),
        None => world
            .query_filtered::<Entity, (
                Or<(With<Transform>, With<UiSceneRoot>, With<Name>)>,
                Without<ChildOf>,
            )>()
            .iter(world)
            .collect(),
    };
    candidates.retain(|&entity| {
        world.get_entity(entity).is_ok()
            && world.get::<EditorEntity>(entity).is_none()
            && world.get::<EditorHidden>(entity).is_none()
            && is_outliner_root(world, entity)
            && (show_all || world.get::<Name>(entity).is_some() || names_a_prefab(world, entity))
    });
    sort_by_kind_and_name(world, &mut candidates);
    let order = crate::scene_io::scene_roots(world);
    let rank: HashMap<Entity, usize> = order.iter().enumerate().map(|(i, &e)| (e, i)).collect();
    candidates.sort_by_key(|entity| rank.get(entity).copied().unwrap_or(usize::MAX));
    candidates
}

/// Open the branches above `target` in `view`, up to the tab's roots.
fn open_ancestors(world: &World, view: &mut OutlinerView, target: Entity) {
    let mut seen = HashSet::new();
    let mut current = target;
    while let Some(&ChildOf(parent)) = world.get::<ChildOf>(current) {
        if world.get::<PhysicsEnvironment>(parent).is_some() || !seen.insert(parent) {
            break;
        }
        if view.expanded.insert(parent) {
            view.stale = true;
        }
        current = parent;
    }
}

/// Re-flatten every panel whose list went stale.
fn flatten_views(world: &mut World) {
    if !world.resource::<OutlinerTree>().dirty {
        return;
    }
    let mut tree = std::mem::take(&mut *world.resource_mut::<OutlinerTree>());
    tree.dirty = false;
    if tree.roots.is_none() {
        tree.roots = Some(root_list(world));
    }
    let containers: Vec<Entity> = world
        .query_filtered::<Entity, With<OutlinerView>>()
        .iter(world)
        .collect();
    let mut shown = HashSet::new();
    let mut open = HashSet::new();
    for container in containers {
        let Some(mut view) = world.entity_mut(container).take::<OutlinerView>() else {
            continue;
        };
        if let Some(target) = view.reveal {
            open_ancestors(world, &mut view, target);
        }
        // Every panel lists again: another panel's change can drop a list this one shows.
        flatten(world, &mut tree, &mut view);
        for row in &view.rows {
            shown.insert(row.source);
            if row.open {
                open.insert(row.source);
            }
        }
        // A walk standing inside a branch that closed resumes on the nearest row still listed
        // above it: the one that was closed.
        if let Some((focused_in, source)) = world.resource::<OutlinerFocus>().0
            && focused_in == container
            && view.position(source).is_none()
        {
            let mut current = source;
            let resumed = loop {
                match world.get::<ChildOf>(current) {
                    Some(&ChildOf(parent)) if view.position(parent).is_some() => break Some(parent),
                    Some(&ChildOf(parent)) => current = parent,
                    None => break None,
                }
            };
            world.resource_mut::<OutlinerFocus>().0 = resumed.map(|row| (container, row));
        }
        view.stale = true;
        world.entity_mut(container).insert(view);
    }
    // Only what a panel shows is watched, so only that is kept.
    tree.children.retain(|entity, _| open.contains(entity));
    tree.has_children.retain(|entity, _| shown.contains(entity));
    tree.shown = shown;
    tree.open = open;
    let dirty = world.resource::<OutlinerTree>().dirty;
    tree.dirty |= dirty;
    *world.resource_mut::<OutlinerTree>() = tree;
}

fn flatten(world: &World, tree: &mut OutlinerTree, view: &mut OutlinerView) {
    let roots = tree.roots.clone().unwrap_or_default();
    let mut rows = Vec::with_capacity(view.rows.len());
    if view.filter.is_empty() {
        for root in roots {
            list_row(world, tree, view, root, 0, &mut rows);
        }
    } else {
        let filter = view.filter.clone();
        for root in roots {
            list_matches(world, tree, &filter, root, 0, &mut rows);
        }
    }
    view.position = rows
        .iter()
        .enumerate()
        .map(|(i, row)| (row.source, i))
        .collect();
    view.rows = rows;
}

fn has_children(world: &World, tree: &mut OutlinerTree, entity: Entity) -> bool {
    *tree.has_children.entry(entity).or_insert_with(|| {
        world
            .get::<Children>(entity)
            .is_some_and(|children| children.iter().any(|child| is_listed_child(world, child)))
    })
}

fn children_of(world: &World, tree: &mut OutlinerTree, entity: Entity) -> Vec<Entity> {
    tree.children
        .entry(entity)
        .or_insert_with(|| child_list(world, entity))
        .clone()
}

fn list_row(
    world: &World,
    tree: &mut OutlinerTree,
    view: &OutlinerView,
    entity: Entity,
    depth: usize,
    rows: &mut Vec<OutlinerRow>,
) {
    if world.get_entity(entity).is_err() {
        return;
    }
    let has_children = has_children(world, tree, entity);
    let open = has_children && view.expanded.contains(&entity);
    rows.push(OutlinerRow {
        source: entity,
        depth,
        has_children,
        open,
    });
    if open {
        for child in children_of(world, tree, entity) {
            list_row(world, tree, view, child, depth + 1, rows);
        }
    }
}

/// List `entity` if its label or anything below it matches `filter`, opened onto the
/// branches that do. Whether anything was listed.
fn list_matches(
    world: &World,
    tree: &mut OutlinerTree,
    filter: &str,
    entity: Entity,
    depth: usize,
    rows: &mut Vec<OutlinerRow>,
) -> bool {
    if world.get_entity(entity).is_err() {
        return false;
    }
    let at = rows.len();
    let has_children = has_children(world, tree, entity);
    rows.push(OutlinerRow {
        source: entity,
        depth,
        has_children,
        open: false,
    });
    let mut below = false;
    if has_children {
        for child in children_of(world, tree, entity) {
            below |= list_matches(world, tree, filter, child, depth + 1, rows);
        }
    }
    let matches = row_label(world, entity).to_lowercase().contains(filter);
    if !matches && !below {
        rows.truncate(at);
        return false;
    }
    rows[at].open = below;
    true
}

/// What a bound row last drew, so a frame's sync only writes what changed. `None` is "not
/// drawn yet": a widget rebound to another row starts over, so a value that happens to match
/// the default (the first row's top, a root's indent) is still written.
#[derive(Component, Default)]
struct RowDrawn {
    /// The disclosure last written; the row's own differing from it is a flip made on the
    /// row (its toggle, a drag resting on it), which the list takes rather than undoes.
    open: Option<bool>,
    top: Option<f32>,
    indent: Option<f32>,
    has_children: Option<bool>,
    label: Option<String>,
    glyph: Option<(String, Color)>,
    selected: Option<bool>,
    locked: Option<bool>,
    hidden: Option<bool>,
}

/// Lay out every panel's window of rows over its list, and sync each from its entity.
fn bind_rows(world: &mut World) {
    let containers: Vec<Entity> = world
        .query_filtered::<Entity, With<OutlinerView>>()
        .iter(world)
        .collect();
    for container in containers {
        bind_panel(world, container);
    }
}

fn bind_panel(world: &mut World, container: Entity) {
    let Some(mut view) = world.entity_mut(container).take::<OutlinerView>() else {
        return;
    };
    let Some(spacer) = view.spacer.filter(|&spacer| world.get_entity(spacer).is_ok()) else {
        world.entity_mut(container).insert(view);
        return;
    };
    let total = view.rows.len() as f32 * ROW_HEIGHT;

    // No more of a list is ever in view than the window holds, whatever its panel's size
    // says (a panel laid out to its content is as tall as the whole list).
    let window_height = world
        .query::<&Window>()
        .iter(world)
        .map(|window| window.height())
        .fold(0.0f32, f32::max);
    let window_height = if window_height > 0.0 {
        window_height
    } else {
        UNLAID_VIEW_HEIGHT
    };
    let view_height = world
        .get::<ComputedNode>(container)
        .map(|computed| computed.size().y * computed.inverse_scale_factor())
        .filter(|height| *height > 0.0)
        .unwrap_or(UNLAID_VIEW_HEIGHT)
        .min(window_height);
    let mut scroll = world
        .get::<ScrollPosition>(container)
        .map_or(0.0, |scroll| scroll.y);
    if let Some(target) = view.reveal.take()
        && let Some(index) = view.position(target)
    {
        let top = index as f32 * ROW_HEIGHT;
        if top < scroll {
            scroll = top;
        } else if top + ROW_HEIGHT > scroll + view_height {
            scroll = top + ROW_HEIGHT - view_height;
        }
    }
    let extent = (total - view_height).max(0.0);
    scroll = scroll.clamp(0.0, extent);
    if let Some(mut position) = world.get_mut::<ScrollPosition>(container)
        && position.y != scroll
    {
        position.y = scroll;
    }
    let extent = jackdaw_widgets::tree_view::TreeScrollExtent(extent);
    if world.get::<jackdaw_widgets::tree_view::TreeScrollExtent>(container) != Some(&extent) {
        world.entity_mut(container).insert(extent);
    }
    if let Some((track, thumb)) = view.bar {
        sync_bar(world, track, thumb, scroll, total, view_height);
    }

    let first = ((scroll / ROW_HEIGHT).floor() as usize).saturating_sub(OVERSCAN);
    let wanted = (view_height / ROW_HEIGHT).ceil() as usize + 2 * OVERSCAN;
    let count = wanted.min(view.rows.len().saturating_sub(first));

    // A widget whose entity stays in the window keeps it; the rest are rebound to the rows
    // that came into view, and the pool grows or shrinks to the window.
    let window = first..first + count;
    let in_window: HashSet<Entity> = view.rows[window.clone()].iter().map(|r| r.source).collect();
    let mut by_source: HashMap<Entity, usize> = HashMap::new();
    let mut free: Vec<usize> = Vec::new();
    for (slot, (_, bound)) in view.pool.iter().enumerate() {
        match bound {
            Some(source) if in_window.contains(source) => {
                by_source.insert(*source, slot);
            }
            _ => free.push(slot),
        }
    }
    let style = TreeRowStyle {
        icon_font: world.resource::<IconFont>().0.clone(),
    };
    for index in window {
        let row = view.rows[index];
        let next_depth = view.rows.get(index + 1).map_or(0, |next| next.depth);
        let slot = match by_source.get(&row.source) {
            Some(&slot) => slot,
            None => {
                let slot = free.pop().unwrap_or_else(|| {
                    let widget = world
                        .spawn((tree_row(&style), RowDrawn::default(), ChildOf(spacer)))
                        .id();
                    view.pool.push((widget, None));
                    view.pool.len() - 1
                });
                let (widget, bound) = view.pool[slot];
                if let Some(old) = bound {
                    unindex(world, container, old, widget);
                }
                cancel_rename_on(world, widget);
                world
                    .entity_mut(widget)
                    .insert((TreeNode(row.source), RowDrawn::default()));
                world
                    .resource_mut::<TreeIndex>()
                    .insert(container, row.source, widget);
                view.pool[slot].1 = Some(row.source);
                slot
            }
        };
        let top = index as f32 * ROW_HEIGHT - scroll;
        if let Some(open) = sync_row(world, view.pool[slot].0, row, top, next_depth)
            && set_expanded(&mut view, row.source, open)
        {
            world.resource_mut::<OutlinerTree>().dirty = true;
        }
    }
    // Widgets left over stand for nothing in view.
    free.sort_unstable();
    for slot in free.into_iter().rev() {
        let (widget, bound) = view.pool.swap_remove(slot);
        if let Some(source) = bound {
            unindex(world, container, source, widget);
        }
        cancel_rename_on(world, widget);
        if let Ok(entity) = world.get_entity_mut(widget) {
            entity.despawn();
        }
    }
    view.stale = false;
    world.entity_mut(container).insert(view);
}

/// Show the bar while the list is longer than the view, its thumb as much of the track as
/// the view is of the list, where the view is in it.
fn sync_bar(world: &mut World, track: Entity, thumb: Entity, scroll: f32, total: f32, view: f32) {
    let shown = total > view + 0.5;
    let visibility = if shown {
        Visibility::Inherited
    } else {
        Visibility::Hidden
    };
    if world.get::<Visibility>(track) != Some(&visibility) {
        world.entity_mut(track).insert(visibility);
    }
    if !shown {
        return;
    }
    let length = world
        .get::<ComputedNode>(track)
        .map(|computed| computed.size().y * computed.inverse_scale_factor())
        .filter(|length| *length > 0.0)
        .unwrap_or(view);
    let height = (length * view / total).clamp(BAR_MIN_THUMB.min(length), length);
    let top = if total > view {
        (length - height) * scroll / (total - view)
    } else {
        0.0
    };
    if let Some(mut node) = world.get_mut::<Node>(thumb)
        && (node.top != px(top) || node.height != px(height))
    {
        node.top = px(top);
        node.height = px(height);
    }
}

fn unindex(world: &mut World, container: Entity, source: Entity, row: Entity) {
    let mut index = world.resource_mut::<TreeIndex>();
    if index.get(container, source) == Some(row) {
        index.remove(container, source);
    }
}

/// A rename open on a row that is about to stand for something else is called off.
fn cancel_rename_on(world: &mut World, row: Entity) {
    let renames: Vec<(Entity, Entity, Entity)> = world
        .query::<(Entity, &InlineRenameInput)>()
        .iter(world)
        .map(|(entry, input)| (entry, input.label_entity, input.source_entity))
        .collect();
    for (entry, label, source) in renames {
        if !is_below(world, label, row) {
            continue;
        }
        let name = row_label(world, source);
        if let Ok(mut label) = world.get_entity_mut(label) {
            label.remove::<TreeRowInlineRename>();
            label.insert(Text::new(name));
            if let Some(mut node) = label.get_mut::<Node>() {
                node.display = Display::Flex;
            }
        }
        if let Ok(entry) = world.get_entity_mut(entry) {
            entry.despawn();
        }
        world.resource_mut::<InputFocus>().clear();
    }
}

fn is_below(world: &World, entity: Entity, ancestor: Entity) -> bool {
    let mut current = entity;
    while let Some(&ChildOf(parent)) = world.get::<ChildOf>(current) {
        if parent == ancestor {
            return true;
        }
        current = parent;
    }
    false
}

/// Bring `widget` in line with its row and the entity it stands for. Returns the disclosure
/// the row was flipped to, when it was flipped on the row since the last sync.
fn sync_row(
    world: &mut World,
    widget: Entity,
    row: OutlinerRow,
    top: f32,
    next_depth: usize,
) -> Option<bool> {
    let mut drawn = world.entity_mut(widget).take::<RowDrawn>()?;
    let source = row.source;

    let indent = row.depth as f32 * INDENT_STEP;
    if drawn.top != Some(top) || drawn.indent != Some(indent) {
        if let Some(mut node) = world.get_mut::<Node>(widget) {
            node.top = px(top);
            node.padding.left = px(indent);
        }
        drawn.top = Some(top);
        drawn.indent = Some(indent);
    }
    let depth = TreeRowDepth {
        depth: row.depth,
        next_depth,
    };
    if world.get::<TreeRowDepth>(widget) != Some(&depth) {
        world.entity_mut(widget).insert(depth);
    }
    let on_row = world.get::<TreeNodeExpanded>(widget).map(|e| e.0);
    let mut flipped = None;
    if drawn.open == Some(row.open) && on_row.is_some() && on_row != Some(row.open) {
        flipped = on_row;
    } else if on_row != Some(row.open) {
        world.entity_mut(widget).insert(TreeNodeExpanded(row.open));
    }
    drawn.open = Some(row.open);
    if drawn.has_children != Some(row.has_children) {
        set_row_expand_toggle(world, widget, row.has_children);
        drawn.has_children = Some(row.has_children);
    }

    let content = first_child_with::<TreeRowContent>(world, widget);
    if let Some(content) = content {
        sync_label(world, content, source, &mut drawn);
        sync_glyph(world, content, source, &mut drawn);
        sync_selected(world, content, source, &mut drawn);
        sync_lock(world, content, source, &mut drawn);
        sync_eye(world, content, source, &mut drawn);
    }
    world.entity_mut(widget).insert(drawn);
    flipped
}

fn first_child_with<C: Component>(world: &World, parent: Entity) -> Option<Entity> {
    world
        .get::<Children>(parent)?
        .iter()
        .find(|&child| world.get::<C>(child).is_some())
}

fn sync_label(world: &mut World, content: Entity, source: Entity, drawn: &mut RowDrawn) {
    let label = row_label(world, source);
    if drawn.label.as_ref() == Some(&label) {
        return;
    }
    let Some(text) = first_child_with::<TreeRowLabel>(world, content) else {
        return;
    };
    if world.get::<TreeRowInlineRename>(text).is_some() {
        return;
    }
    if let Some(mut written) = world.get_mut::<Text>(text) {
        written.0.clone_from(&label);
    }
    // The cut the ellipsis pass made was of the old name.
    if let Ok(mut entity) = world.get_entity_mut(text) {
        entity.remove::<TreeRowLabelFit>();
    }
    drawn.label = Some(label);
}

fn sync_glyph(world: &mut World, content: Entity, source: Entity, drawn: &mut RowDrawn) {
    let category = classify_entity(world, source);
    let inherited = is_inherited_descendant(world, source);
    let icon = registered_icon(world, source).unwrap_or_else(|| category_icon(category));
    let glyph_text = String::from(icon.unicode());
    let color = category_color(category, inherited);
    if drawn.glyph.as_ref() == Some(&(glyph_text.clone(), color)) {
        return;
    }
    let Some(dot) = first_child_with::<TreeRowDot>(world, content) else {
        return;
    };
    let Some(glyph) = world.get::<Children>(dot).and_then(|c| c.iter().next()) else {
        return;
    };
    if let Some(mut text) = world.get_mut::<Text>(glyph) {
        text.0.clone_from(&glyph_text);
    }
    if let Some(mut text_color) = world.get_mut::<TextColor>(glyph) {
        text_color.0 = color;
    }
    drawn.glyph = Some((glyph_text, color));
}

fn sync_selected(world: &mut World, content: Entity, source: Entity, drawn: &mut RowDrawn) {
    let selected = world.resource::<Selection>().is_selected(source);
    if drawn.selected == Some(selected) {
        return;
    }
    let mut entity = world.entity_mut(content);
    if selected {
        entity.insert((
            TreeRowSelected,
            BackgroundColor(tokens::SELECTED_BG),
            BorderColor::all(tokens::SELECTED_BORDER),
        ));
    } else {
        entity.remove::<TreeRowSelected>();
        entity.insert((BackgroundColor(ROW_BG), BorderColor::all(Color::NONE)));
    }
    drawn.selected = Some(selected);
}

fn sync_lock(world: &mut World, content: Entity, source: Entity, drawn: &mut RowDrawn) {
    let locked = world.get::<Locked>(source).is_some();
    if drawn.locked == Some(locked) {
        return;
    }
    let Some(toggle) = first_child_with::<TreeRowLockToggle>(world, content) else {
        return;
    };
    let glyph = String::from(if locked { Icon::Lock } else { Icon::LockOpen }.unicode());
    let alpha = if locked { 1.0 } else { LOCK_IDLE_ALPHA };
    let texts: Vec<Entity> = world
        .get::<Children>(toggle)
        .map(|children| children.iter().collect())
        .unwrap_or_default();
    for text in texts {
        if let Some(mut written) = world.get_mut::<Text>(text) {
            written.0.clone_from(&glyph);
        }
        if let Some(mut color) = world.get_mut::<TextColor>(text) {
            color.0 = color.0.with_alpha(alpha);
        }
    }
    drawn.locked = Some(locked);
}

fn sync_eye(world: &mut World, content: Entity, source: Entity, drawn: &mut RowDrawn) {
    let hidden = matches!(world.get::<Visibility>(source), Some(Visibility::Hidden));
    if drawn.hidden == Some(hidden) {
        return;
    }
    let Some(toggle) = first_child_with::<TreeRowVisibilityToggle>(world, content) else {
        return;
    };
    let Some(glyph) = world.get::<Children>(toggle).and_then(|c| c.iter().next()) else {
        return;
    };
    if let Some(mut text) = world.get_mut::<Text>(glyph) {
        text.0 = String::from(if hidden { Icon::EyeOff } else { Icon::Eye }.unicode());
    }
    if let Some(mut color) = world.get_mut::<TextColor>(glyph) {
        color.0 = color.0.with_alpha(if hidden { 0.7 } else { 0.4 });
    }
    drawn.hidden = Some(hidden);
}

/// The row the keyboard walk stands on: a panel and the entity of one of its rows.
#[derive(Resource, Default, Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutlinerFocus(pub Option<(Entity, Entity)>);

/// Arrow keys walk the focused panel's list; Left closes a branch or steps out of it,
/// Right opens one; Enter or Space selects; F2 renames.
///
/// Read here rather than through the keymap, so the keybind recorder and text entry are
/// checked here.
pub fn outliner_keyboard_navigation(
    keyboard: Res<ButtonInput<KeyCode>>,
    capture: Option<Res<jackdaw_commands::KeymapCapture>>,
    text_inputs: Query<(), With<jackdaw_feathers::text_edit::EditorTextEdit>>,
    input_focus: Res<InputFocus>,
    mut focus: ResMut<OutlinerFocus>,
    mut views: Query<(Entity, &mut OutlinerView)>,
    mut tree: ResMut<OutlinerTree>,
    mut selection: ResMut<Selection>,
    mut commands: Commands,
) {
    if jackdaw_commands::KeymapCapture::is_recording(capture.as_deref()) {
        return;
    }
    if input_focus
        .get()
        .is_some_and(|entity| text_inputs.contains(entity))
    {
        return;
    }
    let keys = [
        KeyCode::ArrowDown,
        KeyCode::ArrowUp,
        KeyCode::ArrowLeft,
        KeyCode::ArrowRight,
        KeyCode::Enter,
        KeyCode::Space,
        KeyCode::F2,
    ];
    if !keyboard.any_just_pressed(keys) {
        return;
    }
    let (container, current) = match focus.0 {
        Some((container, source)) if views.contains(container) => (container, Some(source)),
        _ => match views.iter().next() {
            Some((container, _)) => (container, None),
            None => return,
        },
    };
    let Ok((_, mut view)) = views.get_mut(container) else {
        return;
    };
    if view.rows.is_empty() {
        return;
    }
    let at = current.and_then(|source| view.position(source));
    let mut next = current;
    if keyboard.just_pressed(KeyCode::ArrowDown) {
        next = Some(match at {
            Some(i) => view.rows[(i + 1).min(view.rows.len() - 1)].source,
            None => view.rows[0].source,
        });
    }
    if keyboard.just_pressed(KeyCode::ArrowUp) {
        next = Some(match at {
            Some(i) => view.rows[i.saturating_sub(1)].source,
            None => view.rows[view.rows.len() - 1].source,
        });
    }
    if keyboard.just_pressed(KeyCode::ArrowLeft)
        && let Some(i) = at
    {
        let row = view.rows[i];
        if row.open {
            if set_expanded(&mut view, row.source, false) {
                tree.dirty = true;
            }
        } else if let Some(parent) = view.rows[..i].iter().rev().find(|r| r.depth < row.depth) {
            next = Some(parent.source);
        }
    }
    if keyboard.just_pressed(KeyCode::ArrowRight)
        && let Some(i) = at
    {
        let row = view.rows[i];
        if row.has_children && !row.open && set_expanded(&mut view, row.source, true) {
            tree.dirty = true;
        }
    }
    if (keyboard.just_pressed(KeyCode::Enter) || keyboard.just_pressed(KeyCode::Space))
        && let Some(source) = current
    {
        selection.select_single(&mut commands, source);
    }
    if keyboard.just_pressed(KeyCode::F2)
        && let Some(source) = current
    {
        use jackdaw_api::prelude::*;
        commands
            .operator(super::RenameBeginOp::ID)
            .param("entity", source)
            .call();
    }
    if next != current
        && let Some(source) = next
    {
        focus.0 = Some((container, source));
        view.reveal = Some(source);
        tree.dirty = true;
    }
}

/// The entities of `container`'s rows, top to bottom, as the panel lists them (in view or
/// not). A range is stated over these: a closed row's children are not listed to be swept.
pub fn visible_row_sources(world: &World, container: Entity) -> Vec<Entity> {
    world
        .get::<OutlinerView>(container)
        .map(|view| view.rows.iter().map(|row| row.source).collect())
        .unwrap_or_default()
}

/// The entities listed directly under `source`'s row in `container`'s panel, top to bottom;
/// empty while it is closed or not listed.
pub fn listed_children(world: &World, container: Entity, source: Entity) -> Vec<Entity> {
    let Some(view) = world.get::<OutlinerView>(container) else {
        return Vec::new();
    };
    let Some(at) = view.position(source) else {
        return Vec::new();
    };
    let depth = view.rows[at].depth;
    view.rows[at + 1..]
        .iter()
        .take_while(|row| row.depth > depth)
        .filter(|row| row.depth == depth + 1)
        .map(|row| row.source)
        .collect()
}

/// Despawn every panel's row widgets and forget what each had open and listed.
pub(crate) fn reset_views(world: &mut World) {
    let containers: Vec<Entity> = world
        .query_filtered::<Entity, With<OutlinerView>>()
        .iter(world)
        .collect();
    for container in containers {
        let Some(mut view) = world.entity_mut(container).take::<OutlinerView>() else {
            continue;
        };
        for (row, _) in view.pool.drain(..) {
            if let Ok(entity) = world.get_entity_mut(row) {
                entity.despawn();
            }
        }
        view.rows.clear();
        view.position.clear();
        view.expanded.clear();
        view.stale = true;
        world.entity_mut(container).insert(view);
        world.resource_mut::<TreeIndex>().clear_container(container);
    }
    world.resource_mut::<OutlinerTree>().drop_all();
}
