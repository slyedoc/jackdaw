//! The splat material: terrain shaded from its control map against a texture
//! set, with height-blended transitions between the two ids a control word
//! names. Behind the `render` feature, so the data model below it stays
//! buildable with no GPU crate in the graph.
//!
//! A surface wearing one ([`TerrainSplat3d`]) traces through aurora's surface class for it
//! (`aurora.rs`, `terrain_splat.slang`), which reads each slot's own maps through descriptor
//! handles.
//!
//! [`resolve_with`] maps each slot onto a [`TextureSetEntry`] and its images; finding the
//! material behind a name is the host's job, done through the closure passed to it.
//!
//! The blend is height-weighted rather than a cross-fade: a layer's weight is
//! `pow(corner_weight + layer_weight + height, sharpness)`, normalized across
//! every contributing layer. The raster shader that did it (`shaders/terrain_splat.wgsl`) is the
//! reference for the aurora terrain class.

use bevy::asset::RenderAssetUsages;
use bevy::image::{ImageAddressMode, ImageFilterMode, ImageSampler, ImageSamplerDescriptor};
use bevy::prelude::*;
// `resolve_with` and the splat cache read a plain PBR material for its texture handles;
// on this branch that is aurora's.
use ::aurora::material::AuroraMaterial;
use path_slash::PathExt as _;
use wgpu_types::{Extent3d, TextureDimension, TextureFormat};

mod aurora;
pub mod detail;
pub mod scatter;

pub use detail::{
    ATTRIBUTE_HEIGHT_FRACTION, BuiltDetailMesh, DETAIL_TILE_BUDGET, DETAIL_TILE_CELLS,
    DetailAssets, DetailDirty, DetailLayerSource, DetailMeshes, DetailPressers, DetailRenderPlugin,
    DetailSettings, DetailSource, DetailSystems, DetailTile, DetailViewer, MAX_DETAIL_PRESSERS,
    TerrainDetailSource, card_mesh,
};
pub use scatter::{
    GROUND_COVER_CULL_DISTANCE, GROUND_COVER_HEIGHT, ScatterAssetPlugin, ScatterAssets,
    ScatterChunk, ScatterDirty, ScatterPrimitive, ScatterRegion, ScatterRenderPlugin,
    ScatterRendered, ScatterSystems, TerrainScatter, palette_entry_bounds,
};

use crate::heightmap::Heightmap;
use crate::sidecar::{AutoTerrainSettings, SurfaceSettings, TerrainMaterialSlot};
use crate::splat::ControlTexels;
use crate::texture_set::{MAX_TEXTURES, TextureSet, TextureSetEntry, TextureSetError};

pub use crate::sidecar::DEFAULT_BLEND_SHARPNESS;

/// The loaded image handles for a [`TextureSet`], parallel to its entries.
///
/// Held per entry rather than pre-stacked because stacking needs the decoded
/// images. Every slot is `Option`: a material may be missing, or may not bind
/// that texture.
#[derive(Clone, Debug, Default)]
pub struct TextureSetImages {
    /// Albedo handle per entry, in id order; `None` where the material has
    /// no base colour texture or could not be resolved.
    pub albedo: Vec<Option<Handle<Image>>>,
    /// Normal handle per entry, `None` where the entry has none.
    pub normal: Vec<Option<Handle<Image>>>,
    /// Height handle per entry, `None` where the entry has none.
    pub height: Vec<Option<Handle<Image>>>,
    /// Occlusion handle per entry, `None` where the entry has none.
    pub occlusion: Vec<Option<Handle<Image>>>,
    /// Roughness handle per entry, `None` where the entry has none.
    pub roughness: Vec<Option<Handle<Image>>>,
}

impl TextureSetImages {
    /// Every image handle, in no particular order. What a change watcher
    /// compares an `AssetEvent` against.
    pub fn handles(&self) -> impl Iterator<Item = &Handle<Image>> {
        self.albedo
            .iter()
            .flatten()
            .chain(self.normal.iter().flatten())
            .chain(self.height.iter().flatten())
            .chain(self.occlusion.iter().flatten())
            .chain(self.roughness.iter().flatten())
    }
}

/// A terrain's material slots, resolved into what [`splat_images`] reads.
#[derive(Clone, Debug, Default)]
pub struct ResolvedSlots {
    /// One entry per slot, in id order.
    pub set: TextureSet,
    /// Image handles parallel to [`Self::set`].
    pub images: TextureSetImages,
    /// Names of slots whose material the host could not find, in slot
    /// order. Those slots still hold their ids and draw the fallback
    /// layer; this is what a panel or a log names them by.
    pub missing: Vec<String>,
}

