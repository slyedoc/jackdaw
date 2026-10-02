//! The layered material on aurora: one surface class, one parameter buffer.
//!
//! A mesh wears [`LayeredSurface3d`]. The tracer never sees that asset -- it reads
//! `AuroraMaterial3d` and a `SurfaceClass` -- so this module keeps a mirror `AuroraMaterial`
//! per layered material and tags the entity with the class, which routes its SBT record to
//! `layered.rchit`.

use ash::vk;
use bevy::platform::collections::HashMap;
use bevy::prelude::*;
use bevy_aurora::{
    assets::aurora_asset,
    material::{AuroraMaterial, AuroraMaterial3d},
    render_buffer::{Buffer, BufferProvider},
    render_device::RenderDevice,
    surface_group::{
        LayeredParams, SurfaceClass, SurfaceGroup, SurfaceGroupData, SurfaceGroupRegistry,
    },
    vulkan_asset::VulkanAssets,
};

use crate::LayeredSurfaceMaterial;

/// The layered material a mesh wears, in place of `MeshMaterial3d<LayeredSurfaceMaterial>`.
#[derive(Component, Clone, Debug, Default, Reflect, PartialEq, Eq)]
#[reflect(Component, Default, Clone, PartialEq)]
pub struct LayeredSurface3d(pub Handle<LayeredSurfaceMaterial>);

/// Rows are handed out on first sight and never recycled: a row is baked into a material
/// record, and reusing one would repoint a live surface at another's parameters.
#[derive(Resource)]
struct LayeredBuffer {
    buffer: Buffer<LayeredParams>,
    rows: HashMap<AssetId<LayeredSurfaceMaterial>, u32>,
    /// Mirror `AuroraMaterial` per layered material: what the tracer actually shades with.
    mirrors: HashMap<AssetId<LayeredSurfaceMaterial>, Handle<AuroraMaterial>>,
    class: SurfaceClass,
}

const INITIAL_ROWS: u64 = 256;

fn register_class(
    mut commands: Commands,
    mut registry: ResMut<SurfaceGroupRegistry>,
    mut data: ResMut<SurfaceGroupData>,
    render_device: Res<RenderDevice>,
    asset_server: Res<AssetServer>,
) {
    let class = registry.register(SurfaceGroup {
        label: "jackdaw_surface::layered".to_string(),
        closest_hit: asset_server.load(aurora_asset("shaders/layered.rchit")),
        any_hit: None,
    });
    let buffer: Buffer<LayeredParams> = render_device.create_host_buffer(
        INITIAL_ROWS,
        vk::BufferUsageFlags::STORAGE_BUFFER | vk::BufferUsageFlags::SHADER_DEVICE_ADDRESS,
    );
    data.set(class, buffer.address);
    commands.insert_resource(LayeredBuffer {
        buffer,
        rows: HashMap::default(),
        mirrors: HashMap::default(),
        class,
    });
}

/// Keeps every layered material's mirror and parameter row current.
fn sync_materials(
    mut state: ResMut<LayeredBuffer>,
    mut layered: ResMut<Assets<LayeredSurfaceMaterial>>,
    mut materials: ResMut<Assets<AuroraMaterial>>,
    mut data: ResMut<SurfaceGroupData>,
    render_device: Res<RenderDevice>,
    textures: Res<VulkanAssets<Image>>,
    mut events: MessageReader<AssetEvent<LayeredSurfaceMaterial>>,
) {
    let mut touched: Vec<AssetId<LayeredSurfaceMaterial>> = Vec::new();
    for event in events.read() {
        match event {
            AssetEvent::Added { id }
            | AssetEvent::Modified { id }
            | AssetEvent::LoadedWithDependencies { id } => touched.push(*id),
            AssetEvent::Removed { id } | AssetEvent::Unused { id } => {
                if let Some(handle) = state.mirrors.remove(id) {
                    data.clear_row(handle.id());
                }
            }
        }
    }
    touched.sort_unstable();
    touched.dedup();

    for id in touched {
        let Some(material) = layered.get(id) else {
            continue;
        };
        let (base, extension) = (material.base.clone(), material.extension.clone());

        let next_row = state.rows.len() as u32;
        let row = *state.rows.entry(id).or_insert(next_row);
        if u64::from(row) >= state.buffer.nr_elements {
            error!("layered surfaces exceeded {INITIAL_ROWS} materials; {id:?} not drawn");
            continue;
        }

        // The mirror carries the base surface. Rewritten rather than replaced, so the
        // handle -- and with it the row published in `SurfaceGroupData` -- stays put.
        let mirror = match state.mirrors.get(&id) {
            Some(handle) => {
                if let Some(mut existing) = materials.get_mut(handle) {
                    *existing = base;
                }
                handle.clone()
            }
            None => {
                let handle = materials.add(base);
                state.mirrors.insert(id, handle.clone());
                handle
            }
        };
        data.set_row(mirror.id(), row);

        // Bindless slots for the six triplanar maps. A map still loading resolves to white
        // this frame; the asset event when it lands brings us back through here.
        let slot = |handle: &Option<Handle<Image>>| -> u32 {
            handle
                .as_ref()
                .and_then(|h| textures.get_by_id(h.id()))
                .map_or(bevy_aurora::render_env::WHITE_TEXTURE_IDX, |texture| {
                    render_device.register_bindless_texture(texture)
                })
        };
        let params = LayeredParams {
            layer_base_color_texture: slot(&extension.layer_base_color_texture),
            layer_normal_map_texture: slot(&extension.layer_normal_map_texture),
            layer_orm_texture: slot(&extension.layer_orm_texture),
            detail_base_color_texture: slot(&extension.detail_base_color_texture),
            detail_normal_map_texture: slot(&extension.detail_normal_map_texture),
            detail_orm_texture: slot(&extension.detail_orm_texture),
            ..extension.params()
        };
        let mut mapped = render_device.map_buffer(&mut state.buffer);
        mapped[row as usize] = params;
    }
    // `get_mut` above flags the mirror modified, which is what re-resolves the material
    // record; nothing else in this crate writes them.
    let _ = &mut layered;
}

