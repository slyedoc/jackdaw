//! Drawing a terrain's stored scatter.
//!
//! A placement is data, not an entity: what is spawned is the least a draw needs
//! -- an [`AuroraMesh3d`], an [`AuroraMaterial3d`] and a transform -- so every
//! placement of the same asset shares one BLAS.
//!
//! A palette entry names a baked `.bsn` scene. It is resolved once rather than
//! once per placement: the scene is spawned, its hierarchy flattened into a list
//! of [`ScatterPrimitive`]s (a mesh, a material and the transform that part sat
//! at inside the scene) and despawned again.
//!
//! Placements are spawned under one chunk entity per region. Nothing is culled:
//! a tracer sees what is off screen in its shadows and reflections.
//!
//! Both hosts -- an editor holding its documents in a store, a game holding one
//! per terrain -- write [`TerrainScatter`] onto the terrain entity and mark what
//! changed in [`ScatterDirty`].

use bevy::asset::LoadState;
use bevy::camera::primitives::Aabb;
use bevy::camera::visibility::VisibilityRange;
use bevy::log::warn;
use bevy::platform::collections::{HashMap, HashSet};
use bevy::prelude::*;
use bevy::scene::ScenePatch;
use bevy_aurora::material::{AuroraMaterial, AuroraMaterial3d};
use bevy_aurora::mesh::{AuroraMesh, AuroraMesh3d};
use std::collections::BTreeMap;

use jackdaw_scene_types::{MaterialOverrides, MaterialSlot};

use crate::placement::{ScatterPalette, ScatterPaletteEntry, ScatterPlacement, is_prefab_asset};
use crate::region::RegionCoord;
use crate::sidecar::RegionTerrainData;

/// Tallest an asset can be and still count as ground cover: not an obstacle a
/// path has to go round, and not worth drawing once its pixels are smaller than
/// the batch is wide.
pub const GROUND_COVER_HEIGHT: f32 = 1.5;

/// Distance past which ground cover stops drawing when a palette entry
/// states no cutoff of its own.
pub const GROUND_COVER_CULL_DISTANCE: f32 = 80.0;

/// Fraction of a cutoff distance the fade to nothing spans.
const CULL_FADE: f32 = 0.1;

/// The stored scatter one terrain draws.
///
/// A projection of the terrain's document rather than the document itself, so
/// the renderer is the same however the host keeps its documents.
#[derive(Component, Clone, Debug, Default, PartialEq)]
pub struct TerrainScatter {
    /// The assets and stamp identities the placements index into.
    pub palette: ScatterPalette,
    /// Placements by the region holding them, in region-coordinate order.
    pub regions: Vec<ScatterRegion>,
}

/// One region's placements, with where that region sits.
#[derive(Clone, Debug, PartialEq)]
pub struct ScatterRegion {
    pub coord: RegionCoord,
    /// Terrain-local position of the region's minimum corner: what a
    /// placement's offsets are measured from.
    pub origin: Vec3,
    pub placements: Vec<ScatterPlacement>,
}
impl TerrainScatter {
    /// The scatter a document holds, in the space of the terrain it sits
    /// on.
    pub fn from_document(data: &RegionTerrainData) -> Self {
        let mut regions = Vec::new();
        for (coord, region) in data.regions.iter_sorted() {
            if region.placements().is_empty() {
                continue;
            }
            let origin = data.placement_position(
                coord,
                &ScatterPlacement {
                    group: 0,
                    asset: 0,
                    x: 0.0,
                    y: 0.0,
                    z: 0.0,
                    yaw: 0.0,
                    scale: 1.0,
                },
            );
            regions.push(ScatterRegion {
                coord,
                origin,
                placements: region.placements().to_vec(),
            });
        }
        Self {
            palette: data.scatter.clone(),
            regions,
        }
    }

    /// Whether there is nothing to draw.
    pub fn is_empty(&self) -> bool {
        self.regions.iter().all(|r| r.placements.is_empty())
    }

    /// How many placements this draws.
    pub fn placement_count(&self) -> usize {
        self.regions.iter().map(|r| r.placements.len()).sum()
    }
}