/// Turn a terrain's material names into texture entries, with `lookup`
/// answering what material a name addresses.
///
/// Index-preserving: entry `i` is slot `i` is texture id `i`, whether the slot
/// resolved, went missing, or was vacated. The control map is addressed by these
/// indices, so compacting the list would renumber every id painted above the
/// dropped one.
///
/// The slot mapping is fixed: albedo is `base_color_texture`, normal is
/// `normal_map_texture`, height is `depth_map`, occlusion is
/// `occlusion_texture` and roughness is `metallic_roughness_texture`. A
/// material flagged `flip_normal_map_y` carries that flag through to the
/// array builder.
///
/// A slot naming an occlusion or roughness map of its own takes that image
/// instead of the material's, so a terrain can shade a shared material's
/// ground without editing what every other surface draws.
///
/// Tiling and detiling come from the slot, never from the material: one material
/// is shared across surfaces and tiles differently on each.
pub fn resolve_with<'m>(
    slots: &[TerrainMaterialSlot],
    lookup: impl Fn(&str) -> Option<&'m AuroraMaterial>,
    assets: &AssetServer,
) -> ResolvedSlots {
    let path_of = |handle: &Option<Handle<Image>>| {
        handle
            .as_ref()
            .and_then(|handle| assets.get_path(handle.id()))
            .map(|path| path.path().to_slash_lossy().into_owned())
    };

    // A map the slot names of its own, loaded here so it reaches the array
    // builder the way a material's own texture does.
    let slot_map = |named: &str| -> Option<Handle<Image>> {
        (!named.is_empty()).then(|| assets.load(named.to_string()))
    };

    let mut resolved = ResolvedSlots::default();
    for slot in slots {
        // A vacated id draws the fallback and is not reported as missing.
        if slot.is_tombstone() {
            resolved.push(TextureSetEntry::vacant(), SlotHandles::default());
            continue;
        }
        let Some(material) = lookup(&slot.material) else {
            resolved.missing.push(slot.material.clone());
            resolved.push(
                TextureSetEntry {
                    detile: slot.detile,
                    ..TextureSetEntry::unresolved(&slot.material, slot.uv_scale)
                },
                SlotHandles::default(),
            );
            continue;
        };

        let occlusion = slot_map(&slot.occlusion).or_else(|| material.occlusion_texture.clone());
        let roughness =
            slot_map(&slot.roughness).or_else(|| material.metallic_roughness_texture.clone());
        resolved.push(
            TextureSetEntry {
                material: slot.material.clone(),
                albedo: path_of(&material.base_color_texture),
                normal: path_of(&material.normal_map_texture),
                flip_normal_y: material.flip_normal_map_y,
                height: path_of(&material.depth_map),
                occlusion: path_of(&occlusion),
                roughness: path_of(&roughness),
                uv_scale: slot.uv_scale,
                detile: slot.detile,
            },
            SlotHandles {
                albedo: material.base_color_texture.clone(),
                normal: material.normal_map_texture.clone(),
                height: material.depth_map.clone(),
                occlusion,
                roughness,
            },
        );
    }
    resolved
}

/// One slot's image handles, so the parallel lists in [`TextureSetImages`]
/// can only be appended to together.
#[derive(Default)]
struct SlotHandles {
    albedo: Option<Handle<Image>>,
    normal: Option<Handle<Image>>,
    height: Option<Handle<Image>>,
    occlusion: Option<Handle<Image>>,
    roughness: Option<Handle<Image>>,
}

impl ResolvedSlots {
    /// Append one slot's entry and its handles together, so the entries and
    /// the handle lists cannot fall out of step.
    fn push(&mut self, entry: TextureSetEntry, handles: SlotHandles) {
        self.set.entries.push(entry);
        self.images.albedo.push(handles.albedo);
        self.images.normal.push(handles.normal);
        self.images.height.push(handles.height);
        self.images.occlusion.push(handles.occlusion);
        self.images.roughness.push(handles.roughness);
    }
}

/// Why a texture set cannot draw yet, or at all.
#[derive(Clone, Debug, PartialEq)]
pub enum SplatBuildError {
    /// At least one of the set's images has not finished loading. Not a
    /// failure: the caller tries again next frame.
    NotReady,
    /// The set cannot be used, and retrying will not help.
    Invalid(TextureSetError),
}

impl core::fmt::Display for SplatBuildError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::NotReady => write!(f, "texture set images are still loading"),
            Self::Invalid(reason) => write!(f, "{reason}"),
        }
    }
}

impl core::error::Error for SplatBuildError {}

/// Whether a resolved texture set can draw: valid, and every image it names decoded.
pub fn slot_images_ready(
    set: &TextureSet,
    handles: &TextureSetImages,
    images: &Assets<Image>,
) -> Result<(), SplatBuildError> {
    set.validate().map_err(SplatBuildError::Invalid)?;
    if handles.albedo.is_empty() {
        return Err(SplatBuildError::Invalid(TextureSetError::Empty));
    }
    if handles.handles().any(|handle| images.get(handle).is_none()) {
        return Err(SplatBuildError::NotReady);
    }
    Ok(())
}

/// The control map as an uploadable image: `R32Uint`, one texel per grid
/// point, never filtered.
pub fn control_image(texels: &ControlTexels) -> Image {
    control_image_from_bytes(&texels.to_bytes(), texels.resolution)
}

/// [`control_image`] from a texel buffer the caller already holds, so an editor
/// keeping the CPU-side mirror of the uploaded texture can patch the rows a
/// brush touched instead of rebuilding the buffer every frame of a stroke.
pub fn control_image_from_bytes(bytes: &[u8], resolution: u32) -> Image {
    let side = resolution.max(1);
    let mut data = bytes.to_vec();
    data.resize((side as usize) * (side as usize) * 4, 0);
    let mut image = Image::new(
        Extent3d {
            width: side,
            height: side,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        data,
        TextureFormat::R32Uint,
        RenderAssetUsages::RENDER_WORLD,
    );
    // `textureLoad` ignores the sampler, but wgpu still validates one
    // against the format, and `R32Uint` is unfilterable.
    image.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor {
        mag_filter: ImageFilterMode::Nearest,
        min_filter: ImageFilterMode::Nearest,
        mipmap_filter: ImageFilterMode::Nearest,
        ..default()
    });
    image
}

