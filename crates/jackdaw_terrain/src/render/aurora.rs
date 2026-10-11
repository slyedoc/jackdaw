//! The splat on aurora: one surface class (`terrain_splat.slang`), one parameter row per
//! [`TerrainSplatMaterial`], and per-material control and slope buffers the shader reads by
//! address.
//!
//! A surface wearing [`TerrainSplat3d`] gets the material's mirror `AuroraMaterial` (whose
//! record carries the row) and the class. The row is rebuilt every frame and written when it
//! changed: texture slots resolve to bindless indices as aurora finishes uploading them, and a
//! re-uploaded image (a tint stroke) comes back under a new index.

use ash::vk;
use bevy::platform::collections::HashMap;
use bevy::prelude::*;
use bevy_aurora::{
    assets::aurora_asset,
    material::{AuroraMaterial, AuroraMaterial3d},
    render_buffer::{Buffer, BufferProvider},
    render_device::RenderDevice,
    render_env::WHITE_TEXTURE_IDX,
    surface_group::{SurfaceClass, SurfaceGroup, SurfaceGroupData, SurfaceGroupRegistry},
    vulkan_asset::VulkanAssets,
};
use bytemuck::{Pod, Zeroable};

use super::{TerrainSplat3d, TerrainSplatMaterial};
use crate::texture_set::MAX_TEXTURES;

const HAS_ALBEDO: u32 = 1;
const HAS_NORMAL: u32 = 2;
const FLIP_NORMAL_Y: u32 = 4;
const HAS_HEIGHT: u32 = 8;
const HAS_OCCLUSION: u32 = 16;
const HAS_ROUGHNESS: u32 = 32;

/// `SplatSlot` in terrain_splat.slang.
#[repr(C)]
#[derive(Clone, Copy, Default, PartialEq, Pod, Zeroable)]
struct SplatSlotGpu {
    uv_scale: f32,
    detile: f32,
    albedo: u32,
    normal: u32,
    height: u32,
    occlusion: u32,
    roughness: u32,
    flags: u32,
}

/// `TerrainSplat` in terrain_splat.slang: one row per terrain material.
#[repr(C)]
#[derive(Clone, Copy, PartialEq, Pod, Zeroable)]
struct TerrainSplatGpu {
    control: u64,
    slope: u64,
    terrain_size: [f32; 2],
    blend_sharpness: f32,
    perceptual_roughness: f32,
    control_resolution: u32,
    layer_count: u32,
    autoterrain_enabled: u32,
    autoterrain_base_slot: u32,
    autoterrain_slope_slot: u32,
    autoterrain_slope_start: f32,
    autoterrain_slope_end: f32,
    tint_strength: f32,
    tint: u32,
    terrain_origin: [f32; 2],
    pad: u32,
    slots: [SplatSlotGpu; MAX_TEXTURES],
}

const ROWS: u64 = 64;

struct Entry {
    row: u32,
    mirror: Handle<AuroraMaterial>,
    control: Option<Buffer<u32>>,
    slope: Option<Buffer<f32>>,
    /// The control or slope image changed since their buffers were filled.
    maps_dirty: bool,
    written: Option<TerrainSplatGpu>,
}

#[derive(Resource)]
struct SplatClass {
    class: SurfaceClass,
    rows: Buffer<TerrainSplatGpu>,
    entries: HashMap<AssetId<TerrainSplatMaterial>, Entry>,
    free: Vec<u32>,
    next: u32,
}

pub(super) fn plugin(app: &mut App) {
    app.add_systems(
        Startup,
        register_class
            .run_if(resource_exists::<SurfaceGroupRegistry>)
            .run_if(resource_exists::<RenderDevice>),
    )
    .add_systems(
        PostUpdate,
        (sync_rows, wear_class)
            .chain()
            .run_if(resource_exists::<SplatClass>),
    )
    .add_observer(shed_class);
}

fn register_class(
    mut commands: Commands,
    mut registry: ResMut<SurfaceGroupRegistry>,
    mut data: ResMut<SurfaceGroupData>,
    render_device: Res<RenderDevice>,
    asset_server: Res<AssetServer>,
) {
    let class = registry.register(SurfaceGroup {
        label: "jackdaw_terrain::splat".to_string(),
        closest_hit: asset_server.load(aurora_asset("shaders/terrain_splat.slang")),
        any_hit: None,
    });
    let rows: Buffer<TerrainSplatGpu> = render_device.create_host_buffer(
        ROWS,
        vk::BufferUsageFlags::STORAGE_BUFFER | vk::BufferUsageFlags::SHADER_DEVICE_ADDRESS,
    );
    data.set(class, rows.address);
    commands.insert_resource(SplatClass {
        class,
        rows,
        entries: HashMap::default(),
        free: Vec::new(),
        next: 0,
    });
}