/// Which of a terrain's regions the renderer has yet to catch up with.
///
/// A host that edited one stroke's worth of placements names the regions
/// it touched; one that has just loaded a document sets `all`. Empty and
/// not `all` means every chunk on screen is already the data.
#[derive(Component, Clone, Debug, Default)]
pub struct ScatterDirty {
    /// Every chunk is stale, including ones no region backs any more.
    pub all: bool,
    /// Chunks stale by coordinate.
    pub regions: HashSet<RegionCoord>,
}

impl ScatterDirty {
    /// Mark every chunk stale.
    pub fn all() -> Self {
        Self {
            all: true,
            regions: HashSet::new(),
        }
    }

    /// Mark one region's chunk stale.
    pub fn touch(&mut self, coord: RegionCoord) {
        self.regions.insert(coord);
    }

    /// Fold another mark into this one: a mark the renderer has not caught up
    /// with must not be overwritten by the next edit's, or the regions between
    /// them never rebuild.
    pub fn merge(&mut self, other: &Self) {
        self.all |= other.all;
        self.regions.extend(other.regions.iter().copied());
    }

    fn is_clean(&self) -> bool {
        !self.all && self.regions.is_empty()
    }
}

/// One region's placements, as one parent.
#[derive(Component, Clone, Copy, Debug)]
pub struct ScatterChunk {
    /// The terrain this belongs to.
    pub terrain: Entity,
    pub coord: RegionCoord,
    /// Bounds of everything drawn under this chunk, in the terrain's local
    /// space.
    pub bounds: Aabb,
}

/// One drawn placement, by where in the document it came from.
///
/// The only link back to the data: nothing else about the entity says it is
/// scatter.
#[derive(Component, Clone, Copy, Debug)]
pub struct ScatterRendered {
    pub region: RegionCoord,
    /// Index into that region's placement list.
    pub index: usize,
}

/// One drawable part of a palette asset.
#[derive(Clone, Debug)]
pub struct ScatterPrimitive {
    pub mesh: Handle<AuroraMesh>,
    pub material: Handle<AuroraMaterial>,
    /// The part's material name, which a palette entry's overrides are keyed by.
    pub material_name: Option<String>,
    /// Where this part sat inside the scene, flattened through the hierarchy
    /// above it.
    pub local: Transform,
}

/// What one palette asset resolved to.
#[derive(Clone, Debug)]
enum ScatterAsset {
    /// The scene is loading.
    Loading(Handle<ScenePatch>),
    Ready(ReadyAsset),
    /// The scene failed to load, or the entry names something other than a
    /// scene. Placements of it draw nothing, and the failure is reported once.
    Failed,
}

#[derive(Clone, Debug)]
struct ReadyAsset {
    primitives: Vec<ScatterPrimitive>,
    /// Bounds of every primitive together, at scale 1.
    bounds: Aabb,
}

impl ReadyAsset {
    /// How tall this asset stands at scale 1.
    fn height(&self) -> f32 {
        self.bounds.half_extents.y * 2.0
    }
}

/// Palette assets resolved once each, shared by every terrain naming them.
#[derive(Resource, Default)]
pub struct ScatterAssets {
    entries: HashMap<String, ScatterAsset>,
    /// Assets that finished resolving since the last rebuild: every
    /// terrain naming one of these is stale.
    settled: Vec<String>,
}

impl ScatterAssets {
    /// Start loading `asset` unless it is already known.
    pub fn request(&mut self, server: &AssetServer, asset: &str) {
        if self.entries.contains_key(asset) {
            return;
        }
        let entry = if is_prefab_asset(asset) {
            ScatterAsset::Loading(server.load(asset.to_string()))
        } else {
            warn!(
                "terrain scatter: {asset} is not a .bsn scene; glTF is an import format, bake it with prop_import"
            );
            self.settled.push(asset.to_string());
            ScatterAsset::Failed
        };
        self.entries.insert(asset.to_string(), entry);
    }

