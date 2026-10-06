use std::collections::{HashMap, VecDeque};
use std::path::Path;
use std::time::{Duration, Instant};

use bevy::{
    ecs::system::{SystemParam, SystemState},
    gltf::GltfAssetLabel,
    light::{RectLight, SunDisk},
    prelude::*,
    world_serialization::WorldAsset,
};

use crate::{
    EditorEntity,
    commands::{CommandHistory, DespawnEntity, EditorCommand, HierarchyLocation, MoveEntity},
    frame_work::FramePace,
    selection::{Selected, Selection},
};

/// System clipboard for copy/paste of entities as scene text. On X11 the
/// clipboard is ownership-based, so the `Clipboard` has to stay alive.
#[derive(Resource)]
pub struct SystemClipboard {
    clipboard: arboard::Clipboard,
    /// The text this editor last put on the OS clipboard, so a paste can tell
    /// its own emission from a stranger's.
    last_emitted: String,
}

impl SystemClipboard {
    /// The OS clipboard image as owned RGBA8, or an error when the clipboard
    /// holds no image.
    pub(crate) fn get_image(&mut self) -> Result<arboard::ImageData<'static>, arboard::Error> {
        self.clipboard.get_image()
    }
}

/// The last entity subtree copied in this editor, as BSN text. It mirrors every
/// copy, so a paste works when the OS clipboard is absent or holds no scene.
#[derive(Resource, Default)]
pub struct EntityClipboard {
    /// BSN text, empty until the first copy.
    pub text: String,
}

pub use jackdaw_scene_types::GltfSource;

pub struct EntityOpsPlugin;

impl Plugin for EntityOpsPlugin {
    fn build(&self, app: &mut App) {
        // Note: GltfSource type registration is handled by SceneTypesPlugin
        match arboard::Clipboard::new() {
            Ok(clipboard) => {
                app.insert_resource(SystemClipboard {
                    clipboard,
                    last_emitted: String::new(),
                });
            }
            Err(e) => {
                warn!("Failed to initialize system clipboard: {e}");
            }
        }
        app.init_resource::<EntityClipboard>()
            .init_resource::<PendingModelRoots>()
            .add_systems(Update, hand_out_model_roots)
            .add_systems(PostUpdate, measure_model_cost)
            .add_observer(derive_world_asset_root)
            .add_observer(forget_model_root)
            .register_type::<EmptyEntity>()
            .register_type::<SceneCamera>()
            .register_type::<SceneLight>()
            .register_type::<SceneFogVolume>()
            .register_type::<SceneReflectionProbe>()
            .register_type::<SceneAnimationPlayer>()
            .register_type::<SceneAudioSource>();
    }
}

/// The least a frame will spend bringing models on, whatever else it is doing.
const MODEL_FLOOR_BUDGET: Duration = Duration::from_millis(16);

/// The fewest and the most models one frame hands over.
const MODEL_BATCH_FLOOR: usize = 1;
const MODEL_BATCH_CEILING: usize = 256;

/// Models whose render root has been derived but not yet handed to the
/// world-asset spawner.
///
/// Inserting `WorldAssetRoot` registers an instance the spawner builds later
/// in the same frame, and a large scene holds thousands of them: handing a
/// whole document over at once spends the frame in the spawner and the window
/// never paints. They wait here and go out a batch a frame instead.
#[derive(Resource)]
pub struct PendingModelRoots {
    /// The order models come on in, first derived first.
    order: VecDeque<Entity>,
    /// The model each waiting entity is to be given. An entity whose source
    /// changed again before its turn keeps its place and takes the later
    /// model, so nothing comes on twice.
    wanted: HashMap<Entity, Handle<WorldAsset>>,
    pace: FramePace,
    /// When this frame's batch went out, so what bringing it on cost can be
    /// read once the spawner has run.
    handed: Option<Instant>,
    /// What the last batch cost, from handing it over to the spawner finishing.
    /// The frame time as a whole says nothing useful here: a heavy scene
    /// renders slowly whether or not anything is still coming on.
    cost: Duration,
}

impl Default for PendingModelRoots {
    fn default() -> Self {
        Self {
            order: VecDeque::new(),
            wanted: HashMap::new(),
            pace: FramePace::new(MODEL_BATCH_FLOOR, MODEL_BATCH_CEILING),
            handed: None,
            cost: Duration::ZERO,
        }
    }
}

impl PendingModelRoots {
    /// How many models are still waiting to come on. What is still wanted, not
    /// what is still in the order: an entity that has gone leaves its place in
    /// the order behind and is stepped over when its turn comes.
    pub fn len(&self) -> usize {
        self.wanted.len()
    }

    pub fn is_empty(&self) -> bool {
        self.wanted.is_empty()
    }

    fn push(&mut self, entity: Entity, scene: Handle<WorldAsset>) {
        if self.wanted.insert(entity, scene).is_none() {
            self.order.push_back(entity);
        }
    }

    /// Stop waiting to bring a model on. The model it was to be given goes
    /// with it, so the glTF it names is held only for models still wanted.
    fn forget(&mut self, entity: Entity) {
        self.wanted.remove(&entity);
    }

    /// The models this frame takes on.
    ///
    /// Bringing them on may cost the frame about as much as everything else in
    /// it already does, so a scene that is slow to draw still fills in at a
    /// useful rate and a frame never much more than doubles.
    fn take(&mut self, frame: Duration) -> Vec<(Entity, Handle<WorldAsset>)> {
        let budget = frame.saturating_sub(self.cost).max(MODEL_FLOOR_BUDGET);
        let count = self.pace.take(self.cost, budget, self.wanted.len());
        let mut taking = Vec::with_capacity(count);
        while taking.len() < count {
            let Some(entity) = self.order.pop_front() else {
                break;
            };
            if let Some(scene) = self.wanted.remove(&entity) {
                taking.push((entity, scene));
            }
        }
        if !taking.is_empty() {
            self.handed = Some(Instant::now());
        }
        taking
    }
}

/// Stop waiting to bring on a model whose source has gone, rather than leaving
/// it queued until its turn comes.
///
/// A queued model holds the glTF it names, and the footer and the wait for an
/// idle editor both count what is queued; an entry nothing will use any more
/// would hold an asset nothing renders and a wait nothing can end.
fn forget_model_root(remove: On<Remove<GltfSource>>, mut pending: ResMut<PendingModelRoots>) {
    pending.forget(remove.entity);
}

/// Stop waiting to bring on the models queued for `entities`, for a caller
/// taking them down itself.
///
/// What is queued belongs to whoever authored it: a scene taken down takes its
/// own queue with it, and leaves alone the models queued beside it for the
/// thumbnail stage and anything else that outlives one scene.
pub(crate) fn forget_model_roots<'a>(
    world: &mut World,
    entities: impl IntoIterator<Item = &'a Entity>,
) {
    let Some(mut pending) = world.get_resource_mut::<PendingModelRoots>() else {
        return;
    };
    for entity in entities {
        pending.forget(*entity);
    }
}

/// Derive the render-side `WorldAssetRoot` from the authored `GltfSource`,
/// the way reference images and terrains derive their render state.
///
/// Undo, redo, tab swaps and file loads all re-insert `GltfSource` from the
/// document, so deriving it here is what brings the model back on each of
/// those paths without the handle ever being written to the document.
fn derive_world_asset_root(
    insert: On<Insert<GltfSource>>,
    sources: Query<&GltfSource>,
    existing: Query<&WorldAssetRoot>,
    asset_server: Res<AssetServer>,
    mut pending: ResMut<PendingModelRoots>,
) {
    let entity = insert.entity;
    let Ok(source) = sources.get(entity) else {
        return;
    };
    // Scenes authored before paths were normalised still hold an absolute
    // path; `to_asset_path` reduces those and passes a relative one through.
    let asset_path = to_asset_path(&source.path);
    let scene: Handle<WorldAsset> =
        asset_server.load(GltfAssetLabel::Scene(source.scene_index).from_asset(asset_path));
    // Re-inserting an equal handle still trips `Changed`, and the world-asset
    // spawner despawns and respawns the whole instance on every change.
    // Applying the document re-inserts `GltfSource` wholesale, so without this
    // the model is rebuilt on every undo.
    if existing.get(entity).is_ok_and(|root| root.0 == scene) {
        return;
    }
    pending.push(entity, scene);
}

/// What the footer calls the models still coming on.
const MODEL_PHASE: &str = "models";

/// Hand the next batch of models to the world-asset spawner. Runs in `Update`,
/// so what it inserts is spawned in the same frame's `SpawnScene`.
fn hand_out_model_roots(
    time: Res<Time<Real>>,
    mut pending: ResMut<PendingModelRoots>,
    // Worlds without a footer -- the launcher, and the test harnesses -- still
    // bring models on.
    mut phase: Option<ResMut<crate::status_bar::EditorPhase>>,
    mut named: Local<bool>,
    mut commands: Commands,
) {
    if pending.is_empty() {
        if std::mem::take(&mut *named)
            && let Some(phase) = phase.as_mut()
        {
            phase.finish(MODEL_PHASE);
        }
        return;
    }
    if let Some(phase) = phase.as_mut() {
        phase.begin(MODEL_PHASE, format!("Placing {} models", pending.len()));
        *named = true;
    }
    let batch = pending.take(time.delta());
    commands.queue(move |world: &mut World| {
        for (entity, scene) in batch {
            // Between deriving the root and this frame the entity may have
            // been despawned, or already given this very handle.
            let Ok(mut entity) = world.get_entity_mut(entity) else {
                continue;
            };
            // The source may also have gone -- an undo of the placement that
            // set it, say. An instance under an entity that no longer names a
            // model is one nothing owns and nothing takes down again.
            if entity.get::<GltfSource>().is_none() {
                continue;
            }
            if entity
                .get::<WorldAssetRoot>()
                .is_some_and(|root| root.0 == scene)
            {
                continue;
            }
            entity.insert(WorldAssetRoot(scene));
        }
    });
}

/// Read what the batch this frame handed over cost, once the scene spawner
/// that builds it has run.
fn measure_model_cost(pending: ResMut<PendingModelRoots>) {
    // Reaching through `ResMut` marks the resource changed, and most frames
    // hand nothing over, so the empty case answers before it does.
    if pending.handed.is_none() {
        return;
    }
    let pending = pending.into_inner();
    if let Some(handed) = pending.handed.take() {
        pending.cost = handed.elapsed();
    }
}

/// Marks an entity as an intentionally-empty scene entity (`Add > Empty`).
/// Used by the viewport-overlay system to decide whether to draw a
/// fallback wireframe-cube marker. Serialises through the type registry
/// so empties loaded from a `.jsn` scene keep the marker.
#[derive(Component, Default, Reflect)]
#[reflect(Component, @crate::EditorHidden)]
pub struct EmptyEntity;