/// Puts the mirror and the class on every entity wearing a layered material.
fn sync_entities(
    mut commands: Commands,
    state: Res<LayeredBuffer>,
    worn: Query<(Entity, &LayeredSurface3d), Changed<LayeredSurface3d>>,
) {
    for (entity, layered) in &worn {
        let Some(mirror) = state.mirrors.get(&layered.0.id()) else {
            continue;
        };
        commands
            .entity(entity)
            .insert((AuroraMaterial3d(mirror.clone()), state.class));
    }
}

/// Registers the layered material, its surface class and its reflected types, so a scene
/// naming one renders it.
pub struct LayeredSurfacePlugin;

impl Plugin for LayeredSurfacePlugin {
    fn build(&self, app: &mut App) {
        app.init_asset::<LayeredSurfaceMaterial>()
            .register_type::<crate::LayeredSurface>()
            .register_type::<crate::LayerBlend>()
            .register_type::<crate::VertexColorChannel>()
            .register_type::<LayeredSurface3d>()
            .register_asset_reflect::<LayeredSurfaceMaterial>()
            .register_type_data::<LayeredSurfaceMaterial, bevy::reflect::std_traits::ReflectDefault>()
            // The class needs aurora's render side. A headless app -- the editor's tests,
            // an asset-only tool -- has the materials but no device to publish a buffer to.
            .add_systems(
                Startup,
                register_class
                    .run_if(resource_exists::<SurfaceGroupRegistry>)
                    .run_if(resource_exists::<RenderDevice>),
            )
            .add_systems(
                Update,
                (sync_materials, sync_entities)
                    .chain()
                    .run_if(resource_exists::<LayeredBuffer>),
            );
    }
}

/// A material that is a base PBR surface plus its own shading extension. The mirror machinery
/// only needs the base; the extension is whatever the class's shader reads.
pub trait ExtendedSurface: Asset {
    fn base(&self) -> &AuroraMaterial;
}

impl ExtendedSurface for LayeredSurfaceMaterial {
    fn base(&self) -> &AuroraMaterial {
        &self.base
    }
}

/// Mirror `AuroraMaterial`s for one extended material type. Rewritten in place rather than
/// replaced, so a handle -- and any row published against it -- stays put.
#[derive(Resource)]
pub struct Mirrors<M: ExtendedSurface> {
    map: HashMap<AssetId<M>, Handle<AuroraMaterial>>,
}

impl<M: ExtendedSurface> Default for Mirrors<M> {
    fn default() -> Self {
        Self {
            map: HashMap::default(),
        }
    }
}

impl<M: ExtendedSurface> Mirrors<M> {
    pub fn get(&self, id: AssetId<M>) -> Option<&Handle<AuroraMaterial>> {
        self.map.get(&id)
    }

    /// Brings `id`'s mirror up to date with its base and returns it.
    pub fn sync(
        &mut self,
        id: AssetId<M>,
        base: AuroraMaterial,
        materials: &mut Assets<AuroraMaterial>,
    ) -> Handle<AuroraMaterial> {
        match self.map.get(&id) {
            Some(handle) => {
                if let Some(mut existing) = materials.get_mut(handle) {
                    *existing = base;
                }
                handle.clone()
            }
            None => {
                let handle = materials.add(base);
                self.map.insert(id, handle.clone());
                handle
            }
        }
    }

    pub fn remove(&mut self, id: AssetId<M>) -> Option<Handle<AuroraMaterial>> {
        self.map.remove(&id)
    }
}

/// Keeps every mirror current and tags entities wearing `Worn` with the mirror and `class`.
/// Everything a class needs beyond the base surface -- a parameter row, a wind term -- is its
/// own system; this is only the half every extended material shares.
pub fn mirror_extended<M: ExtendedSurface, Worn: Component + AsRef<Handle<M>>>(
    class: SurfaceClass,
) -> impl FnMut(
    Commands,
    ResMut<Mirrors<M>>,
    Res<Assets<M>>,
    ResMut<Assets<AuroraMaterial>>,
    MessageReader<AssetEvent<M>>,
    Query<(Entity, &Worn)>,
) {
    move |mut commands, mut mirrors, extended, mut materials, mut events, worn| {
        for event in events.read() {
            match event {
                AssetEvent::Added { id }
                | AssetEvent::Modified { id }
                | AssetEvent::LoadedWithDependencies { id } => {
                    if let Some(material) = extended.get(*id) {
                        let base = material.base().clone();
                        mirrors.sync(*id, base, &mut materials);
                    }
                }
                AssetEvent::Removed { id } | AssetEvent::Unused { id } => {
                    mirrors.remove(*id);
                }
            }
        }
        for (entity, handle) in &worn {
            let Some(mirror) = mirrors.get(handle.as_ref().id()) else {
                continue;
            };
            commands
                .entity(entity)
                .insert((AuroraMaterial3d(mirror.clone()), class));
        }
    }
}