/// `bytes` as `T`s, `count` of them, zero-padded.
fn words<T: Pod + Zeroable>(bytes: Option<&[u8]>, count: usize) -> Vec<T> {
    let mut out = vec![T::zeroed(); count];
    if let Some(bytes) = bytes {
        let size = size_of::<T>();
        for (slot, chunk) in out.iter_mut().zip(bytes.chunks_exact(size)) {
            *slot = bytemuck::pod_read_unaligned(chunk);
        }
    }
    out
}

/// Fill (or reallocate) a map buffer from an image's bytes.
fn fill<T: Pod + Zeroable>(
    rd: &RenderDevice,
    buffer: &mut Option<Buffer<T>>,
    image: Option<&Image>,
    count: usize,
) {
    let Some(image) = image else {
        return;
    };
    let values = words::<T>(image.data.as_deref(), count);
    if buffer
        .as_ref()
        .is_some_and(|b| b.nr_elements != count as u64)
        && let Some(old) = buffer.take()
    {
        rd.destroyer.destroy_buffer(old.handle);
    }
    let buffer = buffer.get_or_insert_with(|| {
        rd.create_host_buffer(
            count.max(1) as u64,
            vk::BufferUsageFlags::STORAGE_BUFFER | vk::BufferUsageFlags::SHADER_DEVICE_ADDRESS,
        )
    });
    rd.map_buffer(buffer).copy_from_slice(&values);
}