/// Marks a camera as scene-authored (added via `Add > Camera` or by an
/// extension), so viewport overlays draw a frustum gizmo for it.
/// Editor-internal cameras (main viewport camera, material preview
/// camera) deliberately don't carry this marker.
#[derive(Component, Default, Reflect)]
#[reflect(Component, @crate::EditorHidden)]
pub struct SceneCamera;

/// Marks a light as scene-authored, so viewport overlays draw
/// light-specific gizmos for it. Editor-internal lights (e.g. the
/// material-preview rig) deliberately don't carry this marker.
#[derive(Component, Default, Reflect)]
#[reflect(Component, @crate::EditorHidden)]
pub struct SceneLight;

/// Marks a fog-volume entity (`Add > Fog Volume`), so viewport
/// overlays draw a box gizmo at the volume's extent. The box is the
/// unit cube scaled by the entity's `Transform.scale`.
#[derive(Component, Default, Reflect)]
#[reflect(Component, @crate::EditorHidden)]
pub struct SceneFogVolume;

/// Marks a reflection-probe entity (`Add > Reflection Probe`), so
/// viewport overlays draw a box gizmo at the probe's influence region.
/// The box is the unit cube scaled by the entity's `Transform.scale`.
#[derive(Component, Default, Reflect)]
#[reflect(Component, @crate::EditorHidden)]
pub struct SceneReflectionProbe;

/// Marks an animation-player entity (`Add > Animation Player`) so
/// viewport overlays draw a marker gizmo for it. The entity has no
/// spatial extent, so the marker is the only on-screen cue.
#[derive(Component, Default, Reflect)]
#[reflect(Component, @crate::EditorHidden)]
pub struct SceneAnimationPlayer;

/// Marks an audio-source entity (`Add > Audio Source`) so viewport
/// overlays draw a marker gizmo for it. The entity has no spatial
/// extent, so the marker is the only on-screen cue.
#[derive(Component, Default, Reflect)]
#[reflect(Component, @crate::EditorHidden)]
pub struct SceneAudioSource;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntityTemplate {
    Empty,
    Cube,
    Sphere,
    PointLight,
    DirectionalLight,
    SpotLight,
    RectLight,
    Camera3d,
    #[cfg(feature = "camera_rig")]
    CameraRig,
    Plane,
    Cylinder,
    Wedge,
    Cone,
    Pyramid,
    FogVolume,
    AnimationPlayer,
    AudioSource,
}

impl EntityTemplate {
    pub fn label(self) -> &'static str {
        match self {
            Self::Empty => "Empty Entity",
            Self::Cube => "Cube",
            Self::Sphere => "Sphere",
            Self::PointLight => "Point Light",
            Self::DirectionalLight => "Directional Light",
            Self::SpotLight => "Spot Light",
            Self::RectLight => "Rect Light",
            Self::Camera3d => "Camera",
            #[cfg(feature = "camera_rig")]
            Self::CameraRig => "Camera Rig",
            Self::Plane => "Plane",
            Self::Cylinder => "Cylinder",
            Self::Wedge => "Wedge",
            Self::Cone => "Cone",
            Self::Pyramid => "Pyramid",
            Self::FogVolume => "Fog Volume",
            Self::AnimationPlayer => "Animation Player",
            Self::AudioSource => "Audio Source",
        }
    }
}

pub fn create_entity(
    commands: &mut Commands,
    template: EntityTemplate,
    selection: &mut Selection,
) -> Entity {
    let entity = match template {
        EntityTemplate::Empty => commands
            .spawn((
                Name::new("Empty"),
                EmptyEntity,
                Transform::default(),
                // Required so `InheritedVisibility` exists on the
                // entity. Without it, viewport-overlay systems that
                // gate on `InheritedVisibility` (e.g. the empty
                // wireframe gizmo) silently skip the entity.
                Visibility::default(),
            ))
            .id(),
        EntityTemplate::Cube => {
            let id = commands
                .spawn((
                    Name::new("Cube"),
                    crate::brush::Brush::cuboid(0.5, 0.5, 0.5),
                    Transform::default(),
                    Visibility::default(),
                ))
                .id();
            commands.queue(apply_last_material(id));
            id
        }
        EntityTemplate::Sphere => {
            let id = commands
                .spawn((
                    Name::new("Sphere"),
                    crate::brush::Brush::sphere(0.5),
                    Transform::default(),
                    Visibility::default(),
                ))
                .id();
            commands.queue(apply_last_material(id));
            id
        }
        EntityTemplate::PointLight => commands
            .spawn((
                Name::new("Point Light"),
                SceneLight,
                PointLight::default(),
                Transform::from_xyz(0.0, 3.0, 0.0),
            ))
            .id(),
        EntityTemplate::DirectionalLight => commands
            .spawn((
                Name::new("Directional Light"),
                SceneLight,
                // The sun of its world: aurora draws its disc in the sky (SunDisk) and lights
                // by its illuminance.
                DirectionalLight {
                    illuminance: 20_000.0,
                    ..default()
                },
                SunDisk::EARTH,
                Transform::from_rotation(Quat::from_euler(EulerRot::XYZ, -0.8, 0.4, 0.0))
                    .with_translation(Vec3 {
                        x: 0.0,
                        y: 10.0,
                        z: 0.0,
                    }),
            ))
            .id(),
        EntityTemplate::SpotLight => commands
            .spawn((
                Name::new("Spot Light"),
                SceneLight,
                SpotLight::default(),
                Transform::from_xyz(0.0, 3.0, 0.0).looking_at(Vec3::ZERO, Vec3::Y),
            ))
            .id(),
        // A panel light facing down (it emits along its local -Z).
        EntityTemplate::RectLight => commands
            .spawn((
                Name::new("Rect Light"),
                SceneLight,
                RectLight {
                    width: 1.0,
                    height: 1.0,
                    ..default()
                },
                Transform::from_xyz(0.0, 3.0, 0.0).looking_at(Vec3::ZERO, Vec3::Z),
            ))
            .id(),
        EntityTemplate::Camera3d => commands
            .spawn((
                Name::new("Camera"),
                SceneCamera,
                Camera3d::default(),
                Camera {
                    // Scene cameras are authored inactive so they don't
                    // render over the editor viewport. They become active
                    // at play time (or via a future "preview through this
                    // camera" operator).
                    is_active: false,
                    ..default()
                },
                bevy::camera::RenderTarget::None {
                    size: UVec2::splat(1),
                },
                Transform::from_xyz(0.0, 2.0, 5.0).looking_at(Vec3::ZERO, Vec3::Y),
            ))
            .id(),
        #[cfg(feature = "camera_rig")]
        EntityTemplate::CameraRig => commands
            .spawn((
                Name::new("Camera Rig"),
                jackdaw_camera_rig::CameraRig::default(),
                Transform::default(),
                Visibility::default(),
            ))
            .id(),
        EntityTemplate::Plane => {
            let id = commands
                .spawn((
                    Name::new("Plane"),
                    crate::brush::Brush::plane(0.5, 0.5),
                    Transform::default(),
                    Visibility::default(),
                ))
                .id();
            commands.queue(apply_last_material(id));
            id
        }
        EntityTemplate::Cylinder => {
            let id = commands
                .spawn((
                    Name::new("Cylinder"),
                    crate::brush::Brush::cylinder(0.5, 0.5, 16),
                    Transform::default(),
                    Visibility::default(),
                ))
                .id();
            commands.queue(apply_last_material(id));
            id
        }
        EntityTemplate::Wedge => {
            let id = commands
                .spawn((
                    Name::new("Wedge"),
                    crate::brush::Brush::wedge(0.5, 0.5, 0.5),
                    Transform::default(),
                    Visibility::default(),
                ))
                .id();
            commands.queue(apply_last_material(id));
            id
        }
        EntityTemplate::Cone => {
            let id = commands
                .spawn((
                    Name::new("Cone"),
                    crate::brush::Brush::cone(0.5, 0.5, 16),
                    Transform::default(),
                    Visibility::default(),
                ))
                .id();
            commands.queue(apply_last_material(id));
            id
        }
        EntityTemplate::Pyramid => {
            let id = commands
                .spawn((
                    Name::new("Pyramid"),
                    crate::brush::Brush::pyramid(0.5, 0.5, 0.5),
                    Transform::default(),
                    Visibility::default(),
                ))
                .id();
            commands.queue(apply_last_material(id));
            id
        }
        EntityTemplate::FogVolume => commands
            .spawn((
                Name::new("Fog Volume"),
                bevy::light::FogVolume::default(),
                SceneFogVolume,
                Transform::default(),
                Visibility::default(),
            ))
            .id(),
        EntityTemplate::AnimationPlayer => commands
            .spawn((
                Name::new("Animation Player"),
                SceneAnimationPlayer,
                Transform::default(),
                Visibility::default(),
            ))
            .id(),
        EntityTemplate::AudioSource => commands
            .spawn((
                Name::new("Audio Source"),
                SceneAudioSource,
                Transform::default(),
                Visibility::default(),
            ))
            .id(),
    };

    selection.select_single(commands, entity);
    entity
}

/// Returns a command that applies the last-used material to all faces of a brush entity.
fn apply_last_material(entity: Entity) -> impl FnOnce(&mut World) {
    move |world: &mut World| {
        let last_mat = world
            .resource::<crate::brush::LastUsedMaterial>()
            .material
            .clone();
        if let Some(mat) = last_mat
            && let Some(mut brush) = world.get_mut::<crate::brush::Brush>(entity)
        {
            for face in &mut brush.faces {
                face.material = mat.clone();
            }
        }
    }
}

/// Spawn a template into the live world and register it in the scene document.
pub fn spawn_template_in_document(world: &mut World, template: EntityTemplate) -> Entity {
    let mut system_state: SystemState<(Commands, ResMut<Selection>)> = SystemState::new(world);
    let Ok((mut commands, mut selection)) = system_state.get_mut(world) else {
        return Entity::PLACEHOLDER;
    };
    let entity = create_entity(&mut commands, template, &mut selection);
    system_state.apply(world);
    crate::scene_io::adopt_entity(world, entity);
    if world.get::<crate::brush::Brush>(entity).is_some() {
        crate::physics_brush_bridge::insert_default_brush_physics(world, entity);
    }
    entity
}

/// Kept for callers that seed a fresh scene; the ECS is the document, so there is nothing to make.
pub(crate) fn ensure_scene_document(_world: &mut World) {}

/// Seed an empty live 3D document with a directional light. A UI document has
/// nothing to light.
pub(crate) fn seed_new_scene_defaults(world: &mut World) {
    ensure_scene_document(world);
    spawn_template_in_document(world, EntityTemplate::DirectionalLight);
}