    /// The primitives a palette entry draws, or `None` while it is loading
    /// or after it failed.
    pub fn primitives(&self, asset: &str) -> Option<&[ScatterPrimitive]> {
        match self.entries.get(asset) {
            Some(ScatterAsset::Ready(ready)) => Some(&ready.primitives),
            _ => None,
        }
    }

    /// How tall a palette entry stands at scale 1, or `None` while it is
    /// unresolved.
    pub fn height(&self, asset: &str) -> Option<f32> {
        Some(self.bounds(asset)?.half_extents.y * 2.0)
    }

    /// A palette entry's bounding box at scale 1, or `None` while it is
    /// unresolved. What a navmesh bake stands an obstacle in.
    pub fn bounds(&self, asset: &str) -> Option<Aabb> {
        match self.entries.get(asset) {
            Some(ScatterAsset::Ready(ready)) => Some(ready.bounds),
            _ => None,
        }
    }
}

/// The bounding box a palette entry's placement stands in at scale 1, or
/// `None` while what it draws is unresolved. What a navmesh bake stands an
/// obstacle in.
pub fn palette_entry_bounds(assets: &ScatterAssets, entry: &ScatterPaletteEntry) -> Option<Aabb> {
    assets.bounds(&entry.asset)
}

/// Resolves palette assets and draws stored scatter.
///
/// Independent of [`super::TerrainRenderPlugin`]: a host that draws the ground
/// without scatter adds one and not the other.
pub struct ScatterRenderPlugin;

/// The stages a host orders its own scatter work against.
#[derive(SystemSet, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ScatterSystems {
    /// Resolving palette assets and respawning the chunks a dirty mark
    /// names. A host that writes [`TerrainScatter`] runs before this.
    Rebuild,
    /// Flattening the scenes that have finished loading, which both the
    /// scatter and the detail renderers read the results of.
    Resolve,
}

/// The resolved palette store, shared by everything that draws a flattened
/// asset. Added by [`ScatterRenderPlugin`] and [`super::detail::DetailRenderPlugin`] alike.
pub struct ScatterAssetPlugin;

impl Plugin for ScatterAssetPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<ScatterAssets>()
            .configure_sets(
                Update,
                ScatterSystems::Resolve.in_set(ScatterSystems::Rebuild),
            )
            .add_systems(
                Update,
                resolve_palette_assets
                    .in_set(ScatterSystems::Resolve)
                    .run_if(resource_exists::<Assets<ScenePatch>>)
                    .run_if(resource_exists::<Assets<AuroraMesh>>),
            );
    }
}

/// Add the shared store unless a sibling renderer already did.
pub(super) fn add_asset_plugin(app: &mut App) {
    if !app.is_plugin_added::<ScatterAssetPlugin>() {
        app.add_plugins(ScatterAssetPlugin);
    }
}

impl Plugin for ScatterRenderPlugin {
    fn build(&self, app: &mut App) {
        add_asset_plugin(app);
        app.add_systems(
            Update,
            (
                request_palette_assets.before(ScatterSystems::Resolve),
                rebuild_chunks.after(ScatterSystems::Resolve),
            )
                .in_set(ScatterSystems::Rebuild),
        );
    }
}

/// Start loading every palette asset no terrain has asked for yet.
fn request_palette_assets(
    mut assets: ResMut<ScatterAssets>,
    server: Res<AssetServer>,
    terrains: Query<&TerrainScatter, Changed<TerrainScatter>>,
) {
    for scatter in &terrains {
        for entry in &scatter.palette.assets {
            if !entry.is_tombstone() {
                assets.request(&server, &entry.asset);
            }
        }
    }
}

