//! What a game that plays editor-authored `.bsn` scenes adds beside bevy's own loader.
//!
//! Scenes spawn with bevy's `.bsn` loader (`ScenePatchInstance(assets.load("scene.bsn"))`);
//! [`JackdawPlugin`] registers the authored types and draws what they describe: brushes,
//! surfaces, terrain, material overrides, colliders, a baked navmesh.
//!
//! ```ignore
//! App::new()
//!     .add_plugins((DefaultPlugins, jackdaw_runtime::JackdawPlugin))
//!     .add_systems(Startup, |mut commands: Commands, assets: Res<AssetServer>| {
//!         commands.spawn(ScenePatchInstance(assets.load("scene.bsn")));
//!     })
//!     .run();
//! ```
//!
//! Add [`JackdawPlugin`] after `DefaultPlugins` so it can see how `AssetPlugin` was configured:
//! terrain sidecars and asset files are read from that folder directly.

use std::path::{Path, PathBuf};

use bevy::asset::{AssetPath, ReflectAsset, UntypedAssetId, UntypedHandle};
use bevy::bsn::{BsnDocument, BsnValue};
use bevy::bsn_asset::asset_value_from_document;
use bevy::image::ImageLoaderSettings;
use bevy::platform::collections::{HashMap, HashSet};
use bevy::prelude::*;
use bevy::scene::ScenePatchInstance;
#[cfg(feature = "render")]
use bevy::world_serialization::{WorldAsset, WorldAssetRoot};
#[cfg(feature = "render")]
use bevy_aurora::material::AuroraMaterial;

pub mod bsn_files;

pub use jackdaw_scene_types::{
    Brush, BrushFaceData, CustomProperties, DetailPresser, EditorCategory, EditorDescription,
    EditorHidden, EditorPreview, GltfSource, NAVMESH_EXCLUDE_TYPE_PATH, NavmeshExclude,
    PropertyValue, ScatterGroup, ScatterInstance, SkipSerialization,
};

#[cfg(feature = "render")]
mod material_overrides;
#[cfg(feature = "render")]
pub use material_overrides::{
    MaterialOverridesPlugin, ModelMaterial, dress_model, dress_part, material_of_reference,
    overrides_reaching,
};

#[cfg(feature = "terrain")]
mod terrain;
#[cfg(feature = "terrain")]
pub use jackdaw_terrain::render::{DetailPressers, DetailSettings, DetailViewer};
#[cfg(feature = "terrain")]
pub use terrain::TerrainViewer;

#[cfg(feature = "navmesh")]
mod navmesh;
#[cfg(feature = "navmesh")]
pub use navmesh::JackdawNavmesh;

pub mod prelude {
    #[cfg(feature = "navmesh")]
    pub use crate::JackdawNavmesh;
    pub use crate::{
        DetailPresser, EditorCategory, EditorDescription, EditorHidden, EditorPreview,
        JackdawCatalog, JackdawPlugin, SkipSerialization,
    };
    pub use bevy::scene::ScenePatchInstance;
    #[cfg(feature = "terrain")]
    pub use crate::{DetailPressers, DetailSettings, DetailViewer, TerrainViewer};
}

pub struct JackdawPlugin;