/// What [`seed_2d_scene_root`] names the root it makes. Space-free, so an
/// operator clause can address it as `name=Scene2d`.
pub const SCENE_2D_ROOT_NAME: &str = "Scene2d";

/// Seed the root a new 2D scene starts from: one marked, transformed node
/// sprites are parented to. The marker is reflected, so a reopened document is
/// recognised as 2D again.
pub fn seed_2d_scene_root(world: &mut World) -> Entity {
    ensure_scene_document(world);
    let root = world
        .spawn((
            Name::new(SCENE_2D_ROOT_NAME),
            jackdaw_scene_types::Scene2dRoot,
            Transform::default(),
            Visibility::default(),
        ))
        .id();
    crate::scene_io::adopt_entity(world, root);
    crate::selection::select_only(world, root);
    root
}

/// World-access version of `create_entity`. Used from menu actions and other deferred contexts.
/// Pushes a `SpawnEntity` command so the addition can be undone.
pub fn create_entity_in_world(world: &mut World, template: EntityTemplate) {
    let label = format!("Add {}", template.label());
    let spawn_fn = Box::new(move |world: &mut World| -> Entity {
        spawn_template_in_document(world, template)
    });

    let mut cmd: Box<dyn EditorCommand> = Box::new(crate::commands::SpawnEntity {
        spawned: None,
        spawn_fn,
        label,
    });
    cmd.execute(world);
    world.resource_mut::<CommandHistory>().push_executed(cmd);
}

pub fn spawn_gltf(
    commands: &mut Commands,
    path: &str,
    position: Vec3,
    selection: &mut Selection,
) -> Entity {
    let file_name = Path::new(path)
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "GLTF Model".to_string());
    let scene_index = 0;
    // Store the asset-relative path, not the browser's absolute one: the
    // runtime and other machines cannot strip this project's assets prefix,
    // and an unapproved absolute path is refused outright by the asset server.
    let asset_path = to_asset_path(path);
    let entity = commands
        .spawn((
            Name::new(file_name),
            GltfSource {
                path: asset_path,
                scene_index,
            },
            Transform::from_translation(position),
        ))
        .id();
    selection.select_single(commands, entity);
    entity
}

fn spawn_gltf_in_world(world: &mut World, path: &str, position: Vec3) {
    let mut system_state: SystemState<(Commands, ResMut<Selection>)> = SystemState::new(world);
    let Ok((mut commands, mut selection)) = system_state.get_mut(world) else {
        return;
    };
    let entity = spawn_gltf(&mut commands, path, position, &mut selection);
    system_state.apply(world);
    crate::scene_io::adopt_entity(world, entity);
}

pub fn delete_selected(world: &mut World) {
    let entities: Vec<Entity> = world.resource::<Selection>().entities.clone();
    delete_entities(world, &entities);
}

/// Delete these entities as one history entry, dropping each from the
/// selection.
pub fn delete_entities(world: &mut World, entities: &[Entity]) {
    if entities.is_empty() {
        return;
    }

    let mut cmds: Vec<Box<dyn EditorCommand>> = Vec::new();
    for &entity in entities {
        if world.get_entity(entity).is_err() {
            continue;
        }
        if world.get::<EditorEntity>(entity).is_some() {
            continue;
        }
        cmds.push(Box::new(DespawnEntity::from_world(world, entity)));
    }

    // Drop Selected and Selection before despawn so neither holds ids
    // of entities that are about to disappear.
    for &entity in entities {
        if let Ok(mut ec) = world.get_entity_mut(entity) {
            ec.remove::<Selected>();
        }
    }
    let mut selection = world.resource_mut::<Selection>();
    selection.entities.retain(|held| !entities.contains(held));

    // Execute all despawn commands
    for cmd in &mut cmds {
        cmd.execute(world);
    }

    // Push as a single group command
    if !cmds.is_empty() {
        let group = crate::commands::CommandGroup {
            commands: cmds,
            label: "Delete entities".to_string(),
        };
        let mut history = world.resource_mut::<CommandHistory>();
        history.push_executed(Box::new(group));
    }
}

/// How many siblings the list `parent` names holds. `None` is the scene's own
/// root list.
fn sibling_count(world: &mut World, parent: Option<Entity>) -> usize {
    match parent {
        Some(parent) => world.get::<Children>(parent).map_or(0, Children::len),
        None => crate::scene_io::scene_roots(world).len(),
    }
}

/// Move every selected entity one slot along its own sibling list, earlier for
/// `delta` of -1 and later for 1, as one history entry. Each list keeps a
/// frontier -- the nearest slot still free -- so a selection reaching the end
/// packs against it keeping its own order.
pub(crate) fn move_selected_siblings(world: &mut World, delta: isize) {
    let selected: Vec<Entity> = world.resource::<Selection>().entities.clone();
    let mut located: Vec<(Entity, HierarchyLocation)> = selected
        .into_iter()
        .filter(|&entity| {
            world.get_entity(entity).is_ok() && world.get::<EditorEntity>(entity).is_none()
        })
        .map(|entity| {
            let location = HierarchyLocation::from_world(world, entity);
            (entity, location)
        })
        .collect();
    // Nearest the destination first, so two entities cannot swap past each other.
    located.sort_by_key(|(_, location)| location.index);
    if delta > 0 {
        located.reverse();
    }

    let mut moves: Vec<Box<dyn EditorCommand>> = Vec::new();
    let mut lists: Vec<Option<Entity>> = Vec::new();
    // The nearest free slot to the destination, per sibling list.
    let mut frontiers: Vec<(Option<Entity>, isize)> = Vec::new();
    for (entity, _) in located {
        let old = HierarchyLocation::from_world(world, entity);
        let here = old.index as isize;
        let end = if delta > 0 {
            sibling_count(world, old.parent) as isize - 1
        } else {
            0
        };
        let frontier = frontiers
            .iter()
            .find(|(parent, _)| *parent == old.parent)
            .map_or(end, |(_, at)| *at);
        let target = if delta > 0 {
            (here + delta).min(frontier)
        } else {
            (here + delta).max(frontier)
        };
        let blocked = if delta > 0 {
            target <= here
        } else {
            target >= here
        };
        let settled = if blocked { here } else { target };
        // Whatever the entity settled on, the next one cannot have it.
        let next = if delta > 0 { settled - 1 } else { settled + 1 };
        match frontiers
            .iter_mut()
            .find(|(parent, _)| *parent == old.parent)
        {
            Some((_, at)) => *at = next,
            None => frontiers.push((old.parent, next)),
        }
        if blocked {
            continue;
        }
        let index = settled as usize;
        let mut command = MoveEntity::new(
            world,
            entity,
            HierarchyLocation {
                parent: old.parent,
                index,
            },
        );
        command.execute(world);
        moves.push(Box::new(command));
        if !lists.contains(&old.parent) {
            lists.push(old.parent);
        }
    }

    let entry: Box<dyn EditorCommand> = match moves.len() {
        0 => return,
        1 => moves.pop().expect("one move"),
        _ => Box::new(crate::commands::CommandGroup {
            commands: moves,
            label: "Reorder entities".to_string(),
        }),
    };
    world.resource_mut::<CommandHistory>().push_executed(entry);
    for list in lists {
        crate::hierarchy::sync_outliner_row_order(world, list);
    }
}

/// Duplicate the selected entities, each landing just after its original, as one history entry.
pub fn duplicate_selected(world: &mut World) {
    let entities = selection_roots(world);
    if entities.is_empty() {
        return;
    }
    let mut new_entities = Vec::new();
    let mut commands: Vec<Box<dyn EditorCommand>> = Vec::new();
    for entity in entities {
        let Some(text) = entities_as_bsn(world, &[entity]) else {
            continue;
        };
        let location = HierarchyLocation::from_world(world, entity);
        let target = PasteTarget {
            parent: location.parent,
            index: location.index + 1,
        };
        let spawned = spawn_clipboard_at(world, &text, target);
        new_entities.extend(spawned.iter().copied());
        commands.push(Box::new(PasteEntitiesCommand {
            spawned,
            text,
            target,
            label: "Duplicate entity".to_string(),
        }));
    }
    select_entities(world, &new_entities);
    let entry: Box<dyn EditorCommand> = match commands.len() {
        0 => return,
        1 => commands.pop().expect("one command"),
        _ => Box::new(crate::commands::CommandGroup {
            commands,
            label: "Duplicate entities".to_string(),
        }),
    };
    world.resource_mut::<CommandHistory>().push_executed(entry);
}

/// The selected scene entities that have no selected ancestor.
fn selection_roots(world: &World) -> Vec<Entity> {
    let selected: Vec<Entity> = world.resource::<Selection>().entities.clone();
    selected
        .iter()
        .copied()
        .filter(|&entity| {
            world.get::<crate::scene_io::SceneEntity>(entity).is_some()
                && !crate::instances::is_inherited(world, entity)
        })
        .filter(|&entity| {
            let mut at = world.get::<ChildOf>(entity).map(ChildOf::parent);
            while let Some(parent) = at {
                if selected.contains(&parent) {
                    return false;
                }
                at = world.get::<ChildOf>(parent).map(ChildOf::parent);
            }
            true
        })
        .collect()
}

/// Give every named entity under `roots` a name no other scene entity has.
fn assign_unique_entity_names(world: &mut World, roots: &[Entity]) {
    let mut fresh = Vec::new();
    let mut stack = roots.to_vec();
    while let Some(entity) = stack.pop() {
        fresh.push(entity);
        if let Some(children) = world.get::<Children>(entity) {
            stack.extend(children.iter());
        }
    }
    let mut taken = std::collections::HashSet::new();
    let mut query = world.query_filtered::<(Entity, &Name), With<crate::scene_io::SceneEntity>>();
    for (entity, name) in query.iter(world) {
        if !fresh.contains(&entity) {
            taken.insert(name.as_str().to_owned());
        }
    }
    for entity in fresh {
        let Some(name) = world.get::<Name>(entity).map(|name| name.as_str().to_owned()) else {
            continue;
        };
        if let Some(free) = claim_free_name(&mut taken, &name) {
            world.entity_mut(entity).insert(Name::new(free));
        }
    }
}

/// Every `Name` on a scene entity, editor chrome excluded.
pub(crate) fn scene_entity_names(world: &mut World) -> std::collections::HashSet<String> {
    let mut names = std::collections::HashSet::new();
    let mut query = world.query_filtered::<&Name, With<crate::scene_io::SceneEntity>>();
    for existing in query.iter(world) {
        names.insert(existing.as_str().to_owned());
    }
    names
}