/// Flatten every scene that has finished loading into primitives: spawn it,
/// read its parts, despawn it.
fn resolve_palette_assets(world: &mut World) {
    let loading: Vec<(String, Handle<ScenePatch>)> = world
        .resource::<ScatterAssets>()
        .entries
        .iter()
        .filter_map(|(path, entry)| match entry {
            ScatterAsset::Loading(handle) => Some((path.clone(), handle.clone())),
            _ => None,
        })
        .collect();
    for (path, handle) in loading {
        let failed = matches!(
            world.resource::<AssetServer>().load_state(handle.id()),
            LoadState::Failed(_)
        );
        let resolved = world
            .resource::<Assets<ScenePatch>>()
            .get(&handle)
            .and_then(|patch| patch.resolved.clone());
        let outcome = match (failed, resolved) {
            (true, _) => {
                warn!("terrain scatter: {path} failed to load; its placements draw nothing");
                ScatterAsset::Failed
            }
            (false, None) => continue,
            (false, Some(resolved)) => {
                let root = match resolved.spawn(world) {
                    Ok(root) => root.id(),
                    Err(err) => {
                        warn!("terrain scatter: {path} does not spawn: {err}");
                        settle(world, path, ScatterAsset::Failed);
                        continue;
                    }
                };
                let ready = flatten(world, root);
                world.entity_mut(root).despawn();
                match ready {
                    Some(ready) => ScatterAsset::Ready(ready),
                    None => continue,
                }
            }
        };
        settle(world, path, outcome);
    }
}

fn settle(world: &mut World, path: String, outcome: ScatterAsset) {
    let mut assets = world.resource_mut::<ScatterAssets>();
    assets.settled.push(path.clone());
    assets.entries.insert(path, outcome);
}

/// Every drawable part under `root`, with the transforms above it folded in.
///
/// `None` while a part's mesh is still loading: a half-resolved asset would
/// have to be rebuilt when the rest arrived.
fn flatten(world: &World, root: Entity) -> Option<ReadyAsset> {
    let meshes = world.resource::<Assets<AuroraMesh>>();
    let mut primitives = Vec::new();
    let mut min = Vec3::splat(f32::INFINITY);
    let mut max = Vec3::splat(f32::NEG_INFINITY);
    let mut pending = vec![(root, Transform::IDENTITY)];
    while let Some((entity, parent)) = pending.pop() {
        let entity_ref = world.entity(entity);
        let local = match entity == root {
            true => Transform::IDENTITY,
            false => parent * entity_ref.get::<Transform>().copied().unwrap_or_default(),
        };
        if let Some(children) = entity_ref.get::<Children>() {
            pending.extend(children.iter().map(|child| (child, local)));
        }
        let Some(mesh) = entity_ref.get::<AuroraMesh3d>() else {
            continue;
        };
        let aabb = meshes.get(&mesh.0)?.aabb;
        let centre = Vec3::from_slice(&aabb.center[..3]);
        let half = Vec3::from_slice(&aabb.half_extent[..3]);
        let affine = local.compute_affine();
        let centre = affine.transform_point3(centre);
        let radius = affine.matrix3.abs().mul_vec3(half);
        min = min.min(centre - radius);
        max = max.max(centre + radius);
        primitives.push(ScatterPrimitive {
            mesh: mesh.0.clone(),
            material: entity_ref
                .get::<AuroraMaterial3d>()
                .map(|material| material.0.clone())
                .unwrap_or_default(),
            material_name: entity_ref
                .get::<MaterialSlot>()
                .map(|slot| slot.0.clone())
                .or_else(|| material_name_of(entity_ref.get::<Name>()?)),
            local,
        });
    }
    if primitives.is_empty() {
        min = Vec3::ZERO;
        max = Vec3::ZERO;
    }
    Some(ReadyAsset {
        primitives,
        bounds: Aabb::from_min_max(min, max),
    })
}

/// The material half of a baked part's `node.material` name.
fn material_name_of(name: &Name) -> Option<String> {
    name.as_str()
        .rsplit_once('.')
        .map(|(_, material)| material.to_string())
}