impl Plugin for JackdawPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(AssetFolder(asset_folder(app)));
        app.add_plugins(jackdaw_scene_types::SceneTypesPlugin {
            runtime_mesh_rebuild: true,
        });
        app.add_plugins(jackdaw_bind::JackdawBindPlugin);
        app.add_plugins(jackdaw_widgets_runtime::AuthoredWidgetPlugin);
        app.insert_resource(jackdaw_bind::ValueTextTarget(jackdaw_bind::BindPath::new(
            jackdaw_widgets_runtime::text_value_write_path(),
        )));
        app.configure_sets(
            PostUpdate,
            (
                jackdaw_widgets_runtime::AuthoredTextSystems,
                jackdaw_widgets_runtime::AuthoredNodeSystems,
                jackdaw_widgets_runtime::AuthoredChromeSystems,
            )
                .after(jackdaw_bind::BindEvaluationSystems),
        );
        app.add_plugins(CatalogPlugin);

        #[cfg(feature = "render")]
        app.add_plugins((
            MaterialTextureFormatPlugin,
            jackdaw_surface::LayeredSurfacePlugin,
            jackdaw_surface::FoliagePlugin,
            jackdaw_surface::WaterPlugin,
            jackdaw_surface::EnvironmentPlugin,
            MaterialOverridesPlugin,
        ))
        .add_systems(Update, attach_inserted_gltf_sources);

        #[cfg(feature = "navmesh")]
        app.add_systems(Update, navmesh::attach_navmeshes);
        #[cfg(feature = "terrain")]
        app.add_plugins(terrain::plugin);
        #[cfg(feature = "animation")]
        app.add_plugins(jackdaw_animation_runtime::AnimationRuntimePlugin);
        #[cfg(feature = "physics")]
        app.add_plugins(jackdaw_avian_integration::AvianColliderBridgePlugin);
    }
}

/// Loads every asset file under the asset folder into [`JackdawCatalog`] at startup.
pub struct CatalogPlugin;

impl Plugin for CatalogPlugin {
    fn build(&self, app: &mut App) {
        if !app.world().contains_resource::<AssetFolder>() {
            app.insert_resource(AssetFolder(asset_folder(app)));
        }
        app.init_resource::<JackdawCatalog>()
            .add_systems(Startup, load_asset_files);
    }
}

/// The asset files under the asset folder, by the path a reference spells (`materials/oak.bsn`)
/// and by the bare `@stem` an older reference spells when one file carries it.
#[derive(Resource, Default)]
pub struct JackdawCatalog {
    files: HashMap<String, UntypedHandle>,
    stems: HashMap<String, UntypedHandle>,
    paths: HashMap<UntypedAssetId, String>,
}

impl JackdawCatalog {
    pub fn get(&self, reference: &str) -> Option<&UntypedHandle> {
        self.files.get(reference).or_else(|| self.stems.get(reference))
    }

    /// The handle the catalog holds for `path`, when it holds one of `type_id`'s asset type.
    pub fn handle_for(&self, type_id: core::any::TypeId, path: &str) -> Option<UntypedHandle> {
        self.files
            .get(path)
            .filter(|handle| handle.type_id() == type_id)
            .cloned()
    }

    /// The file a handle the catalog holds was read from.
    pub fn path_of(&self, id: UntypedAssetId) -> Option<&String> {
        self.paths.get(&id)
    }

    /// Every reference the catalog answers to: paths and `@stem`s.
    pub fn references(&self) -> HashMap<String, UntypedHandle> {
        let mut all = self.stems.clone();
        all.extend(self.files.iter().map(|(k, v)| (k.clone(), v.clone())));
        all
    }

    /// Every handle the catalog holds, by the path it was read from.
    pub fn asset_paths(&self) -> HashMap<UntypedAssetId, String> {
        self.paths.clone()
    }

    pub fn insert(&mut self, path: String, handle: UntypedHandle) {
        self.stems
            .insert(format!("@{}", bsn_files::asset_stem(&path)), handle.clone());
        self.paths.insert(handle.id(), path.clone());
        self.files.insert(path, handle);
    }

    pub fn remove(&mut self, path: &str) -> Option<UntypedHandle> {
        let handle = self.files.remove(path)?;
        self.paths.remove(&handle.id());
        Some(handle)
    }

    pub fn clear(&mut self) {
        self.files.clear();
        self.stems.clear();
        self.paths.clear();
    }

    pub fn iter(&self) -> impl Iterator<Item = (&String, &UntypedHandle)> {
        self.files.iter()
    }

    pub fn len(&self) -> usize {
        self.files.len()
    }

    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }
}