/// Reserve `name` in `taken`, and say what it had to become: `None` when it was
/// free, otherwise the next free `BaseN` suffix, also claimed. The suffix
/// carries no space, which an operator clause could not address.
pub(crate) fn claim_free_name(
    taken: &mut std::collections::HashSet<String>,
    name: &str,
) -> Option<String> {
    if taken.insert(name.to_owned()) {
        return None;
    }

    // `Button2` and `Button 2` renumber from `Button`, not from themselves.
    let trimmed = name.trim_end_matches(|c: char| c.is_ascii_digit());
    let base = if trimmed.is_empty() {
        name.to_owned()
    } else {
        trimmed.trim_end().to_owned()
    };

    let mut max_num = 0u32;
    for existing in taken.iter() {
        if existing == &base {
            max_num = max_num.max(1);
        } else if let Some(rest) = existing.strip_prefix(base.as_str())
            && let Ok(n) = rest.trim_start().parse::<u32>()
        {
            max_num = max_num.max(n);
        }
    }
    let free = format!("{base}{}", max_num + 1);
    taken.insert(free.clone());
    Some(free)
}

/// Snap a vector to the nearest cardinal world axis (+/-X, +/-Y, +/-Z).
/// Returns a signed unit vector along the axis with the largest absolute component.
fn snap_to_nearest_axis(v: Vec3) -> Vec3 {
    let abs = v.abs();
    if abs.x >= abs.y && abs.x >= abs.z {
        Vec3::new(v.x.signum(), 0.0, 0.0)
    } else if abs.y >= abs.x && abs.y >= abs.z {
        Vec3::new(0.0, v.y.signum(), 0.0)
    } else {
        Vec3::new(0.0, 0.0, v.z.signum())
    }
}

/// Derive TrenchBroom-style rotation axes from the camera transform.
///
/// - **Yaw** (left/right arrows): always world Y. Vertical rotation is always intuitive.
/// - **Roll** (up/down arrows): camera forward projected to horizontal, snapped to nearest
///   world axis, then negated. This is the axis you're "looking along".
/// - **Pitch** (PageUp/PageDown): camera right snapped to nearest world axis. If it
///   collides with the roll axis, use the cross product with Y instead.
pub(crate) fn camera_snapped_rotation_axes(gt: &GlobalTransform) -> (Vec3, Vec3, Vec3) {
    let yaw_axis = Vec3::Y;

    // Forward projected onto the horizontal plane, snapped to nearest axis
    let fwd = gt.forward().as_vec3();
    let fwd_horiz = Vec3::new(fwd.x, 0.0, fwd.z);
    let roll_axis = if fwd_horiz.length_squared() > 1e-6 {
        -snap_to_nearest_axis(fwd_horiz)
    } else {
        // Looking straight down/up, use camera up projected horizontally instead.
        let up = gt.up().as_vec3();
        let up_horiz = Vec3::new(up.x, 0.0, up.z);
        if up_horiz.length_squared() > 1e-6 {
            snap_to_nearest_axis(up_horiz)
        } else {
            Vec3::NEG_Z
        }
    };

    // Right snapped to nearest axis, with deduplication against roll
    let right = gt.right().as_vec3();
    let mut pitch_axis = snap_to_nearest_axis(right);
    if pitch_axis.abs() == roll_axis.abs() {
        // Collision, derive perpendicular horizontal axis.
        pitch_axis = snap_to_nearest_axis(yaw_axis.cross(roll_axis));
    }

    (yaw_axis, roll_axis, pitch_axis)
}

pub(crate) enum TransformReset {
    Position,
    Rotation,
    Scale,
}

pub(crate) fn reset_transform_selected(world: &mut World, reset: TransformReset) {
    let selection = world.resource::<Selection>();
    let entities: Vec<Entity> = selection.entities.clone();

    if entities.is_empty() {
        return;
    }

    let mut cmds: Vec<Box<dyn EditorCommand>> = Vec::new();

    for &entity in &entities {
        if world.get_entity(entity).is_err() {
            continue;
        }
        let Some(&old_transform) = world.get::<Transform>(entity) else {
            continue;
        };

        let new_transform = match reset {
            TransformReset::Position => Transform {
                translation: Vec3::ZERO,
                ..old_transform
            },
            TransformReset::Rotation => Transform {
                rotation: Quat::IDENTITY,
                ..old_transform
            },
            TransformReset::Scale => Transform {
                scale: Vec3::ONE,
                ..old_transform
            },
        };

        if old_transform == new_transform {
            continue;
        }

        let mut cmd = crate::commands::SetTransform {
            entity,
            old_transform,
            new_transform,
        };
        cmd.execute(world);
        cmds.push(Box::new(cmd));
    }

    if !cmds.is_empty() {
        let label = match reset {
            TransformReset::Position => "Reset position",
            TransformReset::Rotation => "Reset rotation",
            TransformReset::Scale => "Reset scale",
        };
        let group = crate::commands::CommandGroup {
            commands: cmds,
            label: label.to_string(),
        };
        let mut history = world.resource_mut::<CommandHistory>();
        history.push_executed(Box::new(group));
    }
}

pub(crate) fn nudge_selected(world: &mut World, offset: Vec3) {
    let selection = world.resource::<Selection>();
    let entities: Vec<Entity> = selection.entities.clone();

    if entities.is_empty() {
        return;
    }

    let mut cmds: Vec<Box<dyn EditorCommand>> = Vec::new();

    for &entity in &entities {
        if world.get_entity(entity).is_err() {
            continue;
        }
        let Some(&old_transform) = world.get::<Transform>(entity) else {
            continue;
        };

        let new_transform = Transform {
            translation: old_transform.translation + offset,
            ..old_transform
        };

        let mut cmd = crate::commands::SetTransform {
            entity,
            old_transform,
            new_transform,
        };
        cmd.execute(world);
        cmds.push(Box::new(cmd));
    }

    if !cmds.is_empty() {
        let group = crate::commands::CommandGroup {
            commands: cmds,
            label: "Nudge".to_string(),
        };
        let mut history = world.resource_mut::<CommandHistory>();
        history.push_executed(Box::new(group));
    }
}

pub(crate) fn rotate_selected(world: &mut World, rotation: Quat) {
    let selection = world.resource::<Selection>();
    let entities: Vec<Entity> = selection.entities.clone();

    if entities.is_empty() {
        return;
    }

    let mut cmds: Vec<Box<dyn EditorCommand>> = Vec::new();

    for &entity in &entities {
        if world.get_entity(entity).is_err() {
            continue;
        }
        let Some(&old_transform) = world.get::<Transform>(entity) else {
            continue;
        };

        let new_transform = Transform {
            rotation: rotation * old_transform.rotation,
            ..old_transform
        };

        let mut cmd = crate::commands::SetTransform {
            entity,
            old_transform,
            new_transform,
        };
        cmd.execute(world);
        cmds.push(Box::new(cmd));
    }

    if !cmds.is_empty() {
        let group = crate::commands::CommandGroup {
            commands: cmds,
            label: "Rotate 90\u{00b0}".to_string(),
        };
        let mut history = world.resource_mut::<CommandHistory>();
        history.push_executed(Box::new(group));
    }
}

/// `roots` and what is under them as `.bsn` text, in the shape a saved scene uses.
fn entities_as_bsn(world: &mut World, roots: &[Entity]) -> Option<String> {
    let settings = crate::scene_io::write_settings(world);
    match bevy::bsn_asset::write_scene_roots_text(world, roots, &settings) {
        Ok(text) if !text.trim().is_empty() => Some(text),
        Ok(_) => None,
        Err(err) => {
            warn!("Copy: {err}");
            None
        }
    }
}

/// The selection as `.bsn` text. `None` when nothing selected is in the scene.
fn selection_as_bsn(world: &mut World) -> Option<String> {
    let roots = selection_roots(world);
    if roots.is_empty() {
        return None;
    }
    entities_as_bsn(world, &roots)
}

/// Put `text` on the OS clipboard, and on the editor's own [`EntityClipboard`]
/// either way, so a run with no OS clipboard still copies and pastes.
fn write_clipboard(world: &mut World, text: String) {
    if let Some(mut clipboard) = world.get_resource_mut::<SystemClipboard>() {
        clipboard.last_emitted = text.clone();
        if let Err(error) = clipboard.clipboard.set_text(&text) {
            warn!("Copy: system clipboard failed ({error}), keeping the editor's own copy");
        }
    }
    world.resource_mut::<EntityClipboard>().text = text;
}

/// The largest clipboard payload a paste will read. Anything past it is refused
/// before it is parsed.
const MAX_CLIPBOARD_BYTES: usize = 2 * 1024 * 1024;

/// What a refused paste says.
const NOT_ENTITIES: &str = "the clipboard does not hold entities";

/// What a paste refused for its size says, with the cap in the sentence.
fn too_large_to_paste(bytes: usize) -> String {
    format!("the clipboard holds {bytes} bytes, past the {MAX_CLIPBOARD_BYTES} a paste reads")
}

/// Whether `text` is an entity document this editor can paste: within `MAX_CLIPBOARD_BYTES`,
/// parsing, and holding a patch of a registered type.
fn is_entity_document(world: &World, text: &str) -> bool {
    if text.len() > MAX_CLIPBOARD_BYTES {
        warn!(
            "Paste: the clipboard holds {} bytes, past the {MAX_CLIPBOARD_BYTES} a paste reads",
            text.len()
        );
        return false;
    }
    let Ok(document) = bevy::bsn::BsnDocument::parse(text) else {
        return false;
    };
    let registry = world.resource::<AppTypeRegistry>().clone();
    let registry = registry.read();
    document.nodes.iter().any(|node| match &node.kind {
        bevy::bsn::BsnNodeKind::Patch { symbol, .. } => {
            let path = symbol.to_type_path();
            registry.get_with_type_path(&path).is_some()
                || registry.get_with_short_type_path(&path).is_some()
        }
        _ => false,
    })
}

/// The scene text a paste should spawn from, or `None` when the clipboard holds
/// no entities. OS text this editor last emitted is answered from the
/// [`EntityClipboard`] mirror; other text is accepted only when it reads as an
/// entity document, and is otherwise refused rather than falling back.
fn clipboard_entities(world: &mut World) -> Option<String> {
    let own = world
        .get_resource::<EntityClipboard>()
        .map(|clipboard| clipboard.text.clone())
        .filter(|text| !text.trim().is_empty());

    let os_text = world
        .get_resource_mut::<SystemClipboard>()
        .and_then(|mut clipboard| {
            let text = clipboard.clipboard.get_text().ok()?;
            Some((text, clipboard.last_emitted.clone()))
        })
        .filter(|(text, _)| !text.trim().is_empty());

    choose_clipboard_text(world, own, os_text)
}