/// The colour layer as an uploadable image: `Rgba8Unorm`, one texel per grid
/// point, laid out exactly like the control map.
///
/// Not an sRGB texture, because the layer is a multiplier rather than a colour
/// to be lit: a byte the picker draws at half brightness has to arrive as a half
/// multiply.
///
/// Filtered rather than loaded, unlike the control map, so a coarse
/// hand-painted wash reads as a smooth one between the strokes. No mip chain:
/// the tint is already the low-frequency half of the ground's look.
///
/// A short buffer pads with white, which is the identity.
pub fn tint_image_from_bytes(bytes: &[u8], resolution: u32) -> Image {
    let side = resolution.max(1);
    let mut data = bytes.to_vec();
    data.resize((side as usize) * (side as usize) * 4, u8::MAX);
    let mut image = Image::new(
        Extent3d {
            width: side,
            height: side,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        data,
        TextureFormat::Rgba8Unorm,
        RenderAssetUsages::RENDER_WORLD,
    );
    image.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor {
        mag_filter: ImageFilterMode::Linear,
        min_filter: ImageFilterMode::Linear,
        mipmap_filter: ImageFilterMode::Nearest,
        address_mode_u: ImageAddressMode::ClampToEdge,
        address_mode_v: ImageAddressMode::ClampToEdge,
        ..default()
    });
    image
}

/// UV0 remapped onto the tint layer's texel centres.
///
/// The splat shader computes this inline; both spell the same mapping. UV0
/// places control grid point `i` at `i/(res-1)` while a sampled texel `i` is
/// centred at `(i+0.5)/res`, and the error reverses sign across the terrain, so
/// no constant bias corrects it.
#[must_use]
pub fn tint_uv(uv: f32, resolution: u32) -> f32 {
    let res = resolution.max(2) as f32;
    (uv * (res - 1.0) + 0.5) / res
}

/// [`tint_image_from_bytes`] from a dense colour layer.
///
/// A layer shorter than `resolution^2` pads with white, so an unpainted
/// terrain uploads no tint rather than failing.
pub fn tint_image(colors: &[[u8; 4]], resolution: u32) -> Image {
    let side = resolution.max(1);
    let cells = (side as usize) * (side as usize);
    let mut bytes = Vec::with_capacity(cells * 4);
    for texel in colors.iter().take(cells) {
        bytes.extend_from_slice(texel);
    }
    tint_image_from_bytes(&bytes, side)
}

/// A heightmap's slope field as an uploadable image: `R32Float`, one texel per
/// grid point, laid out exactly like the control map so a fragment reads both
/// with one grid coordinate.
///
/// The shader shades autoterrain from this rather than from the surface it is
/// drawing: the surface is a clipmap, so a slope read off the geometry changes
/// as a level hands over. These values are the same from every distance.
pub fn slope_image(map: &Heightmap) -> Image {
    let side = map.resolution.max(1);
    let slopes = map.slope_field();
    let mut data = Vec::with_capacity((side as usize) * (side as usize) * 4);
    for texel in &slopes {
        data.extend_from_slice(&texel.to_le_bytes());
    }
    data.resize((side as usize) * (side as usize) * 4, 0);
    let mut image = Image::new(
        Extent3d {
            width: side,
            height: side,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        data,
        TextureFormat::R32Float,
        RenderAssetUsages::RENDER_WORLD,
    );
    // `textureLoad` ignores the sampler, but wgpu still validates one
    // against the format, and `R32Float` is unfilterable.
    image.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor {
        mag_filter: ImageFilterMode::Nearest,
        min_filter: ImageFilterMode::Nearest,
        mipmap_filter: ImageFilterMode::Nearest,
        ..default()
    });
    image
}