fn load_asset_files(world: &mut World) {
    let Some(root) = assets_root(world) else {
        return;
    };
    let mut stems = bsn_files::StemIndex::default();
    let mut loaded = Vec::new();
    for path in bsn_files::walk_document_files(&root) {
        let Some(key) = assets_relative_key(&root, &path) else {
            continue;
        };
        stems.insert(PathBuf::from(&key));
        if let Some(handle) = load_asset_file(world, &path, &key) {
            loaded.push((key, handle));
        }
    }
    let count = loaded.len();
    let mut catalog = world.resource_mut::<JackdawCatalog>();
    for (key, handle) in loaded {
        let stem = bsn_files::asset_stem(&key).to_string();
        if stems.unique(&stem).is_some() {
            catalog.stems.insert(format!("@{stem}"), handle.clone());
        }
        catalog.paths.insert(handle.id(), key.clone());
        catalog.files.insert(key, handle);
    }
    if count > 0 {
        info!("loaded {count} asset files from {}", root.display());
    }
}

/// Load the asset value the file at `path` holds into its `Assets`, or nothing for a scene or a
/// type this app does not register as an asset. `source` is its asset path.
pub fn load_asset_file(world: &mut World, path: &Path, source: &str) -> Option<UntypedHandle> {
    let text = std::fs::read_to_string(path).ok()?;
    let type_path = bsn_files::asset_text_type(&text, path)?;
    let document = match BsnDocument::parse(&text) {
        Ok(document) => document,
        Err(err) => {
            warn!("{}: {err:?}", path.display());
            return None;
        }
    };
    load_asset_document(world, &document, source, &type_path)
}

/// [`load_asset_file`] over a document already parsed.
pub fn load_asset_document(
    world: &mut World,
    document: &BsnDocument,
    source: &str,
    type_path: &str,
) -> Option<UntypedHandle> {
    let registry = world.resource::<AppTypeRegistry>().clone();
    let registry = registry.read();
    let registration = registry.get_with_type_path(type_path)?;
    let reflect_asset = registration.data::<ReflectAsset>()?.clone();
    let server = world.resource::<AssetServer>().clone();
    let _linear = preload_linear_textures(&server, document);
    let mut handles = handle_provider(world);
    let value = match asset_value_from_document(
        document,
        source,
        registration,
        &registry,
        Some(&mut handles),
    ) {
        Ok((_, value)) => value,
        Err(err) => {
            warn!("{source}: {err}");
            return None;
        }
    };
    drop(registry);
    Some(reflect_asset.add(world, value.as_partial_reflect()))
}

/// What a `.bsn` build resolves a path to: the catalog's handle for that file when it holds one,
/// else a load through the asset server.
pub fn handle_provider(world: &World) -> impl FnMut(core::any::TypeId, AssetPath<'static>) -> UntypedHandle + use<> {
    let server = world.resource::<AssetServer>().clone();
    let held: HashMap<String, UntypedHandle> = world
        .get_resource::<JackdawCatalog>()
        .map(|catalog| catalog.files.clone())
        .unwrap_or_default();
    move |type_id, path: AssetPath<'static>| {
        held.get(&path.to_string())
            .filter(|handle| handle.type_id() == type_id)
            .cloned()
            .unwrap_or_else(|| server.load_builder().load_erased(type_id, path))
    }
}

/// Material slots holding non-color data, loaded without sRGB decoding.
const LINEAR_SLOTS: &[&str] = &[
    "normal_map_texture",
    "metallic_roughness_texture",
    "occlusion_texture",
    "depth_map",
    "layer_normal_map_texture",
    "layer_orm_texture",
    "detail_normal_map_texture",
    "detail_orm_texture",
];

/// Load the document's linear-slot textures as linear first, so the handles the build asks for
/// by path are these. Held until the asset takes its own.
fn preload_linear_textures(server: &AssetServer, document: &BsnDocument) -> Vec<UntypedHandle> {
    let mut paths = HashSet::new();
    for node in &document.values {
        let BsnValue::Struct(_, fields) = &node.value else {
            continue;
        };
        for (name, value) in fields {
            if !LINEAR_SLOTS.contains(&name.as_str()) {
                continue;
            }
            if let Some(BsnValue::String(path)) = document.value(*value).map(|v| &v.value)
                && !path.is_empty()
            {
                paths.insert(path.clone());
            }
        }
    }
    paths
        .into_iter()
        .map(|path| {
            server
                .load_builder()
                .with_settings(|s: &mut ImageLoaderSettings| s.is_srgb = false)
                .load::<Image>(path)
                .untyped()
        })
        .collect()
}