/// `clipboard_entities` with both clipboards already read, so the choice can be
/// exercised without an OS clipboard. `os` pairs its text with our last
/// emission.
fn choose_clipboard_text(
    world: &World,
    own: Option<String>,
    os: Option<(String, String)>,
) -> Option<String> {
    let Some((text, last_emitted)) = os else {
        // No readable OS clipboard: the mirror is the whole answer.
        return own;
    };
    if text == last_emitted {
        return own.or(Some(text));
    }
    is_entity_document(world, &text).then_some(text)
}

/// Copy selected entities to the clipboard as BSN text.
fn copy_components(world: &mut World) {
    let Some(text) = selection_as_bsn(world) else {
        return;
    };
    write_clipboard(world, text);
}

/// Undo entry for a paste or a duplicate: undo despawns what it spawned, redo spawns the text
/// again.
struct PasteEntitiesCommand {
    spawned: Vec<Entity>,
    text: String,
    target: PasteTarget,
    label: String,
}

impl crate::commands::EditorCommand for PasteEntitiesCommand {
    /// Only a redo reaches this: `push_executed` does not run what it is given.
    fn execute(&mut self, world: &mut World) {
        self.spawned = spawn_clipboard_at(world, &self.text, self.target);
        select_entities(world, &self.spawned);
    }

    fn undo(&mut self, world: &mut World) {
        let spawned = std::mem::take(&mut self.spawned);
        crate::commands::deselect_entities(world, &spawned);
        for entity in spawned {
            crate::commands::despawn_scene_entity(world, entity);
        }
    }

    fn description(&self) -> &str {
        &self.label
    }
}

/// Spawn a new brush as a copy of `source` (parent, physics, modifiers and other components
/// follow it) with new geometry. Children are not copied.
pub(crate) fn spawn_cloned_brush_with_geometry(
    world: &mut World,
    source: Entity,
    brush: crate::brush::Brush,
    transform: Transform,
) -> Option<Entity> {
    let text = entities_as_bsn(world, &[source])?;
    let location = HierarchyLocation::from_world(world, source);
    let target = PasteTarget {
        parent: location.parent,
        index: usize::MAX,
    };
    let entity = *spawn_clipboard_at(world, &text, target).first()?;
    let children: Vec<Entity> = world
        .get::<Children>(entity)
        .map(|children| children.iter().collect())
        .unwrap_or_default();
    for child in children {
        if world.get::<crate::scene_io::SceneEntity>(child).is_some() {
            world.entity_mut(child).despawn();
        }
    }
    world.entity_mut(entity).insert((brush, transform));
    Some(entity)
}

/// Where a paste lands.
#[derive(Clone, Copy, Default)]
struct PasteTarget {
    /// `None` is the scene's own root list.
    parent: Option<Entity>,
    /// Sibling index the first pasted root takes; the rest follow it.
    index: usize,
}

impl PasteTarget {
    /// The live location, or the end of the scene root list when the parent is gone.
    fn resolve(&self, world: &World) -> HierarchyLocation {
        match self.parent {
            Some(parent) if world.get_entity(parent).is_err() => HierarchyLocation {
                parent: None,
                index: usize::MAX,
            },
            parent => HierarchyLocation {
                parent,
                index: self.index,
            },
        }
    }
}

/// Sibling straight after the primary selection, which is where a paste goes.
/// With no usable selection, the end of the UI scene's root or of the scene's
/// own root list.
fn paste_target(world: &mut World) -> PasteTarget {
    let primary = world
        .get_resource::<Selection>()
        .and_then(Selection::primary);
    if let Some(primary) = primary
        && world.get::<crate::scene_io::SceneEntity>(primary).is_some()
    {
        let location = HierarchyLocation::from_world(world, primary);
        return PasteTarget {
            parent: location.parent,
            index: location.index + 1,
        };
    }
    PasteTarget {
        parent: crate::ui_palette::ui_scene_root(world),
        index: usize::MAX,
    }
}

/// Spawn clipboard text at `target`, with names no other scene entity has.
fn spawn_clipboard_at(world: &mut World, text: &str, target: PasteTarget) -> Vec<Entity> {
    let source = world
        .resource::<crate::scene_io::SceneFilePath>()
        .path
        .clone()
        .map(|path| crate::scene_io::asset_path_of(world, Path::new(&path)))
        .unwrap_or_default();
    let spawned = match crate::scene_io::spawn_bsn_text(world, text, &source) {
        Ok(spawned) => spawned,
        Err(err) => {
            warn!("Paste: {err}");
            return Vec::new();
        }
    };
    assign_unique_entity_names(world, &spawned);
    let location = target.resolve(world);
    for (offset, &root) in spawned.iter().enumerate() {
        crate::commands::place_entity(
            world,
            root,
            HierarchyLocation {
                parent: location.parent,
                index: location.index.saturating_add(offset),
            },
            crate::commands::WorldTransform::Unplaced,
        );
    }
    crate::hierarchy::sync_outliner_row_order(world, location.parent);
    spawned
}

/// Whether the open document is a UI scene, which decides what may be pasted.
fn open_scene_is_ui(world: &mut World) -> bool {
    crate::ui_palette::ui_scene_root(world).is_some()
}

/// What kind of scene pasted roots belong in. A `Node` is what makes a root a UI node.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum PayloadKind {
    /// Every root is a UI node.
    Ui,
    /// No root is.
    World,
    /// Some are and some are not, so no one scene holds them all.
    Mixed,
}

fn payload_kind(world: &World, roots: &[Entity]) -> PayloadKind {
    let ui = roots
        .iter()
        .filter(|&&root| world.get::<Node>(root).is_some())
        .count();
    if ui == 0 {
        PayloadKind::World
    } else if ui == roots.len() {
        PayloadKind::Ui
    } else {
        PayloadKind::Mixed
    }
}

/// Paste entities from clipboard scene text at `target`, as one history entry,
/// returning the pasted roots, which become the selection.
fn paste_clipboard_entities(world: &mut World, text: &str, target: PasteTarget) -> Vec<Entity> {
    if text.trim().is_empty() {
        return Vec::new();
    }
    let spawned = spawn_clipboard_at(world, text, target);
    if spawned.is_empty() {
        crate::status_bar::notify_error(world, NOT_ENTITIES);
        return Vec::new();
    }

    // A UI node in a world, or a mesh in a screen, neither draws nor saves right.
    let kind = payload_kind(world, &spawned);
    let scene_is_ui = open_scene_is_ui(world);
    let refusal = match (kind, scene_is_ui) {
        (PayloadKind::Mixed, _) => {
            Some("the clipboard holds both UI nodes and world entities, and no scene holds both")
        }
        (PayloadKind::Ui, false) => {
            Some("the clipboard holds UI nodes, and this is not a UI scene")
        }
        (PayloadKind::World, true) => {
            Some("the clipboard holds world entities, and this is a UI scene")
        }
        _ => None,
    };
    if let Some(message) = refusal {
        for entity in spawned {
            crate::commands::despawn_scene_entity(world, entity);
        }
        crate::status_bar::notify_error(world, message);
        return Vec::new();
    }

    select_entities(world, &spawned);
    info!("Pasted {} entities", spawned.len());
    let cmd = PasteEntitiesCommand {
        spawned: spawned.clone(),
        text: text.to_string(),
        target,
        label: "Paste entities".to_string(),
    };
    world
        .resource_mut::<CommandHistory>()
        .push_executed(Box::new(cmd));
    spawned
}

/// Make `entities` the whole selection.
fn select_entities(world: &mut World, entities: &[Entity]) {
    for &entity in &world.resource::<Selection>().entities.clone() {
        if let Ok(mut ec) = world.get_entity_mut(entity) {
            ec.remove::<Selected>();
        }
    }
    world.resource_mut::<Selection>().entities = entities.to_vec();
    for &entity in entities {
        if let Ok(mut ec) = world.get_entity_mut(entity) {
            ec.insert(Selected);
        }
    }
}

/// Paste clipboard entities at the end of the scene's root list.
fn paste_components(world: &mut World) {
    paste_clipboard(
        world,
        PasteTarget {
            parent: None,
            index: usize::MAX,
        },
    );
}

/// Paste clipboard entities as the sibling straight after the selection.
fn paste_entities_after_selection(world: &mut World) {
    let target = paste_target(world);
    paste_clipboard(world, target);
}

/// Put whatever the clipboard holds into the scene at `target`, entities before
/// images: a clipboard holds both at once, and a screenshot taken between a copy
/// and its paste must not swallow the subtree.
fn paste_clipboard(world: &mut World, target: PasteTarget) {
    if let Some(text) = clipboard_entities(world) {
        paste_clipboard_entities(world, &text, target);
        return;
    }
    if crate::asset_ingest::paste_clipboard_image(world) {
        return;
    }
    // The size cap refuses before the parse, so say which of the two it was.
    let oversized = world
        .get_resource_mut::<SystemClipboard>()
        .and_then(|mut clipboard| clipboard.clipboard.get_text().ok())
        .map(|text| text.len())
        .filter(|&bytes| bytes > MAX_CLIPBOARD_BYTES);
    match oversized {
        Some(bytes) => crate::status_bar::notify_error(world, too_large_to_paste(bytes)),
        None => crate::status_bar::notify_error(world, NOT_ENTITIES),
    }
}

/// Copy the selection to the clipboard as BSN text.
fn copy_selected_entities(world: &mut World) {
    let Some(text) = selection_as_bsn(world) else {
        return;
    };
    write_clipboard(world, text);
}

/// Copy the selection and then delete it. The copy records nothing, so the
/// delete's entry is the whole cut.
fn cut_selected_entities(world: &mut World) {
    let Some(text) = selection_as_bsn(world) else {
        return;
    };
    write_clipboard(world, text);
    delete_selected(world);
}

/// Flip `Visibility` between hidden and inherited on every selected entity,
/// writing through the document. It pushes no history entry of its own: the
/// dispatcher's before/after snapshot pair is the entry.
fn hide_selected(world: &mut World) {
    let selection = world.resource::<Selection>();
    let entities: Vec<Entity> = selection.entities.clone();

    if entities.is_empty() {
        return;
    }

    for &entity in &entities {
        let current = world
            .get::<Visibility>(entity)
            .copied()
            .unwrap_or(Visibility::Inherited);

        let new_visibility = match current {
            Visibility::Hidden => Visibility::Inherited,
            _ => Visibility::Hidden,
        };

        let mut cmd = set_visibility(entity, current, new_visibility);
        cmd.execute(world);
    }
}

fn set_visibility(entity: Entity, old: Visibility, new: Visibility) -> crate::commands::SetField {
    crate::commands::SetField {
        entity,
        type_path: "bevy_camera::visibility::Visibility".to_string(),
        field_path: String::new(),
        old_value: Some(Box::new(old)),
        new_value: Box::new(new),
    }
}