/// Terrain shaded from a control map against a texture set.
///
/// One material per terrain: the control map and terrain size live here rather
/// than per chunk, so every chunk of a terrain shares one material and one
/// upload.
#[derive(Asset, TypePath, Clone, Debug)]
pub struct TerrainSplatMaterial {
    /// Per-id UV scales, four to a `Vec4`. The shader unpacks by id.
    pub uv_scales: [Vec4; MAX_TEXTURES / 4],
    /// Per-id detiling strengths, packed the same way. 0 leaves a layer
    /// sampled exactly where its UV scale puts it.
    pub detile_strengths: [Vec4; MAX_TEXTURES / 4],
    /// Terrain XZ extent in world units.
    pub terrain_size: Vec2,
    /// Local XZ of control grid point `(0, 0)`: where the extent starts.
    pub terrain_origin: Vec2,
    /// `0..1`, remapped by the shader onto a `4..64` power exponent. Low is
    /// a soft cross-fade, high a near-binary height cutout.
    pub blend_sharpness: f32,
    pub perceptual_roughness: f32,
    /// Grid points per terrain edge; the control map's side length.
    pub control_resolution: u32,
    /// Texture ids the bound set defines. Ids past it clamp to the last.
    pub layer_count: u32,
    /// Nonzero where this terrain textures the cells no hand has claimed
    /// from their slope. 0 leaves every cell to its own control word.
    pub autoterrain_enabled: u32,
    /// Texture id flat ground draws where autoterrain is on.
    pub autoterrain_base_slot: u32,
    /// Texture id steep ground draws where autoterrain is on.
    pub autoterrain_slope_slot: u32,
    /// Slope at which the base texture starts giving way, in radians.
    /// Converted from the authored degrees here so the shader carries no
    /// conversion of its own.
    pub autoterrain_slope_start: f32,
    /// Slope at which the slope texture has fully taken over, in radians.
    pub autoterrain_slope_end: f32,
    /// How much of the tint texture reaches the finished albedo, `0..1`. 0 draws
    /// the textures untinted.
    pub tint_strength: f32,
    /// Bit `i` is set where texture id `i` has an occlusion map. An id
    /// without one shades unoccluded.
    pub occlusion_slots: u32,
    /// Bit `i` is set where texture id `i` has a roughness map. An id
    /// without one takes [`Self::perceptual_roughness`].
    pub roughness_slots: u32,
    /// Bit `i` is set where texture id `i`'s normal map is authored green-down.
    pub flip_normal_slots: u32,
    /// Each id's own maps, sampled where they are: the tracer reads textures
    /// bindless, so nothing is stacked into arrays.
    pub slots: TextureSetImages,
    pub control: Handle<Image>,
    /// Slope per grid point, in radians. Only autoterrain reads it.
    pub slope: Handle<Image>,
    /// The colour layer the finished albedo is multiplied by.
    pub tint: Handle<Image>,
}

impl TerrainSplatMaterial {
    /// Build a material for a terrain from its set, its maps, its
    /// control map and how it textures the cells no hand has claimed.
    pub fn new(
        set: &TextureSet,
        slots: TextureSetImages,
        control: Handle<Image>,
        slope: Handle<Image>,
        tint: Handle<Image>,
        terrain_size: Vec2,
        control_resolution: u32,
        autoterrain: AutoTerrainSettings,
        surface: SurfaceSettings,
    ) -> Self {
        let scales = set.uv_scales();
        let mut uv_scales = [Vec4::ZERO; MAX_TEXTURES / 4];
        for (i, scale) in scales.iter().enumerate() {
            uv_scales[i / 4][i % 4] = *scale;
        }
        let mut detile_strengths = [Vec4::ZERO; MAX_TEXTURES / 4];
        for (i, strength) in set.detile_strengths().iter().enumerate() {
            detile_strengths[i / 4][i % 4] = *strength;
        }
        let mut material = Self {
            uv_scales,
            detile_strengths,
            terrain_size,
            terrain_origin: Vec2::ZERO,
            blend_sharpness: DEFAULT_BLEND_SHARPNESS,
            perceptual_roughness: 0.9,
            control_resolution,
            layer_count: set.len().max(1) as u32,
            autoterrain_enabled: 0,
            autoterrain_base_slot: 0,
            autoterrain_slope_slot: 0,
            autoterrain_slope_start: 0.0,
            autoterrain_slope_end: 0.0,
            tint_strength: crate::sidecar::DEFAULT_TINT_STRENGTH,
            occlusion_slots: slot_mask(set, |entry| entry.occlusion.is_some()),
            roughness_slots: slot_mask(set, |entry| entry.roughness.is_some()),
            flip_normal_slots: slot_mask(set, |entry| entry.flip_normal_y),
            slots,
            control,
            slope,
            tint,
        };
        material.set_autoterrain(autoterrain);
        material.set_surface(surface);
        material
    }

    /// Point the autoterrain half of the uniform at `settings`.
    ///
    /// Separate from [`Self::new`] because changing these is a uniform write:
    /// the control map, the arrays and the mesh all stand.
    ///
    /// The settings are sanitized on the way in: they reach a `smoothstep` the
    /// fragment cannot guard.
    pub fn set_autoterrain(&mut self, settings: AutoTerrainSettings) {
        let settings = settings.sanitized();
        self.autoterrain_enabled = u32::from(settings.enabled);
        self.autoterrain_base_slot = settings.base_slot as u32;
        self.autoterrain_slope_slot = settings.slope_slot as u32;
        self.autoterrain_slope_start = settings.slope_start_deg.to_radians();
        self.autoterrain_slope_end = settings.slope_end_deg.to_radians();
    }

    /// Point the surface half of the uniform at `settings`.
    ///
    /// A uniform write, like [`Self::set_autoterrain`], and sanitized on the way
    /// in for the same reason: a NaN reaches the fragment's `pow` and its `mix`
    /// unguarded.
    pub fn set_surface(&mut self, settings: SurfaceSettings) {
        let settings = settings.sanitized();
        self.blend_sharpness = settings.blend_sharpness;
        self.tint_strength = settings.tint_strength;
    }
}

/// Which texture ids answer `has_map`, as the bit per id the shader tests.
///
/// Ids past [`MAX_TEXTURES`] cannot be addressed by a control word, so a set
/// longer than the id space contributes nothing past it.
fn slot_mask(set: &TextureSet, has_map: impl Fn(&TextureSetEntry) -> bool) -> u32 {
    let mut mask = 0u32;
    for (id, entry) in set.entries.iter().take(MAX_TEXTURES).enumerate() {
        if has_map(entry) {
            mask |= 1 << id;
        }
    }
    mask
}