/// Respawn the chunks a terrain's dirty regions name.
fn rebuild_chunks(
    mut commands: Commands,
    mut assets: ResMut<ScatterAssets>,
    mut terrains: Query<(
        Entity,
        &TerrainScatter,
        &mut ScatterDirty,
        Option<&Children>,
    )>,
    chunks: Query<&ScatterChunk>,
) {
    let settled = std::mem::take(&mut assets.settled);
    for (terrain, scatter, mut dirty, children) in &mut terrains {
        // An asset that has just resolved makes every chunk drawing it stale,
        // and a chunk that drew nothing is exactly the one waiting for it.
        if !settled.is_empty()
            && scatter
                .palette
                .assets
                .iter()
                .any(|entry| settled.contains(&entry.asset))
        {
            dirty.all = true;
        }
        if dirty.is_clean() {
            continue;
        }

        let stale: Vec<Entity> = children
            .map(RelationshipTarget::iter)
            .into_iter()
            .flatten()
            .filter(|child| {
                chunks
                    .get(*child)
                    .is_ok_and(|chunk| dirty.all || dirty.regions.contains(&chunk.coord))
            })
            .collect();
        for entity in stale {
            commands.entity(entity).despawn();
        }

        for region in &scatter.regions {
            if !dirty.all && !dirty.regions.contains(&region.coord) {
                continue;
            }
            spawn_chunk(&mut commands, &assets, terrain, scatter, region);
        }

        *dirty = ScatterDirty::default();
    }
}

fn spawn_chunk(
    commands: &mut Commands,
    assets: &ScatterAssets,
    terrain: Entity,
    scatter: &TerrainScatter,
    region: &ScatterRegion,
) {
    let mut min = Vec3::splat(f32::INFINITY);
    let mut max = Vec3::splat(f32::NEG_INFINITY);
    let mut instances = Vec::new();

    for (index, placement) in region.placements.iter().enumerate() {
        let Some(entry) = scatter.palette.asset(placement.asset) else {
            continue;
        };
        let Some(ScatterAsset::Ready(ready)) = assets.entries.get(&entry.asset) else {
            continue;
        };
        let materials: &BTreeMap<String, String> = &entry.materials;
        let stand = Transform {
            translation: region.origin + placement.offset(),
            rotation: Quat::from_rotation_y(placement.yaw),
            scale: Vec3::splat(placement.scale),
        };
        let range = cull_range(entry.cull_distance, ready.height() * stand.scale.y);
        for primitive in &ready.primitives {
            let dressed = primitive
                .material_name
                .as_ref()
                .filter(|name| materials.contains_key(*name))
                .map(|name| {
                    (
                        MaterialSlot(name.clone()),
                        MaterialOverrides {
                            materials: materials.clone(),
                        },
                    )
                });
            instances.push((
                (
                    AuroraMesh3d(primitive.mesh.clone()),
                    AuroraMaterial3d(primitive.material.clone()),
                    stand * primitive.local,
                    ScatterRendered {
                        region: region.coord,
                        index,
                    },
                ),
                range.clone(),
                dressed,
            ));
        }
        let affine = stand.compute_affine();
        let centre = affine.transform_point3(Vec3::from(ready.bounds.center));
        let radius = affine
            .matrix3
            .abs()
            .mul_vec3(Vec3::from(ready.bounds.half_extents));
        min = min.min(centre - radius);
        max = max.max(centre + radius);
    }

    if instances.is_empty() {
        return;
    }
    let bounds = Aabb::from_min_max(min, max);
    commands
        .spawn((
            ScatterChunk {
                terrain,
                coord: region.coord,
                bounds,
            },
            Transform::IDENTITY,
            Visibility::default(),
            ChildOf(terrain),
        ))
        .with_children(|chunk| {
            for (instance, range, dressed) in instances {
                let mut drawn = chunk.spawn(instance);
                if let Some(range) = range {
                    drawn.insert(range);
                }
                if let Some(dressed) = dressed {
                    drawn.insert(dressed);
                }
            }
        });
}