// FIXME: this breaks down whenever an extension uses `Name`
#[derive(SystemParam, Deref, DerefMut)]
struct SceneEntities<'w, 's> {
    query: Query<
        'w,
        's,
        (Entity, &'static Visibility),
        (With<Name>, With<crate::scene_io::SceneEntity>, Without<Node>),
    >,
}

fn unhide_all_entities(world: &mut World, scene_entities: &mut SystemState<SceneEntities>) {
    let mut cmds: Vec<Box<dyn EditorCommand>> = Vec::new();

    // Only unhide top-level scene entities (with Name), matching hide_unselected logic.
    let hidden: Vec<Entity> = {
        let Ok(entities) = scene_entities.get(world) else {
            return;
        };
        entities
            .iter()
            .filter(|(_, vis)| **vis == Visibility::Hidden)
            .map(|(e, _)| e)
            .collect()
    };

    for entity in hidden {
        let mut cmd = set_visibility(entity, Visibility::Hidden, Visibility::Inherited);
        cmd.execute(world);
        cmds.push(Box::new(cmd));
    }

    if !cmds.is_empty() {
        let group = crate::commands::CommandGroup {
            commands: cmds,
            label: "Unhide all".to_string(),
        };
        let mut history = world.resource_mut::<CommandHistory>();
        history.push_executed(Box::new(group));
    }
}

fn hide_all_entities(world: &mut World, scene_entities: &mut SystemState<SceneEntities>) {
    let mut cmds: Vec<Box<dyn EditorCommand>> = Vec::new();

    // Hide all top-level scene entities (same filter as H, applied to everything).
    let to_hide: Vec<(Entity, Visibility)> = {
        let Ok(entities) = scene_entities.get(world) else {
            return;
        };
        entities
            .iter()
            .filter(|(_, vis)| **vis != Visibility::Hidden)
            .map(|(e, vis)| (e, *vis))
            .collect()
    };

    for (entity, current) in to_hide {
        let mut cmd = set_visibility(entity, current, Visibility::Hidden);
        cmd.execute(world);
        cmds.push(Box::new(cmd));
    }

    if !cmds.is_empty() {
        let group = crate::commands::CommandGroup {
            commands: cmds,
            label: "Hide all".to_string(),
        };
        let mut history = world.resource_mut::<CommandHistory>();
        history.push_executed(Box::new(group));
    }
}

/// Convert a filesystem path to a Bevy asset path (relative to the assets directory).
///
/// Bevy's default asset source reads from `<base>/assets/` where `<base>` is
/// `BEVY_ASSET_ROOT`, `CARGO_MANIFEST_DIR`, or the executable's parent directory.
///
/// The rule itself lives in [`jackdaw_scene_types::to_asset_path`], which the
/// standalone runtime applies to the same authored paths; this only supplies
/// the editor's notion of where the assets directory is.
pub fn to_asset_path(path: &str) -> String {
    jackdaw_scene_types::to_asset_path(path, get_assets_base_dir().as_deref())
}

/// Get the absolute path of Bevy's assets directory.
///
/// The open project's `assets/` comes from the resident `ProjectRoot` mirror
/// rather than from disk, because this runs once per model per frame; with no
/// project open it falls back to the recents file and `FileAssetReader`.
pub fn get_assets_base_dir() -> Option<std::path::PathBuf> {
    if let Some(assets) = crate::project::open_project_assets_dir() {
        return Some(dunce::simplified(assets.as_path()).to_path_buf());
    }

    if let Some(project_dir) = crate::project::read_last_project() {
        let assets = dunce::simplified(project_dir.as_path()).join("assets");
        if assets.is_dir() {
            return Some(assets);
        }
    }

    let base = if let Ok(dir) = std::env::var("BEVY_ASSET_ROOT") {
        std::path::PathBuf::from(dir)
    } else if let Ok(dir) = std::env::var("CARGO_MANIFEST_DIR") {
        std::path::PathBuf::from(dir)
    } else {
        std::env::current_exe().ok()?.parent()?.to_path_buf()
    };
    Some(dunce::simplified(base.as_path()).join("assets"))
}

// ----------------------- Operators ----------------------------
//
// Entity-level operators (`entity.*`) and the `Add` menu
// (`entity.add.*`). Keybind and menu dispatch both arrive here.
// Operators are gated with `is_available = can_act_on_entities` so
// they refuse to fire while a brush sub-element drag or modal
// operator has the scene locked, matching the guards the legacy
// `handle_entity_keys` applied.

use jackdaw_api::prelude::*;
use jackdaw_api_internal::keymap::PresetInput;

use crate::core_extension::CoreExtensionInputContext;

pub(crate) fn add_to_extension(ctx: &mut ExtensionContext) {
    ctx.register_operator::<EntityDeleteOp>()
        .register_operator::<EntityDuplicateOp>()
        .register_operator::<EntityCopyOp>()
        .register_operator::<EntityCutOp>()
        .register_operator::<EntityPasteOp>()
        .register_operator::<EntityMoveUpOp>()
        .register_operator::<EntityMoveDownOp>()
        .register_operator::<EntityPlaceGltfOp>()
        .register_operator::<EntityCopyComponentsOp>()
        .register_operator::<EntityPasteComponentsOp>()
        .register_operator::<EntityToggleVisibilityOp>()
        .register_operator::<EntityHideUnselectedOp>()
        .register_operator::<EntityUnhideAllOp>()
        .register_operator::<EntityAddCubeOp>()
        .register_operator::<EntityAddSphereOp>()
        .register_operator::<EntityAddPointLightOp>()
        .register_operator::<EntityAddDirectionalLightOp>()
        .register_operator::<EntityAddSpotLightOp>()
        .register_operator::<EntityAddRectLightOp>()
        .register_operator::<EntityAddCameraOp>();
    #[cfg(feature = "camera_rig")]
    ctx.register_operator::<EntityAddCameraRigOp>();
    ctx.register_operator::<EntityAddEmptyOp>()
        .register_operator::<EntityAddImageOp>()
        .register_operator::<EntityAddTerrainOp>()
        .register_operator::<EntityAddPrefabOp>()
        .register_operator::<EntityAddPlaneOp>()
        .register_operator::<EntityAddCylinderOp>()
        .register_operator::<EntityAddWedgeOp>()
        .register_operator::<EntityAddConeOp>()
        .register_operator::<EntityAddPyramidOp>()
        .register_operator::<EntityAddAnimationPlayerOp>()
        .register_operator::<EntityAddAudioSourceOp>()
        .register_operator::<EntityAddFogVolumeOp>()
        .register_operator::<EntityAddReflectionProbeOp>()
        // Registered on core rather than on the UI Widgets extension, so it
        // reports an unknown widget name rather than disappearing with it.
        .register_operator::<crate::ui_palette::WidgetAddOp>()
        .register_operator::<crate::add_entity_picker::EntityAddPickerOp>();


    ctx.bind_operator::<CoreExtensionInputContext, EntityDeleteOp>([PresetInput::key("Delete")]);
    ctx.bind_operator::<CoreExtensionInputContext, EntityDuplicateOp>([
        PresetInput::key("KeyD").ctrl()
    ]);
    // The timeline claims the same chords for keyframes; the two availability
    // checks are disjoint on it being focused, so one press answers once.
    ctx.bind_operator::<CoreExtensionInputContext, EntityCopyOp>([PresetInput::key("KeyC").ctrl()]);
    ctx.bind_operator::<CoreExtensionInputContext, EntityCutOp>([PresetInput::key("KeyX").ctrl()]);
    ctx.bind_operator::<CoreExtensionInputContext, EntityPasteOp>(
        [PresetInput::key("KeyV").ctrl()],
    );
    // Ctrl+Shift is the whole-component clipboard, which pastes at the scene
    // root rather than beside the selection.
    ctx.bind_operator::<CoreExtensionInputContext, EntityCopyComponentsOp>([PresetInput::key(
        "KeyC",
    )
    .ctrl()
    .shift()]);
    ctx.bind_operator::<CoreExtensionInputContext, EntityPasteComponentsOp>([PresetInput::key(
        "KeyV",
    )
    .ctrl()
    .shift()]);
    ctx.bind_operator::<CoreExtensionInputContext, EntityMoveUpOp>([
        PresetInput::key("ArrowUp").ctrl()
    ]);
    ctx.bind_operator::<CoreExtensionInputContext, EntityMoveDownOp>([PresetInput::key(
        "ArrowDown",
    )
    .ctrl()]);
    ctx.bind_operator::<CoreExtensionInputContext, EntityToggleVisibilityOp>([PresetInput::key(
        "KeyH",
    )]);
    ctx.bind_operator::<CoreExtensionInputContext, EntityUnhideAllOp>([
        PresetInput::key("KeyH").ctrl()
    ]);
    ctx.bind_operator::<CoreExtensionInputContext, EntityHideUnselectedOp>([PresetInput::key(
        "KeyH",
    )
    .alt()]);
    ctx.bind_operator::<CoreExtensionInputContext, crate::add_entity_picker::EntityAddPickerOp>([
        PresetInput::key("KeyA").ctrl(),
    ]);
}

/// Shared availability check for entity manipulation operators.
///
/// Refuses while a text input has focus, a modal is in flight, or the timeline
/// is focused, so the keyframe operators sharing those chords answer alone.
/// Typing is asked through `KeybindFocus`, since `InputFocus` reports the
/// primary window as focused when nothing has claimed it.
pub(crate) fn can_act_on_entities(
    keybind_focus: crate::keybind_focus::KeybindFocus,
    active: ActiveModalQuery,
    modal: Res<crate::modal_transform::ModalTransformState>,
    draw_state: Res<crate::draw_brush::DrawBrushState>,
    edit_mode: Res<crate::brush::EditMode>,
    panel_focus: crate::panel_focus::PanelFocus,
) -> bool {
    if keybind_focus.keyboard_is_spoken_for() || active.is_modal_running() || modal.active.is_some()
    {
        return false;
    }
    if draw_state.active.is_some() {
        return false;
    }
    if panel_focus.is_focused(TIMELINE_WINDOW_ID) {
        return false;
    }

    matches!(*edit_mode, crate::brush::EditMode::Object)
}

/// The timeline panel, which claims the clipboard chords and Delete in its own
/// bounds.
pub(crate) const TIMELINE_WINDOW_ID: &str = "jackdaw.timeline";

// -- Entity lifecycle --------------------------------------------

#[operator(
    id = "entity.place_gltf",
    label = "Place GLTF",
    description = "Place a GLTF asset into the active scene at a world position.",
    allows_undo = true,
    params(
        path(String, doc = "Path to the GLTF asset."),
        pos_x(f64, doc = "World-space X position."),
        pos_y(
            f64,
            doc = "World-space Y position. The terrain height under the point \
                   when omitted."
        ),
        pos_z(f64, doc = "World-space Z position."),
    )
)]
pub(crate) fn entity_place_gltf(
    params: In<OperatorParameters>,
    terrains: crate::terrain::ground::TerrainSurfaces,
    store: Res<crate::terrain::TerrainDataStore>,
    mut commands: Commands,
) -> OperatorResult {
    let Some(path) = params.as_str("path").map(str::to_owned) else {
        warn!("entity.place_gltf: missing `path` param");
        return OperatorResult::Cancelled;
    };
    let Some(x) = params.as_float("pos_x") else {
        warn!("entity.place_gltf: missing `pos_x` param");
        return OperatorResult::Cancelled;
    };
    let Some(z) = params.as_float("pos_z") else {
        warn!("entity.place_gltf: missing `pos_z` param");
        return OperatorResult::Cancelled;
    };
    let ground =
        || crate::terrain::ground::height_under(&terrains, &store, Vec2::new(x as f32, z as f32));
    // Placing a model on the ground is what a caller with no viewport asks
    // for, so an omitted height is the ground rather than a refusal.
    let y = params
        .as_float("pos_y")
        .map(|y| y as f32)
        .or_else(ground)
        .unwrap_or(0.0);
    let position = Vec3::new(x as f32, y, z as f32);
    commands.queue(move |world: &mut World| {
        spawn_gltf_in_world(world, &path, position);
    });
    OperatorResult::Finished
}