/// The splat material a terrain chunk wears.
#[derive(Component, Clone, Debug, Default, Reflect, PartialEq, Eq)]
#[reflect(Component, Default, Clone, PartialEq)]
pub struct TerrainSplat3d(pub Handle<TerrainSplatMaterial>);

/// Registers the splat material asset and the surface class that traces it.
pub struct TerrainRenderPlugin;

impl Plugin for TerrainRenderPlugin {
    fn build(&self, app: &mut App) {
        if !app
            .world()
            .contains_resource::<Assets<TerrainSplatMaterial>>()
        {
            app.init_asset::<TerrainSplatMaterial>();
        }
        app.register_type::<TerrainSplat3d>()
            .add_plugins(aurora::plugin);
    }
}

/// Resolution: turning material names into entries, through a lookup
/// closure standing in for the host's name store.
#[cfg(test)]
mod resolve_tests {
    use std::collections::HashMap;

    use bevy::app::App;
    use bevy::asset::{AssetPlugin, AssetServer, Assets};
    use bevy::prelude::*;
    use ::aurora::material::AuroraMaterial;

    use super::{ResolvedSlots, resolve_with};
    use crate::sidecar::TerrainMaterialSlot;

    /// A host's name store, minus everything resolution does not use.
    #[derive(Resource, Default)]
    struct Saved(HashMap<String, Handle<AuroraMaterial>>);

    fn resolve_app() -> App {
        let mut app = App::new();
        app.add_plugins((bevy::app::TaskPoolPlugin::default(), AssetPlugin::default()));
        app.init_asset::<Image>();
        app.init_asset::<AuroraMaterial>();
        app.init_resource::<Saved>();
        app
    }

    /// Save a material under `name` with the three slots a terrain reads.
    fn saved_material(app: &mut App, name: &str, flip_y: bool) {
        let server = app.world().resource::<AssetServer>().clone();
        let base = server.load::<Image>(format!("t/{name}_base.png"));
        let normal = server.load::<Image>(format!("t/{name}_normal.png"));
        let depth = server.load::<Image>(format!("t/{name}_height.png"));
        let handle = app
            .world_mut()
            .resource_mut::<Assets<AuroraMaterial>>()
            .add(AuroraMaterial {
                base_color_texture: Some(base),
                normal_map_texture: Some(normal),
                depth_map: Some(depth),
                flip_normal_map_y: flip_y,
                ..default()
            });
        app.world_mut()
            .resource_mut::<Saved>()
            .0
            .insert(name.to_string(), handle);
    }

    /// Save a material under `name` carrying the two maps a slot shades
    /// its occlusion and roughness from.
    fn saved_shaded_material(app: &mut App, name: &str) {
        let server = app.world().resource::<AssetServer>().clone();
        let occlusion = server.load::<Image>(format!("t/{name}_ao.png"));
        let roughness = server.load::<Image>(format!("t/{name}_orm.png"));
        let handle = app
            .world_mut()
            .resource_mut::<Assets<AuroraMaterial>>()
            .add(AuroraMaterial {
                occlusion_texture: Some(occlusion),
                metallic_roughness_texture: Some(roughness),
                ..default()
            });
        app.world_mut()
            .resource_mut::<Saved>()
            .0
            .insert(name.to_string(), handle);
    }

    fn resolve_slots(app: &App, slots: &[TerrainMaterialSlot]) -> ResolvedSlots {
        let saved = app.world().resource::<Saved>();
        let materials = app.world().resource::<Assets<AuroraMaterial>>();
        resolve_with(
            slots,
            |name| saved.0.get(name).and_then(|handle| materials.get(handle)),
            app.world().resource::<AssetServer>(),
        )
    }

    /// Base colour is albedo, the normal map is the normal, and the depth
    /// map is the height.
    #[test]
    fn a_materials_three_slots_become_albedo_normal_and_height() {
        let mut app = resolve_app();
        saved_material(&mut app, "grass", false);

        let resolved = resolve_slots(
            &app,
            &[TerrainMaterialSlot {
                material: "grass".to_string(),
                uv_scale: 0.3,
                detile: 0.6,
                occlusion: String::new(),
                roughness: String::new(),
            }],
        );

        assert!(resolved.missing.is_empty());
        let entry = &resolved.set.entries[0];
        assert_eq!(entry.material, "grass");
        assert_eq!(entry.albedo.as_deref(), Some("t/grass_base.png"));
        assert_eq!(entry.normal.as_deref(), Some("t/grass_normal.png"));
        assert_eq!(entry.height.as_deref(), Some("t/grass_height.png"));
        assert_eq!(entry.uv_scale, 0.3, "tiling comes from the slot");
        assert_eq!(entry.detile, 0.6, "so does detiling");
        assert!(!entry.flip_normal_y);
        assert!(resolved.images.albedo[0].is_some());
        assert!(resolved.images.normal[0].is_some());
        assert!(resolved.images.height[0].is_some());
        assert_eq!(resolved.set.validate(), Ok(()));
    }

