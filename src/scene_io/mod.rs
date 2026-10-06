//! Scene persistence: a scene file spawns with bevy's `.bsn` loader, and saves with the scene
//! writer (`bevy::bsn_asset::write_scene_roots`) over the entities the scene holds.

use std::any::TypeId;
use std::path::PathBuf;

use bevy::ecs::reflect::AppTypeRegistry;
use bevy::prelude::*;

mod load;
mod authored;
mod registration;
pub(crate) mod save;
pub mod stamp;

pub use load::{
    LoadOutcome, LoadRefusal, PendingSceneSpawns, RefusalCategory, SceneFile, SceneSpawn,
    apply_scene_kind, asset_path_of, load_scene_from_file, load_scene_from_file_with_outcome,
    open_scene_kind, read_scene_file, scene_kind_of, spawn_bsn_text, spawn_default_lighting, spawn_open_dialog,
    spawn_scene_file,
};
pub(crate) use load::{
    SidecarImport, clear_scene_entities, despawn_scene_entities, import_terrain_sidecars,
    is_unresolved,
};
pub use authored::{author, freeze_authored};
pub use registration::{
    SceneEntity, SceneRootOf, SceneRoots, adopt_entities, adopt_entity, despawn_tab_world,
    ensure_scene_world, is_ui_root, place_root, scene_parent, scene_world, set_tab_open,
};
pub use save::{
    SaveOutcome, emit_bsn_scene_for_file, emit_bsn_scene_with_inline_assets, retarget_active_scene,
    save_layout_to_project, save_scene, save_scene_as, save_scene_with_outcome,
};
pub(crate) use save::save_scene_inner;

use load::poll_scene_dialog;

/// Component type path prefixes that should never be saved (runtime-only / internal).
const SKIP_COMPONENT_PREFIXES: &[&str] = &[
    "bevy_render::",
    "bevy_picking::",
    "bevy_window::",
    "bevy_ecs::observer::",
    "bevy_camera::primitives::",
    "bevy_camera::visibility::",
    // AnimationPlayer / AnimationGraphHandle / AnimationTargetId / AnimatedBy
    // are installed on targets at runtime by the animation plugin.
    // They're derived from the authored clip components and must not be
    // serialized; otherwise load would restore stale player state and
    // dangling asset handles.
    "bevy_animation::",
    // Propagated/inherited values are recomputed from their source every frame
    // (`Inherited<TextColor>`, `Propagate<TextFont>`, ...).
    "bevy_app::propagate::",
    // Widget implementation detail: the marker components and generated parts
    // a feathers control builds for itself. The styling components a widget
    // definition authors are listed in `ALWAYS_SAVE_PATHS` below and override
    // this prefix.
    "bevy_feathers::",
    // Accessibility nodes are built by the widget implementation.
    "bevy_a11y::",
    // Avian's computed state (AABBs, collider links, mass caches, `Position`/`Rotation`) is
    // rebuilt from `RigidBody` + `AvianCollider`; what a user authors is in
    // `AUTHORED_PHYSICS` below.
    "avian3d::",
    "bevy_heavy::",
];

/// The avian components a user sets, by type name: everything else under `avian3d::` is derived.
const AUTHORED_PHYSICS: &[&str] = &[
    "RigidBody",
    "Friction",
    "Restitution",
    "Mass",
    "AngularInertia",
    "CenterOfMass",
    "ColliderDensity",
    "GravityScale",
    "LinearDamping",
    "AngularDamping",
    "LockedAxes",
    "CollisionLayers",
    "Sensor",
    "LinearVelocity",
    "AngularVelocity",
    "Dominance",
    "SweptCcd",
    "CollisionEventsEnabled",
    "RigidBodyDisabled",
    "ColliderDisabled",
];

/// Specific component type paths that should never be saved.
const SKIP_COMPONENT_PATHS: &[&str] = &[
    "bevy_transform::components::transform::TransformTreeChanged",
    "bevy_light::cascade::Cascades",
    // Runtime activation state, granted and revoked by the rig systems (the
    // multiplayer gate on clients). Persisting it plants a rig that fights
    // those systems on every load.
    "jackdaw_camera_rig::ActiveCameraRig",
    // An `AnimationRig`'s player and the bone bindings it stamps: derived from
    // the rig and its names whenever it spawns, and a saved player would be
    // stale before the file closed.
    "bevy_animation_graph_core::animation_graph_player::AnimationGraphPlayer",
    "bevy_animation::AnimationTargetId",
    "bevy_animation::AnimatedBy",
    // The GLTF instance handle, derived from the authored `GltfSource` by
    // `derive_world_asset_root`. Writing it into the document would put a
    // raw asset handle in a file that other machines and the runtime read.
    "bevy_world_serialization::components::WorldAssetRoot",
    // Editor-managed routing, inserted by `route_ui_roots_to_cameras` to aim a
    // UI scene root at its view: the open 2D viewport for an authored root, the
    // 3D viewport for one a world scene imports. It names a camera entity this
    // session spawned, so a saved copy would point at nothing on reload.
    "bevy_ui::ui_node::UiTargetCamera",
];