/// How far a placement of this asset draws, or `None` for one that draws at
/// every distance.
///
/// A palette entry that states a cutoff is taken at its word. One that does not
/// gets the ground-cover default when the asset is short enough to be ground
/// cover, and no cutoff otherwise.
///
/// The cutoff fades rather than snaps, so a batch of ground cover does not pop
/// in as the camera creeps over the distance.
fn cull_range(cull_distance: f32, height: f32) -> Option<VisibilityRange> {
    let distance = if cull_distance > 0.0 {
        cull_distance
    } else if height > 0.0 && height <= GROUND_COVER_HEIGHT {
        GROUND_COVER_CULL_DISTANCE
    } else {
        return None;
    };
    if !distance.is_finite() {
        return None;
    }
    let margin = distance * CULL_FADE;
    Some(VisibilityRange {
        start_margin: 0.0..0.0,
        end_margin: (distance - margin)..distance,
        use_aabb: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::placement::ScatterPaletteEntry;
    use crate::region::{RegionSize, TerrainRegions};

    fn document() -> RegionTerrainData {
        let mut data = RegionTerrainData {
            regions: TerrainRegions::new(RegionSize::new(4).unwrap()),
            scatter: ScatterPalette {
                assets: vec![ScatterPaletteEntry::new("models/tree.bsn")],
                groups: vec!["woods".to_string()],
            },
            ..RegionTerrainData::default()
        };
        data.regions.set_height(0, 0, 0.0);
        data.add_placement(Vec3::new(1.0, 0.5, 2.0), 0, 0, 0.25, 1.0)
            .expect("the region is allocated");
        data.add_placement(Vec3::new(3.0, 0.5, 1.0), 0, 0, 0.0, 2.0)
            .expect("the region is allocated");
        data
    }

    #[test]
    fn a_projection_carries_every_placement_in_terrain_local_space() {
        let data = document();
        let scatter = TerrainScatter::from_document(&data);
        assert_eq!(scatter.placement_count(), 2);
        let region = &scatter.regions[0];
        assert_eq!(region.origin, Vec3::ZERO);
        assert_eq!(
            region.origin + region.placements[0].offset(),
            Vec3::new(1.0, 0.5, 2.0)
        );
    }

    #[test]
    fn a_document_with_no_placements_projects_to_nothing_to_draw() {
        let data = RegionTerrainData::default();
        assert!(TerrainScatter::from_document(&data).is_empty());
    }

    /// A chunk holds one entity per primitive per placement, and each is
    /// only what a draw needs: no name, and nothing a document or an
    /// outliner could pick up.
    #[test]
    fn a_chunk_spawns_one_entity_per_primitive_per_placement_and_names_none_of_them() {
        let mut app = App::new();
        app.add_plugins(bevy::asset::AssetPlugin::default())
            .init_asset::<AuroraMesh>()
            .init_asset::<AuroraMaterial>();

        let (bark, leaves) = {
            let meshes = app.world_mut().resource_mut::<Assets<AuroraMesh>>();
            (meshes.reserve_handle(), meshes.reserve_handle())
        };
        let material = app
            .world_mut()
            .resource_mut::<Assets<AuroraMaterial>>()
            .reserve_handle();
        let mut assets = ScatterAssets::default();
        assets.entries.insert(
            "models/tree.bsn".to_string(),
            ScatterAsset::Ready(ReadyAsset {
                primitives: vec![
                    ScatterPrimitive {
                        mesh: bark,
                        material: material.clone(),
                        material_name: Some("Bark".to_string()),
                        local: Transform::IDENTITY,
                    },
                    ScatterPrimitive {
                        mesh: leaves,
                        material,
                        material_name: Some("Leaves".to_string()),
                        local: Transform::from_xyz(0.0, 2.0, 0.0),
                    },
                ],
                bounds: Aabb::from_min_max(Vec3::new(-1.0, 0.0, -1.0), Vec3::new(1.0, 4.0, 1.0)),
            }),
        );
        app.insert_resource(assets);

        let scatter = TerrainScatter::from_document(&document());
        let placements = scatter.placement_count();
        let terrain = app
            .world_mut()
            .spawn((
                Transform::IDENTITY,
                Visibility::default(),
                scatter,
                ScatterDirty::all(),
            ))
            .id();
        app.add_systems(Update, rebuild_chunks);
        app.update();

        let chunks: Vec<Entity> = app
            .world_mut()
            .query_filtered::<Entity, With<ScatterChunk>>()
            .iter(app.world())
            .collect();
        assert_eq!(chunks.len(), 1, "one chunk per region holding placements");
        assert_eq!(
            app.world().get::<ChildOf>(chunks[0]).map(ChildOf::parent),
            Some(terrain)
        );

        let drawn: Vec<Entity> = app
            .world()
            .get::<Children>(chunks[0])
            .map(|children| children.iter().collect())
            .unwrap_or_default();
        assert_eq!(drawn.len(), placements * 2, "one entity per primitive");
        for entity in drawn {
            assert!(app.world().get::<AuroraMesh3d>(entity).is_some());
            assert!(app.world().get::<ScatterRendered>(entity).is_some());
            assert!(
                app.world().get::<Name>(entity).is_none(),
                "a drawn placement is not a named scene node"
            );
        }
    }

    #[test]
    fn a_palette_entrys_overrides_ride_on_every_placement_of_the_parts_they_name() {
        let mut app = App::new();
        app.add_plugins(bevy::asset::AssetPlugin::default())
            .init_asset::<AuroraMesh>()
            .init_asset::<AuroraMaterial>();
        let (bark, leaves) = {
            let meshes = app.world_mut().resource_mut::<Assets<AuroraMesh>>();
            (meshes.reserve_handle(), meshes.reserve_handle())
        };
        let material = app
            .world_mut()
            .resource_mut::<Assets<AuroraMaterial>>()
            .reserve_handle();
        let mut assets = ScatterAssets::default();
        assets.entries.insert(
            "models/tree.bsn".to_string(),
            ScatterAsset::Ready(ReadyAsset {
                primitives: vec![
                    ScatterPrimitive {
                        mesh: bark,
                        material: material.clone(),
                        material_name: Some("Bark".to_string()),
                        local: Transform::IDENTITY,
                    },
                    ScatterPrimitive {
                        mesh: leaves.clone(),
                        material,
                        material_name: Some("Leaves".to_string()),
                        local: Transform::from_xyz(0.0, 2.0, 0.0),
                    },
                ],
                bounds: Aabb::from_min_max(Vec3::new(-1.0, 0.0, -1.0), Vec3::new(1.0, 4.0, 1.0)),
            }),
        );
        app.insert_resource(assets);

        let mut data = document();
        data.scatter.assets[0]
            .materials
            .insert("Leaves".to_string(), "materials/pine.bsn".to_string());
        let scatter = TerrainScatter::from_document(&data);
        let placements = scatter.placement_count();
        app.world_mut().spawn((
            Transform::IDENTITY,
            Visibility::default(),
            scatter,
            ScatterDirty::all(),
        ));
        app.add_systems(Update, rebuild_chunks);
        app.update();

        let mut dressed = app
            .world_mut()
            .query::<(&AuroraMesh3d, &MaterialSlot, &MaterialOverrides)>();
        let dressed: Vec<_> = dressed.iter(app.world()).collect();
        assert_eq!(
            dressed.len(),
            placements,
            "every placement's leaves carry the override"
        );
        for (mesh, name, overrides) in dressed {
            assert_eq!(mesh.0, leaves);
            assert_eq!(name.0, "Leaves");
            assert_eq!(
                overrides.materials.get("Leaves").map(String::as_str),
                Some("materials/pine.bsn")
            );
        }
    }

    /// A region marked dirty is the only one respawned, so a stroke costs
    /// its own chunk rather than the whole terrain's.
    #[test]
    fn only_a_dirty_region_is_rebuilt() {
        let mut dirty = ScatterDirty::default();
        assert!(dirty.is_clean());
        dirty.touch(RegionCoord::new(1, 2));
        assert!(!dirty.is_clean());
        assert!(!dirty.all);
        assert!(ScatterDirty::all().all);
    }

    #[test]
    fn ground_cover_fades_out_and_anything_taller_draws_at_every_distance() {
        let cover = cull_range(0.0, 0.4).expect("ground cover has a cutoff");
        assert_eq!(cover.end_margin.end, GROUND_COVER_CULL_DISTANCE);
        assert!(
            cover.end_margin.start < cover.end_margin.end,
            "the cutoff fades rather than snaps"
        );
        assert!(
            cull_range(0.0, 6.0).is_none(),
            "nothing is attached when there is no cutoff"
        );
        assert_eq!(
            cull_range(25.0, 6.0)
                .expect("a stated cutoff")
                .end_margin
                .end,
            25.0
        );
    }
}