    /// A material whose normal map has green pointing down carries that
    /// flag to the array builder, the only place that can flip it before
    /// the shader samples it.
    #[test]
    fn a_materials_normal_convention_flag_reaches_the_texture_entry() {
        let mut app = resolve_app();
        saved_material(&mut app, "rock", true);

        let resolved = resolve_slots(&app, &[TerrainMaterialSlot::new("rock")]);

        assert!(resolved.set.entries[0].flip_normal_y);
    }

    /// A terrain referencing a material with no file keeps the slot:
    /// dropping it would renumber every id painted after it. The name is
    /// reported so the host can name the one that went.
    #[test]
    fn a_missing_material_keeps_its_slot_and_is_named() {
        let mut app = resolve_app();
        saved_material(&mut app, "grass", false);

        let resolved = resolve_slots(
            &app,
            &[
                TerrainMaterialSlot::new("grass"),
                TerrainMaterialSlot::new("deleted"),
                TerrainMaterialSlot::new("grass2"),
            ],
        );

        assert_eq!(resolved.set.entries.len(), 3, "ids keep their positions");
        assert_eq!(resolved.set.entries[1].material, "deleted");
        assert_eq!(resolved.set.entries[1].albedo, None);
        assert_eq!(resolved.images.albedo[1], None);
        assert_eq!(resolved.missing, vec!["deleted", "grass2"]);
    }

    /// A vacated id keeps its place and draws the fallback, and is not
    /// reported as missing.
    #[test]
    fn a_vacated_slot_holds_its_id_without_being_called_missing() {
        let mut app = resolve_app();
        saved_material(&mut app, "grass", false);

        let resolved = resolve_slots(
            &app,
            &[
                TerrainMaterialSlot::new("grass"),
                TerrainMaterialSlot::tombstone(),
                TerrainMaterialSlot::new("grass"),
            ],
        );

        assert_eq!(resolved.set.entries.len(), 3);
        assert!(resolved.set.entries[1].is_vacant());
        assert_eq!(resolved.images.albedo[1], None);
        assert!(resolved.missing.is_empty());
    }

    /// A material with no textures at all is not an error: it resolves to
    /// a slot that draws the fallback, the same as a missing one, but is
    /// not reported as missing because its name still resolves.
    #[test]
    fn a_material_with_no_textures_resolves_without_being_called_missing() {
        let mut app = resolve_app();
        let handle = app
            .world_mut()
            .resource_mut::<Assets<AuroraMaterial>>()
            .add(AuroraMaterial::default());
        app.world_mut()
            .resource_mut::<Saved>()
            .0
            .insert("plain".to_string(), handle);

        let resolved = resolve_slots(&app, &[TerrainMaterialSlot::new("plain")]);

        assert!(resolved.missing.is_empty());
        assert_eq!(resolved.set.entries[0].albedo, None);
        assert_eq!(resolved.set.validate(), Ok(()));
    }

    /// The entries and the three handle lists are one row per slot, so
    /// the array builder can read them by index.
    #[test]
    fn every_slot_contributes_one_entry_and_one_handle_row() {
        let mut app = resolve_app();
        saved_material(&mut app, "grass", false);
        let slots = [
            TerrainMaterialSlot::new("grass"),
            TerrainMaterialSlot::tombstone(),
            TerrainMaterialSlot::new("gone"),
        ];

        let resolved = resolve_slots(&app, &slots);

        assert_eq!(resolved.set.entries.len(), slots.len());
        assert_eq!(resolved.images.albedo.len(), slots.len());
        assert_eq!(resolved.images.normal.len(), slots.len());
        assert_eq!(resolved.images.height.len(), slots.len());
        assert_eq!(resolved.images.occlusion.len(), slots.len());
        assert_eq!(resolved.images.roughness.len(), slots.len());
    }

    /// Occlusion is the material's occlusion map and roughness is its
    /// metallic-roughness map, so a ground texture set that ships both
    /// shades with them without anything new to author.
    #[test]
    fn a_slot_whose_material_has_an_occlusion_map_binds_it() {
        let mut app = resolve_app();
        saved_shaded_material(&mut app, "gravel");

        let resolved = resolve_slots(&app, &[TerrainMaterialSlot::new("gravel")]);

        let entry = &resolved.set.entries[0];
        assert_eq!(entry.occlusion.as_deref(), Some("t/gravel_ao.png"));
        assert_eq!(entry.roughness.as_deref(), Some("t/gravel_orm.png"));
        assert!(resolved.images.occlusion[0].is_some());
        assert!(resolved.images.roughness[0].is_some());
    }

    /// A material with neither map leaves the slot with none, so the
    /// shader shades it unoccluded at the terrain's own roughness.
    #[test]
    fn a_slot_whose_material_has_no_maps_binds_none() {
        let mut app = resolve_app();
        saved_material(&mut app, "grass", false);

        let resolved = resolve_slots(&app, &[TerrainMaterialSlot::new("grass")]);

        assert_eq!(resolved.set.entries[0].occlusion, None);
        assert_eq!(resolved.set.entries[0].roughness, None);
        assert_eq!(resolved.images.occlusion[0], None);
        assert_eq!(resolved.images.roughness[0], None);
    }