/// The directory of the scene file an entity was spawned from, relative to the asset folder:
/// the nearest `ScenePatchInstance` at or above it. Empty when none is.
pub fn scene_dir_of(world: &World, entity: Entity) -> PathBuf {
    let mut at = Some(entity);
    while let Some(current) = at {
        if let Some(path) = world
            .get::<ScenePatchInstance>(current)
            .and_then(|instance| instance.0.path())
        {
            return path.path().parent().map(Path::to_path_buf).unwrap_or_default();
        }
        at = world.get::<ChildOf>(current).map(ChildOf::parent);
    }
    PathBuf::new()
}

/// Where a file sits under the asset root, with forward slashes.
fn assets_relative_key(root: &Path, path: &Path) -> Option<String> {
    let relative = path.strip_prefix(root).ok()?;
    let mut key = String::new();
    for component in relative.components() {
        if !key.is_empty() {
            key.push('/');
        }
        key.push_str(component.as_os_str().to_str()?);
    }
    (!key.is_empty()).then_some(key)
}

/// The folder Bevy reads assets from, captured when the plugin is built.
#[derive(Resource, Clone, Debug)]
pub struct AssetFolder(pub Option<PathBuf>);

pub fn assets_root(world: &World) -> Option<PathBuf> {
    world
        .get_resource::<AssetFolder>()
        .and_then(|folder| folder.0.clone())
}

/// Where the app reads assets from: [`AssetPlugin::file_path`] resolved the way the file reader
/// resolves it.
fn asset_folder(app: &App) -> Option<PathBuf> {
    let file_path = app
        .get_added_plugins::<AssetPlugin>()
        .first()
        .map_or_else(|| AssetPlugin::default().file_path, |p| p.file_path.clone());
    let base = if let Ok(p) = std::env::var("BEVY_ASSET_ROOT") {
        PathBuf::from(p)
    } else if let Ok(p) = std::env::var("CARGO_MANIFEST_DIR") {
        PathBuf::from(p)
    } else {
        std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(ToOwned::to_owned))?
    };
    Some(base.join(file_path))
}

#[cfg(feature = "render")]
fn world_asset_root(
    asset_server: &AssetServer,
    source: &GltfSource,
    assets_dir: Option<&Path>,
) -> WorldAssetRoot {
    let path = jackdaw_scene_types::to_asset_path(&source.path, assets_dir);
    let scene: Handle<WorldAsset> =
        asset_server.load(format!("{path}#Scene{}", source.scene_index));
    WorldAssetRoot(scene)
}

/// Give each [`GltfSource`] the `WorldAssetRoot` that spawns the model, leaving one already
/// pointing at the same glTF scene alone.
#[cfg(feature = "render")]
fn attach_inserted_gltf_sources(
    added: Query<(Entity, &GltfSource), Changed<GltfSource>>,
    existing: Query<&WorldAssetRoot>,
    asset_folder: Option<Res<AssetFolder>>,
    asset_server: Res<AssetServer>,
    mut commands: Commands,
) {
    if added.is_empty() {
        return;
    }
    let assets_dir = asset_folder.and_then(|folder| folder.0.clone());
    for (entity, source) in &added {
        let root = world_asset_root(&asset_server, source, assets_dir.as_deref());
        if existing
            .get(entity)
            .is_ok_and(|current| current.0 == root.0)
        {
            continue;
        }
        commands.entity(entity).insert(root);
    }
}