#[operator(
    id = "entity.delete",
    label = "Delete",
    is_available = can_act_on_entities,
    params(
        entity(Entity, doc = "Entity to delete. Defaults to the selection."),
        entities(
            String,
            doc = "Entities to delete, as a comma-separated list of ids. Defaults \
                   to the selection."
        ),
    )
)]
pub(crate) fn entity_delete(
    params: In<OperatorParameters>,
    selection: Res<Selection>,
    authored: Query<(), Without<crate::EditorEntity>>,
    mut commands: Commands,
) -> OperatorResult {
    let targets = match crate::boot_ops::target_entities(&params, &selection, &authored) {
        Ok(targets) => targets,
        Err(refusal) => {
            commands.queue(move |world: &mut World| {
                warn_caller(world, format!("entity.delete: {refusal}"));
            });
            return OperatorResult::Cancelled;
        }
    };
    commands.queue(move |world: &mut World| {
        // Nodes of a held graph leave with their links, as one graph edit.
        let (held, scene): (Vec<Entity>, Vec<Entity>) = targets
            .iter()
            .partition(|&&entity| crate::animgraph::held::is_held(world, entity));
        for entity in held {
            crate::animgraph::document::delete_node(world, entity);
        }
        if !scene.is_empty() {
            delete_entities(world, &scene);
        }
    });
    OperatorResult::Finished
}

#[operator(
    id = "entity.duplicate",
    label = "Duplicate",
    is_available = can_act_on_entities
)]
pub(crate) fn entity_duplicate(
    _: In<OperatorParameters>,
    mut commands: Commands,
) -> OperatorResult {
    commands.queue(duplicate_selected);
    OperatorResult::Finished
}

#[operator(
    id = "entity.move_up",
    label = "Move Up",
    description = "Move the selection one slot earlier among its siblings.",
    allows_undo = false,
    is_available = can_act_on_entities
)]
pub(crate) fn entity_move_up(_: In<OperatorParameters>, mut commands: Commands) -> OperatorResult {
    commands.queue(|world: &mut World| move_selected_siblings(world, -1));
    OperatorResult::Finished
}

#[operator(
    id = "entity.move_down",
    label = "Move Down",
    description = "Move the selection one slot later among its siblings.",
    allows_undo = false,
    is_available = can_act_on_entities
)]
pub(crate) fn entity_move_down(
    _: In<OperatorParameters>,
    mut commands: Commands,
) -> OperatorResult {
    commands.queue(|world: &mut World| move_selected_siblings(world, 1));
    OperatorResult::Finished
}

#[operator(
    id = "entity.copy",
    label = "Copy",
    description = "Copy the selected subtrees to the clipboard as scene text.",
    allows_undo = false,
    is_available = can_act_on_entities
)]
pub(crate) fn entity_copy(_: In<OperatorParameters>, mut commands: Commands) -> OperatorResult {
    commands.queue(copy_selected_entities);
    OperatorResult::Finished
}

#[operator(
    id = "entity.cut",
    label = "Cut",
    description = "Copy the selected subtrees to the clipboard and delete them.",
    allows_undo = false,
    is_available = can_act_on_entities
)]
pub(crate) fn entity_cut(_: In<OperatorParameters>, mut commands: Commands) -> OperatorResult {
    commands.queue(cut_selected_entities);
    OperatorResult::Finished
}

#[operator(
    id = "entity.paste",
    label = "Paste",
    description = "Paste the clipboard's subtrees as siblings after the selection.",
    allows_undo = false,
    is_available = can_act_on_entities
)]
pub(crate) fn entity_paste(_: In<OperatorParameters>, mut commands: Commands) -> OperatorResult {
    commands.queue(paste_entities_after_selection);
    OperatorResult::Finished
}

#[operator(
    id = "entity.copy_components",
    label = "Copy Components",
    allows_undo = false,
    is_available = can_act_on_entities
)]
pub(crate) fn entity_copy_components(
    _: In<OperatorParameters>,
    mut commands: Commands,
) -> OperatorResult {
    commands.queue(copy_components);
    OperatorResult::Finished
}

#[operator(
    id = "entity.paste_components",
    label = "Paste Components",
    is_available = can_act_on_entities
)]
pub(crate) fn entity_paste_components(
    _: In<OperatorParameters>,
    mut commands: Commands,
) -> OperatorResult {
    commands.queue(paste_components);
    OperatorResult::Finished
}

#[operator(
    id = "entity.toggle_visibility",
    label = "Toggle Visibility",
    is_available = can_act_on_entities
)]
pub(crate) fn entity_toggle_visibility(
    _: In<OperatorParameters>,
    mut commands: Commands,
) -> OperatorResult {
    commands.queue(hide_selected);
    OperatorResult::Finished
}

#[operator(
    id = "entity.hide_unselected",
    label = "Hide Unselected",
    allows_undo = false,
    is_available = can_act_on_entities
)]
pub(crate) fn entity_hide_unselected(
    _: In<OperatorParameters>,
    mut commands: Commands,
) -> OperatorResult {
    commands.queue(|world: &mut World| {
        if let Err(err) = world.run_system_cached(hide_all_entities) {
            warn!("hide_all_entities: {err:?}");
        }
    });
    OperatorResult::Finished
}

#[operator(
    id = "entity.unhide_all",
    label = "Unhide All",
    allows_undo = false,
    is_available = can_act_on_entities
)]
pub(crate) fn entity_unhide_all(
    _: In<OperatorParameters>,
    mut commands: Commands,
) -> OperatorResult {
    commands.queue(|world: &mut World| {
        if let Err(err) = world.run_system_cached(unhide_all_entities) {
            warn!("unhide_all_entities: {err:?}");
        }
    });
    OperatorResult::Finished
}

// -- Add menu ----------------------------------------------------

#[operator(id = "entity.add.cube", label = "Cube")]
pub(crate) fn entity_add_cube(_: In<OperatorParameters>, mut commands: Commands) -> OperatorResult {
    commands.queue(|world: &mut World| {
        create_entity_in_world(world, EntityTemplate::Cube);
    });
    OperatorResult::Finished
}

#[operator(id = "entity.add.sphere", label = "Sphere")]
pub(crate) fn entity_add_sphere(
    _: In<OperatorParameters>,
    mut commands: Commands,
) -> OperatorResult {
    commands.queue(|world: &mut World| {
        create_entity_in_world(world, EntityTemplate::Sphere);
    });
    OperatorResult::Finished
}

#[operator(id = "entity.add.point_light", label = "Point Light")]
pub(crate) fn entity_add_point_light(
    _: In<OperatorParameters>,
    mut commands: Commands,
) -> OperatorResult {
    commands.queue(|world: &mut World| {
        create_entity_in_world(world, EntityTemplate::PointLight);
    });
    OperatorResult::Finished
}

#[operator(id = "entity.add.directional_light", label = "Directional Light")]
pub(crate) fn entity_add_directional_light(
    _: In<OperatorParameters>,
    mut commands: Commands,
) -> OperatorResult {
    commands.queue(|world: &mut World| {
        create_entity_in_world(world, EntityTemplate::DirectionalLight);
    });
    OperatorResult::Finished
}

#[operator(id = "entity.add.spot_light", label = "Spot Light")]
pub(crate) fn entity_add_spot_light(
    _: In<OperatorParameters>,
    mut commands: Commands,
) -> OperatorResult {
    commands.queue(|world: &mut World| {
        create_entity_in_world(world, EntityTemplate::SpotLight);
    });
    OperatorResult::Finished
}

#[operator(id = "entity.add.rect_light", label = "Rect Light")]
pub(crate) fn entity_add_rect_light(
    _: In<OperatorParameters>,
    mut commands: Commands,
) -> OperatorResult {
    commands.queue(|world: &mut World| {
        create_entity_in_world(world, EntityTemplate::RectLight);
    });
    OperatorResult::Finished
}

#[operator(id = "entity.add.camera", label = "Camera")]
pub(crate) fn entity_add_camera(
    _: In<OperatorParameters>,
    mut commands: Commands,
) -> OperatorResult {
    commands.queue(|world: &mut World| {
        create_entity_in_world(world, EntityTemplate::Camera3d);
    });
    OperatorResult::Finished
}

#[cfg(feature = "camera_rig")]
#[operator(id = "entity.add.camera_rig", label = "Camera Rig")]
pub(crate) fn entity_add_camera_rig(
    _: In<OperatorParameters>,
    mut commands: Commands,
) -> OperatorResult {
    commands.queue(|world: &mut World| {
        create_entity_in_world(world, EntityTemplate::CameraRig);
    });
    OperatorResult::Finished
}

#[operator(id = "entity.add.image", label = "Reference Image")]
pub fn entity_add_image(_: In<OperatorParameters>, mut commands: Commands) -> OperatorResult {
    commands.queue(crate::reference_image::open_reference_image_picker);
    OperatorResult::Finished
}

#[operator(id = "entity.add.empty", label = "Empty")]
pub(crate) fn entity_add_empty(
    _: In<OperatorParameters>,
    mut commands: Commands,
) -> OperatorResult {
    commands.queue(|world: &mut World| {
        create_entity_in_world(world, EntityTemplate::Empty);
    });
    OperatorResult::Finished
}