    /// A map named on the slot wins over the one its material carries: one
    /// material is shared across surfaces, and a terrain may shade its own
    /// ground without editing what the others draw.
    #[test]
    fn a_map_named_on_the_slot_wins_over_the_materials() {
        let mut app = resolve_app();
        saved_shaded_material(&mut app, "gravel");

        let resolved = resolve_slots(
            &app,
            &[TerrainMaterialSlot {
                occlusion: "ground/path_ao.png".to_string(),
                ..TerrainMaterialSlot::new("gravel")
            }],
        );

        let entry = &resolved.set.entries[0];
        assert_eq!(entry.occlusion.as_deref(), Some("ground/path_ao.png"));
        assert_eq!(
            entry.roughness.as_deref(),
            Some("t/gravel_orm.png"),
            "the slot named no roughness, so the material's still stands"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control::Control;
    use crate::texture_set::TextureSetEntry;

    fn solid(width: u32, height: u32, texel: [u8; 4], format: TextureFormat) -> Image {
        let data: Vec<u8> = texel
            .iter()
            .copied()
            .cycle()
            .take((width as usize) * (height as usize) * 4)
            .collect();
        Image::new(
            Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            TextureDimension::D2,
            data,
            format,
            RenderAssetUsages::RENDER_WORLD,
        )
    }

    /// A set of solid-colour entries, all loaded, one per requested size.
    fn loaded_set(sizes: &[(u32, u32)]) -> (Assets<Image>, TextureSet, TextureSetImages) {
        let mut images = Assets::<Image>::default();
        let mut albedo = Vec::new();
        let mut entries = Vec::new();
        for (i, (w, h)) in sizes.iter().enumerate() {
            albedo.push(Some(images.add(solid(
                *w,
                *h,
                [10, 20, 30, 255],
                TextureFormat::Rgba8UnormSrgb,
            ))));
            entries.push(TextureSetEntry::new(format!("m{i}"), format!("t{i}.png")));
        }
        let count = sizes.len();
        (
            images,
            TextureSet { entries },
            TextureSetImages {
                albedo,
                normal: vec![None; count],
                height: vec![None; count],
                occlusion: vec![None; count],
                roughness: vec![None; count],
            },
        )
    }

    #[test]
    fn handles_with_no_layers_at_all_are_refused() {
        let images = Assets::<Image>::default();
        let set = TextureSet {
            entries: vec![TextureSetEntry::new("grass", "a.png")],
        };
        let err = slot_images_ready(&set, &TextureSetImages::default(), &images)
            .expect_err("handles that name no layers are refused");
        assert_eq!(err, SplatBuildError::Invalid(TextureSetError::Empty));
    }

    #[test]
    fn an_image_that_has_not_decoded_yet_reports_not_ready_rather_than_failing() {
        let (_, set, handles) = loaded_set(&[(4, 4)]);
        let empty = Assets::<Image>::default();
        assert_eq!(
            slot_images_ready(&set, &handles, &empty),
            Err(SplatBuildError::NotReady)
        );
    }

    #[test]
    fn a_decoded_set_is_ready() {
        let (images, set, handles) = loaded_set(&[(4, 4), (8, 8)]);
        assert_eq!(slot_images_ready(&set, &handles, &images), Ok(()));
    }

    #[test]
    fn an_unusable_set_reports_invalid_rather_than_not_ready() {
        let (images, mut set, handles) = loaded_set(&[(4, 4)]);
        set.entries[0].uv_scale = 0.0;
        assert!(matches!(
            slot_images_ready(&set, &handles, &images),
            Err(SplatBuildError::Invalid(TextureSetError::BadUvScale { .. }))
        ));
    }

    #[test]
    fn the_control_image_is_unfiltered_r32uint_at_the_terrain_resolution() {
        let control = vec![Control::default().with_base_id(3); 16];
        let texels = ControlTexels::from_control(&control, 4);
        let image = control_image(&texels);
        assert_eq!(image.texture_descriptor.format, TextureFormat::R32Uint);
        assert_eq!(image.texture_descriptor.size.width, 4);
        assert_eq!(image.texture_descriptor.size.height, 4);
        assert_eq!(image.data.as_ref().unwrap().len(), 4 * 4 * 4);
        assert_eq!(&image.data.as_ref().unwrap()[0..4], &3u32.to_le_bytes());
    }

    #[test]
    fn a_zero_resolution_terrain_still_produces_a_bindable_control_image() {
        let image = control_image(&ControlTexels::from_control(&[], 0));
        assert_eq!(image.texture_descriptor.size.width, 1);
        assert_eq!(image.data.as_ref().unwrap().len(), 4);
    }

    fn test_material(autoterrain: AutoTerrainSettings) -> TerrainSplatMaterial {
        TerrainSplatMaterial::new(
            &TextureSet {
                entries: vec![TextureSetEntry::new("grass", "a.png")],
            },
            TextureSetImages::default(),
            Handle::default(),
            Handle::default(),
            Handle::default(),
            Vec2::splat(100.0),
            256,
            autoterrain,
            SurfaceSettings::default(),
        )
    }

    /// Off is the default, and off means the uniform says nothing the
    /// shader's guard can act on.
    #[test]
    fn a_terrain_that_never_asked_for_autoterrain_uploads_it_disabled() {
        let material = test_material(AutoTerrainSettings::default());
        assert_eq!(material.autoterrain_enabled, 0);
    }

    /// The angles are authored in degrees and sampled in radians, so the
    /// conversion happens once, here.
    #[test]
    fn autoterrain_slots_and_angles_reach_the_uniform_in_radians() {
        let material = test_material(AutoTerrainSettings {
            enabled: true,
            base_slot: 1,
            slope_slot: 3,
            slope_start_deg: 30.0,
            slope_end_deg: 60.0,
        });

        assert_eq!(material.autoterrain_enabled, 1);
        assert_eq!(material.autoterrain_base_slot, 1);
        assert_eq!(material.autoterrain_slope_slot, 3);
        assert!((material.autoterrain_slope_start - core::f32::consts::FRAC_PI_6).abs() < 1e-6);
        assert!((material.autoterrain_slope_end - core::f32::consts::FRAC_PI_3).abs() < 1e-6);
    }

    /// Settings that never went through a sidecar decode still cannot
    /// reach the shader's `smoothstep` as a NaN or a backwards range.
    #[test]
    fn unsanitized_autoterrain_settings_are_pulled_back_before_they_are_uploaded() {
        let material = test_material(AutoTerrainSettings {
            enabled: true,
            base_slot: 0,
            slope_slot: 1,
            slope_start_deg: 80.0,
            slope_end_deg: f32::NAN,
        });

        assert!(material.autoterrain_slope_start.is_finite());
        assert!(material.autoterrain_slope_end.is_finite());
        assert!(
            material.autoterrain_slope_start <= material.autoterrain_slope_end,
            "the range must reach the shader the right way round",
        );
    }

    /// Changing the settings is a uniform write: nothing else about the
    /// material moves, so an editor moving a slider re-uploads one
    /// buffer rather than rebuilding the terrain.
    #[test]
    fn setting_autoterrain_again_touches_nothing_but_its_own_fields() {
        let mut material = test_material(AutoTerrainSettings::default());
        let before = material.clone();

        material.set_autoterrain(AutoTerrainSettings {
            enabled: true,
            base_slot: 2,
            slope_slot: 4,
            slope_start_deg: 10.0,
            slope_end_deg: 20.0,
        });

        assert_eq!(material.autoterrain_enabled, 1);
        assert_eq!(material.uv_scales, before.uv_scales);
        assert_eq!(material.detile_strengths, before.detile_strengths);
        assert_eq!(material.control_resolution, before.control_resolution);
        assert_eq!(material.layer_count, before.layer_count);
        assert_eq!(material.terrain_size, before.terrain_size);
        assert_eq!(material.control, before.control);
    }

    #[test]
    fn uv_scales_reach_the_uniform_in_id_order() {
        let set = TextureSet {
            entries: vec![
                TextureSetEntry {
                    uv_scale: 0.25,
                    ..TextureSetEntry::new("grass", "a.png")
                },
                TextureSetEntry {
                    uv_scale: 4.0,
                    ..TextureSetEntry::new("rock", "b.png")
                },
            ],
        };
        let material = TerrainSplatMaterial::new(
            &set,
            TextureSetImages::default(),
            Handle::default(),
            Handle::default(),
            Handle::default(),
            Vec2::splat(100.0),
            256,
            AutoTerrainSettings::default(),
            SurfaceSettings::default(),
        );
        assert_eq!(material.uv_scales[0].x, 0.25);
        assert_eq!(material.uv_scales[0].y, 4.0);
        assert_eq!(material.layer_count, 2);
        assert_eq!(material.control_resolution, 256);
        assert_eq!(material.terrain_size, Vec2::splat(100.0));
    }

    /// The shader reads a slot's maps only where the uniform's bit for
    /// that id says it has them, so an id with no map shades unoccluded at
    /// the terrain's own roughness rather than sampling a filled layer.
    #[test]
    fn only_the_slots_with_maps_are_flagged_in_the_uniform() {
        let set = TextureSet {
            entries: vec![
                TextureSetEntry::new("grass", "a.png"),
                TextureSetEntry {
                    occlusion: Some("b_ao.png".to_string()),
                    ..TextureSetEntry::new("rock", "b.png")
                },
                TextureSetEntry {
                    roughness: Some("c_orm.png".to_string()),
                    ..TextureSetEntry::new("sand", "c.png")
                },
            ],
        };
        let material = TerrainSplatMaterial::new(
            &set,
            TextureSetImages::default(),
            Handle::default(),
            Handle::default(),
            Handle::default(),
            Vec2::splat(100.0),
            256,
            AutoTerrainSettings::default(),
            SurfaceSettings::default(),
        );

        assert_eq!(material.occlusion_slots, 0b010);
        assert_eq!(material.roughness_slots, 0b100);
    }

    /// A NaN reaches the fragment's `pow` and its `mix` unguarded, so the
    /// uniform write clamps rather than trusting its caller.
    #[test]
    fn the_surface_uniform_is_sanitized_on_the_way_in() {
        let material = TerrainSplatMaterial::new(
            &TextureSet::default(),
            TextureSetImages::default(),
            Handle::default(),
            Handle::default(),
            Handle::default(),
            Vec2::splat(100.0),
            256,
            AutoTerrainSettings::default(),
            SurfaceSettings {
                blend_sharpness: f32::NAN,
                tint_strength: 9.0,
            },
        );
        assert_eq!(material.blend_sharpness, DEFAULT_BLEND_SHARPNESS);
        assert_eq!(material.tint_strength, 1.0);
    }
}