/// The UI state bevy computes every frame from `Node` and the text or image
/// beside it. None of it is authored, and a document that records it reloads
/// carrying a measurement of the session that saved it.
///
/// Spelled through [`TypePath`] rather than written out: the same list held as
/// strings went stale when `TextLayoutInfo` moved module, and a path that no
/// longer names anything skips nothing and says nothing.
///
/// [`TypePath`]: bevy::reflect::TypePath
pub fn computed_ui_component_paths() -> [&'static str; 10] {
    use bevy::reflect::TypePath;
    [
        bevy::ui::ComputedNode::type_path(),
        bevy::ui::ComputedUiTargetCamera::type_path(),
        bevy::ui::ComputedUiRenderTargetInfo::type_path(),
        bevy::ui::ComputedStackIndex::type_path(),
        bevy::ui::UiGlobalTransform::type_path(),
        bevy::ui::ContentSize::type_path(),
        bevy::text::ComputedTextBlock::type_path(),
        bevy::text::TextLayoutInfo::type_path(),
        bevy::ui::widget::TextNodeFlags::type_path(),
        bevy::ui::widget::ImageNodeSize::type_path(),
    ]
}

/// Paths that override the skip prefixes  -- these are always saved even if
/// they match a skip prefix.
const ALWAYS_SAVE_PATHS: &[&str] = &[
    "bevy_camera::visibility::Visibility",
    // Reference image boards persist with the scene; the quad mesh and
    // material are derived from this component at runtime.
    "jackdaw::reference_image::ReferenceImage",
    // Authored feathers styling, as opposed to the derived styling the
    // `bevy_feathers::` skip above covers. These are plain `Reflect` components
    // widget creation puts on an authored node so the widget follows the theme
    // rather than a colour frozen at spawn time. Dropping them on save reloads
    // the document as flat boxes.
    "bevy_feathers::theme::ThemeBackgroundColor",
    "bevy_feathers::theme::ThemeBorderColor",
    "bevy_feathers::theme::ThemeTextColor",
    "bevy_feathers::theme::InheritableThemeTextColor",
    "bevy_feathers::theme::ThemedText",
    "bevy_feathers::controls::button::ButtonVariant",
    "bevy_feathers::focus::FocusIndicator",
    "bevy_picking::cursor::EntityCursor",
];

pub fn should_skip_component(type_path: &str) -> bool {
    // Always-save takes priority over any skip rule
    if ALWAYS_SAVE_PATHS.contains(&type_path) {
        return false;
    }
    if type_path.starts_with("avian3d::")
        && type_path
            .rsplit("::")
            .next()
            .is_some_and(|name| AUTHORED_PHYSICS.contains(&name))
    {
        return false;
    }
    if type_path.starts_with("jackdaw::") {
        return true;
    }
    for prefix in SKIP_COMPONENT_PREFIXES {
        if type_path.starts_with(prefix) {
            return true;
        }
    }
    SKIP_COMPONENT_PATHS.contains(&type_path)
        || computed_ui_component_paths().contains(&type_path)
        || type_path == crate::worn_material::layered_material_component()
        || type_path == crate::worn_material::foliage_material_component()
        || type_path == crate::worn_material::water_material_component()
}