/// The `Unorm` twin of a 16-bit `Uint` texture format: same bytes per texel,
/// same channel order, but float-filterable where the `Uint` side is not.
///
/// Bevy decodes 16-bit grayscale PNGs as `R16Uint` and grayscale+alpha as
/// `Rg16Uint`. A `AuroraMaterial` slot demands a filterable float sampler,
/// so binding either one fails the whole bind group.
#[cfg(feature = "render")]
fn filterable_twin(format: wgpu_types::TextureFormat) -> Option<wgpu_types::TextureFormat> {
    use wgpu_types::TextureFormat;
    match format {
        TextureFormat::R16Uint => Some(TextureFormat::R16Unorm),
        TextureFormat::Rg16Uint => Some(TextureFormat::Rg16Unorm),
        TextureFormat::Rgba16Uint => Some(TextureFormat::Rgba16Unorm),
        _ => None,
    }
}

/// The images a `AuroraMaterial` binds, over the slots every build has.
#[cfg(feature = "render")]
fn material_texture_ids(
    material: &AuroraMaterial,
) -> impl Iterator<Item = bevy::asset::AssetId<Image>> {
    [
        material.base_color_texture.as_ref(),
        material.emissive_texture.as_ref(),
        material.metallic_roughness_texture.as_ref(),
        material.normal_map_texture.as_ref(),
        material.occlusion_texture.as_ref(),
        material.depth_map.as_ref(),
    ]
    .into_iter()
    .flatten()
    .map(Handle::id)
}

/// Retags 16-bit `Uint` material textures as their filterable `Unorm`
/// twins.
///
/// The editor adds this plugin rather than keeping a copy of the retag, so a
/// pack of 16-bit maps renders the same in the editor and in a built game.
#[cfg(feature = "render")]
pub struct MaterialTextureFormatPlugin;

#[cfg(feature = "render")]
impl Plugin for MaterialTextureFormatPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(
            PostUpdate,
            promote_material_texture_formats.after(bevy::asset::AssetEventSystems),
        );
    }
}

/// Retag every 16-bit `Uint` image a material binds as its `Unorm` twin.
///
/// Descriptor only: the texels already have the layout the twin declares. Runs
/// after the asset events are published and before the render world extracts.
/// Both event streams matter, since an image may decode long after the material
/// naming it, or the other way round.
#[cfg(feature = "render")]
fn promote_material_texture_formats(
    mut image_events: MessageReader<AssetEvent<Image>>,
    mut material_events: MessageReader<AssetEvent<AuroraMaterial>>,
    materials: Res<Assets<AuroraMaterial>>,
    mut images: ResMut<Assets<Image>>,
) {
    use bevy::asset::AssetId;

    let touched: Vec<AssetId<Image>> = image_events
        .read()
        .filter_map(|event| match event {
            AssetEvent::Added { id }
            | AssetEvent::Modified { id }
            | AssetEvent::LoadedWithDependencies { id } => Some(*id),
            _ => None,
        })
        .collect();
    let materials_changed = material_events.read().any(|event| {
        matches!(
            event,
            AssetEvent::Added { .. } | AssetEvent::Modified { .. }
        )
    });
    if touched.is_empty() && !materials_changed {
        return;
    }

    let bound: HashSet<AssetId<Image>> = materials
        .iter()
        .flat_map(|(_, material)| material_texture_ids(material))
        .collect();
    let candidates: Vec<AssetId<Image>> = if materials_changed {
        bound.into_iter().collect()
    } else {
        touched
            .into_iter()
            .filter(|id| bound.contains(id))
            .collect()
    };

    for id in candidates {
        let Some(twin) = images
            .get(id)
            .and_then(|image| filterable_twin(image.texture_descriptor.format))
        else {
            continue;
        };
        if let Some(mut image) = images.get_mut(id) {
            image.texture_descriptor.format = twin;
        }
    }
}

/// The file the project's remaining named assets are read from, under the
/// asset root.


/// The one definition of the `Uint` retag, exercised through the plugin both
/// the editor and a built game add.
#[cfg(all(test, feature = "render"))]
mod material_texture_format_tests {
    use super::*;
    use bevy::app::App;
    use bevy::asset::{AssetApp, AssetPlugin, RenderAssetUsages};
    use wgpu_types::{Extent3d, TextureDimension, TextureFormat};