#[operator(id = "entity.add.plane", label = "Plane")]
pub(crate) fn entity_add_plane(
    _: In<OperatorParameters>,
    mut commands: Commands,
) -> OperatorResult {
    commands.queue(|world: &mut World| {
        create_entity_in_world(world, EntityTemplate::Plane);
    });
    OperatorResult::Finished
}

#[operator(id = "entity.add.cylinder", label = "Cylinder")]
pub(crate) fn entity_add_cylinder(
    _: In<OperatorParameters>,
    mut commands: Commands,
) -> OperatorResult {
    commands.queue(|world: &mut World| {
        create_entity_in_world(world, EntityTemplate::Cylinder);
    });
    OperatorResult::Finished
}

#[operator(id = "entity.add.wedge", label = "Wedge")]
pub(crate) fn entity_add_wedge(
    _: In<OperatorParameters>,
    mut commands: Commands,
) -> OperatorResult {
    commands.queue(|world: &mut World| {
        create_entity_in_world(world, EntityTemplate::Wedge);
    });
    OperatorResult::Finished
}

#[operator(id = "entity.add.cone", label = "Cone")]
pub(crate) fn entity_add_cone(_: In<OperatorParameters>, mut commands: Commands) -> OperatorResult {
    commands.queue(|world: &mut World| {
        create_entity_in_world(world, EntityTemplate::Cone);
    });
    OperatorResult::Finished
}

#[operator(id = "entity.add.pyramid", label = "Pyramid")]
pub(crate) fn entity_add_pyramid(
    _: In<OperatorParameters>,
    mut commands: Commands,
) -> OperatorResult {
    commands.queue(|world: &mut World| {
        create_entity_in_world(world, EntityTemplate::Pyramid);
    });
    OperatorResult::Finished
}

#[operator(id = "entity.add.animation_player", label = "Animation Player")]
pub(crate) fn entity_add_animation_player(
    _: In<OperatorParameters>,
    mut commands: Commands,
) -> OperatorResult {
    commands.queue(|world: &mut World| {
        create_entity_in_world(world, EntityTemplate::AnimationPlayer);
    });
    OperatorResult::Finished
}

#[operator(id = "entity.add.audio_source", label = "Audio Source")]
pub(crate) fn entity_add_audio_source(
    _: In<OperatorParameters>,
    mut commands: Commands,
) -> OperatorResult {
    commands.queue(|world: &mut World| {
        create_entity_in_world(world, EntityTemplate::AudioSource);
    });
    OperatorResult::Finished
}

#[operator(id = "entity.add.fog_volume", label = "Fog Volume")]
pub(crate) fn entity_add_fog_volume(
    _: In<OperatorParameters>,
    mut commands: Commands,
) -> OperatorResult {
    commands.queue(|world: &mut World| {
        create_entity_in_world(world, EntityTemplate::FogVolume);
    });
    OperatorResult::Finished
}

#[operator(id = "entity.add.reflection_probe", label = "Reflection Probe")]
pub(crate) fn entity_add_reflection_probe(
    _: In<OperatorParameters>,
    mut commands: Commands,
) -> OperatorResult {
    commands.queue(|world: &mut World| {
        crate::spawn_undoable(world, "Add Reflection Probe", |world| {
            let mut system_state: SystemState<(Commands, Res<AssetServer>, ResMut<Selection>)> =
                SystemState::new(world);
            let Ok((mut commands, asset_server, mut selection)) = system_state.get_mut(world)
            else {
                return Entity::PLACEHOLDER;
            };
            // Reuse the editor's shipped environment-map cubemaps as the
            // probe's reflection source, the same embedded asset the
            // viewport and material preview load.
            let diffuse_map = bevy::asset::load_embedded_asset!(
                &*asset_server,
                "../assets/environment_maps/voortrekker_interior_1k_diffuse.ktx2"
            );
            let specular_map = bevy::asset::load_embedded_asset!(
                &*asset_server,
                "../assets/environment_maps/voortrekker_interior_1k_specular.ktx2"
            );
            let entity = commands
                .spawn((
                    Name::new("Reflection Probe"),
                    LightProbe::default(),
                    EnvironmentMapLight {
                        diffuse_map,
                        specular_map,
                        intensity: 1000.0,
                        ..default()
                    },
                    SceneReflectionProbe,
                    Transform::from_scale(Vec3::splat(2.0)),
                    Visibility::default(),
                ))
                .id();
            selection.select_single(&mut commands, entity);
            system_state.apply(world);
            crate::scene_io::adopt_entity(world, entity);
            entity
        });
    });
    OperatorResult::Finished
}




#[operator(id = "entity.add.terrain", label = "Terrain")]
pub(crate) fn entity_add_terrain(
    _: In<OperatorParameters>,
    mut commands: Commands,
) -> OperatorResult {
    commands.queue(|world: &mut World| {
        let is_first_terrain = world
            .query_filtered::<Entity, With<jackdaw_scene_types::Terrain>>()
            .iter(world)
            .next()
            .is_none();
        crate::spawn_undoable(world, "Add Terrain", move |world| {
            let mut system_state: SystemState<(Commands, ResMut<Selection>)> =
                SystemState::new(world);
            let Ok((mut commands, mut selection)) = system_state.get_mut(world) else {
                return Entity::PLACEHOLDER;
            };
            let entity = crate::terrain::spawn_terrain_entity(&mut commands);
            selection.select_single(&mut commands, entity);
            system_state.apply(world);
            crate::scene_io::adopt_entity(world, entity);
            // Only the first terrain opens the panel: a later add must not
            // steal focus from the tab in use.
            if is_first_terrain {
                crate::open_window_in_default_area_if_absent(world, "jackdaw.inspector.terrain");
            }
            entity
        });
    });
    OperatorResult::Finished
}

/// Pick a `.bsn` file and place an instance of it (`:"file.bsn"`) at the origin.
#[operator(id = "entity.add.prefab", label = "Instance")]
pub fn entity_add_prefab(_: In<OperatorParameters>, mut commands: Commands) -> OperatorResult {
    commands.queue(open_instance_picker);
    OperatorResult::Finished
}

#[derive(Resource)]
struct InstancePicker(bevy::tasks::Task<Option<rfd::FileHandle>>);

fn open_instance_picker(world: &mut World) {
    open_instance_picker_in(world, None);
}

/// The instance picker, started in `folder` when given.
pub(crate) fn open_instance_picker_in(world: &mut World, folder: Option<std::path::PathBuf>) {
    if world.contains_resource::<InstancePicker>() {
        return;
    }
    let mut dialog =
        crate::native_dialog::file_dialog(world, crate::native_dialog::DialogPurpose::Prefab)
            .set_title("Place an instance of")
            .add_filter("Jackdaw scene", &["bsn"]);
    if let Some(folder) = folder {
        dialog = dialog.set_directory(folder);
    }
    let task = bevy::tasks::AsyncComputeTaskPool::get().spawn(
        crate::native_dialog::unless_suppressed(move || dialog.pick_file()),
    );
    world.insert_resource(InstancePicker(task));
}

pub(crate) fn poll_instance_picker(world: &mut World) {
    let Some(mut picker) = world.remove_resource::<InstancePicker>() else {
        return;
    };
    let Some(picked) = bevy::tasks::futures_lite::future::block_on(
        bevy::tasks::futures_lite::future::poll_once(&mut picker.0),
    ) else {
        world.insert_resource(picker);
        return;
    };
    let Some(file) = picked else {
        return;
    };
    let path = file.path().to_path_buf();
    crate::native_dialog::remember_pick(world, crate::native_dialog::DialogPurpose::Prefab, &path);
    place_instance_of(world, &path);
}

/// Place an instance of the `.bsn` file at `path` at the open scene's top level, selected and
/// undoable, once its bases have loaded.
pub(crate) fn place_instance_of(world: &mut World, path: &std::path::Path) {
    let base = crate::scene_io::asset_path_of(world, path);
    let placed_base = base.clone();
    crate::instances::queue_instance(world, &base, Transform::default(), None, move |world, placed| {
        let entity = match placed {
            Ok(entity) => entity,
            Err(err) => {
                crate::status_bar::notify_error(world, err);
                return;
            }
        };
        crate::scene_io::adopt_entity(world, entity);
        select_entities(world, &[entity]);
        let cmd = PlaceInstance {
            base: placed_base,
            spawned: Some(entity),
        };
        world.resource_mut::<CommandHistory>().push_executed(Box::new(cmd));
    });
}

/// Undo entry for placing an instance of a `.bsn` file.
struct PlaceInstance {
    base: String,
    spawned: Option<Entity>,
}

impl EditorCommand for PlaceInstance {
    fn execute(&mut self, world: &mut World) {
        match crate::instances::place_instance(world, &self.base, Transform::default(), None) {
            Ok(entity) => {
                crate::scene_io::adopt_entity(world, entity);
                self.spawned = Some(entity);
            }
            Err(err) => warn!("{err}"),
        }
    }

    fn undo(&mut self, world: &mut World) {
        if let Some(entity) = self.spawned.take() {
            crate::commands::deselect_entities(world, &[entity]);
            crate::commands::despawn_scene_entity(world, entity);
        }
    }

    fn description(&self) -> &str {
        "Place instance"
    }
}

#[cfg(test)]
mod asset_path_tests {
    use super::*;

    /// The directory named here does not exist, so a relative path coming back
    /// is proof nothing went to the filesystem. The mirror is process-global, so
    /// set, clear and the system driving it are one test rather than three.
    #[test]
    fn an_open_project_resolves_asset_paths_without_touching_the_disk() {
        crate::project::set_open_project_assets_dir(Some(std::path::PathBuf::from(
            "/jackdaw-no-such-project/assets",
        )));

        assert_eq!(
            to_asset_path("/jackdaw-no-such-project/assets/models/rock.glb"),
            "models/rock.glb"
        );

        // Cleared, the same path has no base to strip and comes back whole.
        crate::project::set_open_project_assets_dir(None);
        assert_eq!(
            to_asset_path("/jackdaw-no-such-project/assets/models/rock.glb"),
            "/jackdaw-no-such-project/assets/models/rock.glb"
        );

        // And what the mirror holds is what the resource says.
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let mut world = World::new();
        world.insert_resource(crate::project::ProjectRoot {
            root: root.clone(),
            config: default(),
        });
        world
            .run_system_cached(crate::project::mirror_open_project)
            .expect("the mirror runs");
        assert_eq!(
            crate::project::open_project_assets_dir(),
            Some(root.join("assets"))
        );

        world.remove_resource::<crate::project::ProjectRoot>();
        world
            .run_system_cached(crate::project::mirror_open_project)
            .expect("the mirror runs");
        assert_eq!(crate::project::open_project_assets_dir(), None);
    }
}