/// What a save leaves out: component types the editor derives or owns, the entities it
/// generates (brush faces, terrain chunks), editor infrastructure, and mesh handles with no path
/// (meshes the editor builds; an authored mesh names its file).
pub fn write_settings(world: &mut World) -> bevy::bsn_asset::WriteSettings {
    let skip_components: bevy::platform::collections::HashSet<TypeId> = {
        let registry = world.resource::<AppTypeRegistry>().read();
        registry
            .iter()
            .filter(|registration| {
                should_skip_component(registration.type_info().type_path_table().path())
            })
            .map(|registration| registration.type_id())
            .collect()
    };
    let skip_entities: bevy::platform::collections::HashSet<Entity> = world
        .query_filtered::<Entity, Or<(
            With<crate::NonSerializable>,
            With<crate::EditorEntity>,
            With<crate::animgraph::held::HeldGraphNode>,
        )>>()
        .iter(world)
        .collect();
    let mesh = TypeId::of::<bevy_aurora::mesh::AuroraMesh3d>();
    bevy::bsn_asset::WriteSettings {
        skip_components,
        skip_entities,
        skip_value: Some(std::sync::Arc::new(move |type_id, value| {
            type_id == mesh
                && value
                    .try_downcast_ref::<bevy_aurora::mesh::AuroraMesh3d>()
                    .is_some_and(|mesh| mesh.0.path().is_none())
        })),
        asset_paths: world
            .get_resource::<jackdaw_runtime::JackdawCatalog>()
            .map(|catalog| catalog.asset_paths().into_iter().collect())
            .unwrap_or_default(),
    }
}

/// The open scene's top-level entities, in the order a file lists them.
pub fn scene_roots(world: &mut World) -> Vec<Entity> {
    if let Some(tab_world) = scene_world(world) {
        return world
            .get::<SceneRoots>(tab_world)
            .map(|roots| roots.entities().to_vec())
            .unwrap_or_default();
    }
    let mut roots: Vec<Entity> = world
        .query_filtered::<Entity, (With<SceneEntity>, Without<ChildOf>)>()
        .iter(world)
        .collect();
    roots.sort_by_key(|entity| entity.index());
    roots
}

pub struct SceneIoPlugin;

impl Plugin for SceneIoPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(authored::plugin);
        app.init_resource::<SceneFilePath>()
            .init_resource::<SceneDirtyState>()
            .add_systems(
                Update,
                poll_scene_dialog.run_if(in_state(crate::AppState::Editor)),
            )
            .init_resource::<PendingSceneSpawns>()
            .add_systems(Update, (load::finish_pending_scene_spawns, crate::entity_ops::poll_instance_picker))
            .add_systems(PostUpdate, deactivate_scene_cameras);
    }
}

/// Keeps cameras authored in the scene document from rendering in the
/// editor. `Camera3d` on a document entity pulls in a required `Camera`
/// whose defaults target the primary window at order 0, the same window
/// the editor UI camera composites into, so an authored game camera
/// drew the scene on top of the docks. The components stay on the
/// entity for inspection and save; only rendering is suppressed.
fn deactivate_scene_cameras(
    mut cameras: Query<&mut bevy::camera::Camera, (With<SceneEntity>, Without<crate::EditorEntity>)>,
) {
    for mut camera in &mut cameras {
        if camera.is_active {
            camera.is_active = false;
        }
    }
}

/// Tracks whether the scene has unsaved changes by comparing the current
/// undo stack length against the length at the time of last save/load/new.
#[derive(Resource, Default)]
pub struct SceneDirtyState {
    pub undo_len_at_save: usize,
}

/// Returns `true` when the scene has unsaved changes.
pub fn is_scene_dirty(world: &World) -> bool {
    let history = world.resource::<jackdaw_commands::CommandHistory>();
    let dirty_state = world.resource::<SceneDirtyState>();
    history.undo_stack.len() != dirty_state.undo_len_at_save
}

/// Stores the currently active scene file path and metadata.
#[derive(Resource, Default)]
pub struct SceneFilePath {
    pub path: Option<String>,
    pub metadata: SceneMetadata,
    pub last_directory: Option<PathBuf>,
}

/// Human-readable metadata for the active scene, tracked live on [`SceneFilePath`].
#[derive(Clone, Debug, Default)]
pub struct SceneMetadata {
    pub name: String,
    pub description: String,
    pub author: String,
    pub created: String,
    pub modified: String,
}

#[cfg(test)]
mod camera_tests {
    use super::*;

    #[test]
    fn document_cameras_are_deactivated_and_editor_cameras_kept() {
        let mut world = World::new();
        let authored = world
            .spawn((bevy::camera::Camera::default(), SceneEntity))
            .id();
        let editor = world.spawn(bevy::camera::Camera::default()).id();

        world
            .run_system_cached(deactivate_scene_cameras)
            .expect("run the deactivation system");

        let is_active = |world: &World, e| world.get::<bevy::camera::Camera>(e).unwrap().is_active;
        assert!(
            !is_active(&world, authored),
            "a document camera must not render in the editor"
        );
        assert!(
            is_active(&world, editor),
            "non-document cameras keep rendering"
        );
    }
}