    fn promotion_app() -> App {
        let mut app = App::new();
        app.add_plugins((bevy::app::TaskPoolPlugin::default(), AssetPlugin::default()));
        app.init_asset::<Image>();
        app.init_asset::<AuroraMaterial>();
        app.add_plugins(MaterialTextureFormatPlugin);
        app
    }

    /// A one-texel image in `format`, sized from the texel it is given.
    fn raw_image(app: &mut App, texel: &[u8], format: TextureFormat) -> Handle<Image> {
        let image = Image::new(
            Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
            TextureDimension::D2,
            texel.to_vec(),
            format,
            RenderAssetUsages::default(),
        );
        app.world_mut().resource_mut::<Assets<Image>>().add(image)
    }

    fn format_of(app: &App, handle: &Handle<Image>) -> TextureFormat {
        app.world()
            .resource::<Assets<Image>>()
            .get(handle)
            .expect("image")
            .texture_descriptor
            .format
    }

    /// A 16-bit grayscale PNG decodes as `R16Uint`, which has no filterable
    /// sampler; bound to a material slot it fails the whole bind group.
    #[test]
    fn a_sixteen_bit_uint_image_a_material_binds_is_retagged_as_its_unorm_twin() {
        let mut app = promotion_app();
        let occlusion = raw_image(&mut app, &[0x00, 0x80], TextureFormat::R16Uint);
        let gray_alpha = raw_image(&mut app, &[0x00, 0x80, 0x00, 0xff], TextureFormat::Rg16Uint);
        let _material = app
            .world_mut()
            .resource_mut::<Assets<AuroraMaterial>>()
            .add(AuroraMaterial {
                occlusion_texture: Some(occlusion.clone()),
                depth_map: Some(gray_alpha.clone()),
                ..default()
            });

        app.update();

        assert_eq!(format_of(&app, &occlusion), TextureFormat::R16Unorm);
        assert_eq!(format_of(&app, &gray_alpha), TextureFormat::Rg16Unorm);
    }

    /// The texels are what they were: only the descriptor is rewritten.
    #[test]
    fn promotion_leaves_the_texels_alone() {
        let mut app = promotion_app();
        let image = raw_image(&mut app, &[0x34, 0x12], TextureFormat::R16Uint);
        let _material = app
            .world_mut()
            .resource_mut::<Assets<AuroraMaterial>>()
            .add(AuroraMaterial {
                depth_map: Some(image.clone()),
                ..default()
            });

        app.update();

        let images = app.world().resource::<Assets<Image>>();
        assert_eq!(
            images.get(&image).unwrap().data.as_deref(),
            Some(&[0x34u8, 0x12][..])
        );
    }

    /// An image no material binds keeps its format: an integer texture read
    /// with `textureLoad` is meant to stay integer.
    #[test]
    fn an_image_no_material_binds_keeps_its_uint_format() {
        let mut app = promotion_app();
        let unbound = raw_image(&mut app, &[0x00, 0x80], TextureFormat::R16Uint);

        app.update();

        assert_eq!(format_of(&app, &unbound), TextureFormat::R16Uint);
    }

    /// An image may decode before the material naming it, so the material's own
    /// event has to sweep the slots it just claimed.
    #[test]
    fn a_material_added_after_its_image_still_gets_it_promoted() {
        let mut app = promotion_app();
        let image = raw_image(&mut app, &[0x00, 0x80], TextureFormat::R16Uint);
        app.update();
        assert_eq!(format_of(&app, &image), TextureFormat::R16Uint);

        let _material = app
            .world_mut()
            .resource_mut::<Assets<AuroraMaterial>>()
            .add(AuroraMaterial {
                normal_map_texture: Some(image.clone()),
                ..default()
            });
        app.update();

        assert_eq!(format_of(&app, &image), TextureFormat::R16Unorm);
    }
}