#[allow(clippy::too_many_arguments)]
fn sync_rows(
    mut state: ResMut<SplatClass>,
    splats: Res<Assets<TerrainSplatMaterial>>,
    images: Res<Assets<Image>>,
    textures: Res<VulkanAssets<Image>>,
    mut materials: ResMut<Assets<AuroraMaterial>>,
    mut data: ResMut<SurfaceGroupData>,
    rd: Res<RenderDevice>,
    mut splat_events: MessageReader<AssetEvent<TerrainSplatMaterial>>,
    mut image_events: MessageReader<AssetEvent<Image>>,
) {
    let state = &mut *state;
    for event in splat_events.read() {
        match event {
            AssetEvent::Added { id }
            | AssetEvent::Modified { id }
            | AssetEvent::LoadedWithDependencies { id } => {
                if let Some(entry) = state.entries.get_mut(id) {
                    entry.maps_dirty = true;
                }
            }
            AssetEvent::Removed { id } | AssetEvent::Unused { id } => {
                if let Some(entry) = state.entries.remove(id) {
                    data.clear_row(entry.mirror.id());
                    for handle in [
                        entry.control.map(|b| b.handle),
                        entry.slope.map(|b| b.handle),
                    ]
                    .into_iter()
                    .flatten()
                    {
                        rd.destroyer.destroy_buffer(handle);
                    }
                    state.free.push(entry.row);
                }
            }
        }
    }
    let changed: Vec<AssetId<Image>> = image_events
        .read()
        .filter_map(|event| match event {
            AssetEvent::Added { id } | AssetEvent::Modified { id } => Some(*id),
            _ => None,
        })
        .collect();

    for (id, splat) in splats.iter() {
        let res = splat.control_resolution.max(1) as usize;
        if !state.entries.contains_key(&id) {
            let row = state.free.pop().unwrap_or_else(|| {
                state.next += 1;
                state.next - 1
            });
            if u64::from(row) >= ROWS {
                warn_once!(
                    "terrain splat: more than {ROWS} terrain materials; the rest draw plain"
                );
                continue;
            }
            let mirror = materials.add(AuroraMaterial {
                base_color: Color::WHITE,
                perceptual_roughness: splat.perceptual_roughness,
                ..default()
            });
            data.set_row(mirror.id(), row);
            state.entries.insert(
                id,
                Entry {
                    row,
                    mirror,
                    control: None,
                    slope: None,
                    maps_dirty: true,
                    written: None,
                },
            );
        }
        let entry = state.entries.get_mut(&id).expect("inserted above");
        if changed.contains(&splat.control.id()) || changed.contains(&splat.slope.id()) {
            entry.maps_dirty = true;
        }
        if entry.maps_dirty {
            fill(
                &rd,
                &mut entry.control,
                images.get(&splat.control),
                res * res,
            );
            fill(&rd, &mut entry.slope, images.get(&splat.slope), res * res);
            entry.maps_dirty = entry.control.is_none();
        }

        let index = |handle: Option<&Handle<Image>>| -> Option<u32> {
            let texture = textures.get_by_id(handle?.id())?;
            Some(rd.register_bindless_texture(texture))
        };
        let pick = |list: &Vec<Option<Handle<Image>>>, slot: usize| -> Option<u32> {
            index(list.get(slot).and_then(Option::as_ref))
        };
        let mut slots = [SplatSlotGpu::default(); MAX_TEXTURES];
        for (slot, gpu) in slots.iter_mut().enumerate() {
            let (uv_scale, detile) = (
                splat.uv_scales[slot / 4][slot % 4],
                splat.detile_strengths[slot / 4][slot % 4],
            );
            let bit = |mask: u32| mask & (1 << slot) != 0;
            let mut flags = 0;
            let mut take = |found: Option<u32>, flag: u32| {
                found.map_or(WHITE_TEXTURE_IDX, |i| {
                    flags |= flag;
                    i
                })
            };
            let albedo = take(pick(&splat.slots.albedo, slot), HAS_ALBEDO);
            let normal = take(pick(&splat.slots.normal, slot), HAS_NORMAL);
            let height = take(pick(&splat.slots.height, slot), HAS_HEIGHT);
            let occlusion = take(
                pick(&splat.slots.occlusion, slot).filter(|_| bit(splat.occlusion_slots)),
                HAS_OCCLUSION,
            );
            let roughness = take(
                pick(&splat.slots.roughness, slot).filter(|_| bit(splat.roughness_slots)),
                HAS_ROUGHNESS,
            );
            if bit(splat.flip_normal_slots) {
                flags |= FLIP_NORMAL_Y;
            }
            *gpu = SplatSlotGpu {
                uv_scale,
                detile,
                albedo,
                normal,
                height,
                occlusion,
                roughness,
                flags,
            };
        }
        let row = TerrainSplatGpu {
            control: entry.control.as_ref().map_or(0, |b| b.address),
            slope: entry.slope.as_ref().map_or(0, |b| b.address),
            terrain_size: splat.terrain_size.to_array(),
            blend_sharpness: splat.blend_sharpness,
            perceptual_roughness: splat.perceptual_roughness,
            control_resolution: splat.control_resolution,
            layer_count: splat.layer_count,
            autoterrain_enabled: splat.autoterrain_enabled,
            autoterrain_base_slot: splat.autoterrain_base_slot,
            autoterrain_slope_slot: splat.autoterrain_slope_slot,
            autoterrain_slope_start: splat.autoterrain_slope_start,
            autoterrain_slope_end: splat.autoterrain_slope_end,
            tint_strength: splat.tint_strength,
            tint: index(Some(&splat.tint)).unwrap_or(WHITE_TEXTURE_IDX),
            terrain_origin: splat.terrain_origin.to_array(),
            pad: 0,
            slots,
        };
        if entry.written != Some(row) {
            rd.map_buffer(&mut state.rows)[entry.row as usize] = row;
            entry.written = Some(row);
        }
    }
}

/// Every surface wearing a splat traces with its material's mirror, in the class.
fn wear_class(
    mut commands: Commands,
    state: Res<SplatClass>,
    worn: Query<(
        Entity,
        &TerrainSplat3d,
        Option<&AuroraMaterial3d>,
        Option<&SurfaceClass>,
    )>,
) {
    for (entity, splat, material, class) in &worn {
        let Some(entry) = state.entries.get(&splat.0.id()) else {
            continue;
        };
        if material.is_some_and(|m| m.0 == entry.mirror) && class == Some(&state.class) {
            continue;
        }
        commands
            .entity(entity)
            .insert((AuroraMaterial3d(entry.mirror.clone()), state.class));
    }
}

/// A surface that stops wearing a splat goes back to the opaque class with its own material.
fn shed_class(remove: On<Remove<TerrainSplat3d>>, mut commands: Commands) {
    commands.entity(remove.entity).try_remove::<SurfaceClass>();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The row is read by address with the shader's layout; `TerrainSplat` there is 592 bytes.
    #[test]
    fn the_row_is_the_shaders_size() {
        assert_eq!(size_of::<SplatSlotGpu>(), 32);
        assert_eq!(size_of::<TerrainSplatGpu>(), 80 + 32 * MAX_TEXTURES);
    }
}
