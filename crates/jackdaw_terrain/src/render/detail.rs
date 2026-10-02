//! Drawing a terrain's detail layers: each tile's instances baked into one
//! [`AuroraMesh`]. A host writes [`TerrainDetailSource`] and [`DetailDirty`]
//! onto the terrain.
//!
//! TODO(aurora): the raster pass bent blades in the vertex stage (wind,
//! pressers, distance fade). A baked tile stands still until a tracer-side
//! deformer exists; `shaders/detail.wgsl` is the reference for it.

use std::sync::Arc;

use bevy::asset::RenderAssetUsages;
use bevy::camera::RenderTarget;
use bevy::camera::primitives::Aabb;
use bevy::camera::visibility::NoAutoAabb;
use bevy::math::{Affine3A, Mat3A};
use bevy::mesh::{Indices, MeshVertexAttribute, PrimitiveTopology, VertexFormat};
use bevy::platform::collections::{HashMap, HashSet};
use bevy::prelude::*;
use bevy_aurora::material::{AlphaMode, AuroraMaterial, AuroraMaterial3d};
use bevy_aurora::mesh::{AuroraMesh, AuroraMesh3d};
use jackdaw_scene_types::{
    DetailLayer, DetailMesh, DetailPresser, NavmeshExclude, SceneWind, Terrain, Wind,
};

use crate::channel::ChannelElement;
use crate::detail::{
    DetailInstance, DetailLod, detail_lod_at, detail_tiles_around, place_detail,
    tile_centre_distance,
};
use crate::heightmap::Heightmap;
use crate::rect::GridRect;
use crate::sidecar::{GridShape, RegionTerrainData};

use super::scatter::{ScatterAssets, ScatterPrimitive, ScatterSystems};

/// Pressers gathered around the viewer. Past this the nearest win.
pub const MAX_DETAIL_PRESSERS: usize = 16;

/// Grid cells along one edge of a detail tile.
pub const DETAIL_TILE_CELLS: u32 = 16;

/// Tiles seeded per frame.
pub const DETAIL_TILE_BUDGET: usize = 2;

/// How far up its own mesh a vertex stands, in `0..1`. The bake bends and
/// shades by it.
pub const ATTRIBUTE_HEIGHT_FRACTION: MeshVertexAttribute =
    MeshVertexAttribute::new("HeightFraction", 1_903_552_411, VertexFormat::Float32);

/// Card segments at each detail level.
const NEAR_SEGMENTS: u32 = 3;
const FAR_SEGMENTS: u32 = 1;

/// What a terrain's detail is grown from: its ground, and one entry per layer.
/// A projection of the terrain's document and component, shared in one `Arc`.
#[derive(Component, Clone, Debug)]
pub struct TerrainDetailSource(Arc<DetailSource>);

/// The body of a [`TerrainDetailSource`].
#[derive(Debug)]
pub struct DetailSource {
    /// The ground the instances stand on.
    pub heightmap: Heightmap,
    /// Where the grid sits and how far it reaches.
    pub grid: GridShape,
    /// One entry per layer, in the terrain's own order.
    pub layers: Vec<DetailLayerSource>,
}

/// What one layer is grown from.
#[derive(Debug)]
pub struct DetailLayerSource {
    /// The density channel as a dense row-major plane at the grid's resolution.
    pub density: Vec<u16>,
    /// The density channel's ceiling: a cell reading this is full coverage.
    pub max: u16,
    /// How the layer reads.
    pub layer: DetailLayer,
    /// What the placement is seeded from: the grid's placement and the density
    /// channel's name.
    pub seed: u64,
}

impl TerrainDetailSource {
    /// The detail a document and its terrain component grow between them, or
    /// `None` for a terrain with no layers.
    pub fn from_document(data: &RegionTerrainData, terrain: &Terrain) -> Option<Self> {
        if terrain.detail.is_empty() {
            return None;
        }
        let grid = data.grid_shape(terrain.size, terrain.resolution);
        let cells = (grid.resolution as usize) * (grid.resolution as usize);
        let layers = terrain
            .detail
            .iter()
            .map(|layer| {
                let index = data
                    .channels
                    .iter()
                    .position(|channel| channel.name == layer.density_channel);
                let (density, max) = match index {
                    Some(index) => (
                        data.regions.read_grid_channel(index, grid.resolution),
                        data.channels[index].element.max_value(),
                    ),
                    None => (vec![0; cells], ChannelElement::U8.max_value()),
                };
                DetailLayerSource {
                    density,
                    max,
                    seed: seed_of(&grid, &layer.density_channel),
                    layer: layer.clone(),
                }
            })
            .collect();

        let mut heights = Vec::new();
        data.regions
            .sample_grid_heights_into(grid.resolution, &mut heights);
        Some(Self(Arc::new(DetailSource {
            heightmap: Heightmap {
                resolution: grid.resolution,
                size: grid.size,
                origin: grid.origin,
                max_height: terrain.max_height,
                heights,
            },
            grid,
            layers,
        })))
    }

    /// What the tiles are seeded from.
    pub fn source(&self) -> &DetailSource {
        &self.0
    }

    /// World units per grid cell edge.
    pub fn cell_size(&self) -> f32 {
        let grid = &self.0.grid;
        grid.size.x / (grid.resolution.max(2) - 1) as f32
    }
}

/// A seed stable across runs: the ground the grid covers, plus the name of
/// the channel that grows the layer.
fn seed_of(grid: &GridShape, density_channel: &str) -> u64 {
    let mut seed = u64::from(grid.resolution);
    for word in [
        grid.origin.x.to_bits(),
        grid.origin.y.to_bits(),
        grid.size.x.to_bits(),
    ] {
        seed = seed.rotate_left(17) ^ u64::from(word);
    }
    for byte in density_channel.bytes() {
        seed = seed.rotate_left(7) ^ u64::from(byte);
    }
    seed
}

/// Which of a terrain's ground the renderer has yet to catch up with. The rect
/// covers the cells edits have touched; `None` is ground in step.
#[derive(Component, Clone, Copy, Debug, Default)]
pub struct DetailDirty {
    pub rect: Option<GridRect>,
}

impl DetailDirty {
    /// Mark the ground a rect covers stale. An uncaught mark widens to cover
    /// both rather than being replaced.
    pub fn touch(&mut self, rect: GridRect) {
        self.rect = Some(match self.rect {
            Some(held) => union(held, rect),
            None => rect,
        });
    }
}

/// The smallest rect covering both.
fn union(a: GridRect, b: GridRect) -> GridRect {
    let x = a.x.min(b.x);
    let z = a.z.min(b.z);
    let max_x = (a.x + a.width).max(b.x + b.width);
    let max_z = (a.z + a.height).max(b.z + b.height);
    GridRect {
        x,
        z,
        width: max_x - x,
        height: max_z - z,
    }
}

/// One seeded tile of one layer, and every instance standing on it.
#[derive(Component, Clone, Debug)]
pub struct DetailTile {
    /// The terrain this grew from, whose layers the instances draw with.
    pub terrain: Entity,
    /// Which of that terrain's layers this tile belongs to.
    pub layer: usize,
    /// Which tile of that terrain's grid this is.
    pub tile: IVec2,
    pub lod: DetailLod,
    /// The instances, in world space.
    pub instances: Arc<Vec<DetailInstance>>,
    /// What the instances occupy, for the frustum test.
    pub bounds: Aabb,
}

/// How far and how thickly detail draws, over what every layer declares.
/// Not persisted.
#[derive(Resource, Clone, Copy, Debug)]
pub struct DetailSettings {
    /// Multiplies every layer's instances per square metre.
    pub density_scale: f32,
    /// Multiplies every layer's cull distance.
    pub cull_scale: f32,
}

impl Default for DetailSettings {
    fn default() -> Self {
        Self {
            density_scale: 1.0,
            cull_scale: 1.0,
        }
    }
}

/// What the detail is currently bending away from, nearest the viewer first.
/// Filled from [`DetailPresser`] entities, capped at [`MAX_DETAIL_PRESSERS`].
#[derive(Resource, Clone, Debug, Default)]
pub struct DetailPressers {
    /// World position and reach, one entry per presser.
    pub pressers: Vec<(Vec3, f32)>,
}

/// The card meshes every terrain's card layers instance.
#[derive(Resource, Clone, Debug)]
pub struct DetailAssets {
    pub near_card: Handle<Mesh>,
    pub far_card: Handle<Mesh>,
}

impl DetailAssets {
    fn card(&self, lod: DetailLod) -> Handle<Mesh> {
        match lod {
            DetailLod::Near => self.near_card.clone(),
            DetailLod::Far => self.far_card.clone(),
        }
    }
}

/// What one asset path flattened to, keyed by that path.
#[derive(Resource, Default, Debug)]
pub struct DetailMeshes(HashMap<String, BuiltDetailMesh>);

/// One scene, merged into the single mesh a layer instances.
#[derive(Clone, Debug)]
pub struct BuiltDetailMesh {
    pub mesh: Handle<Mesh>,
    /// The base colour texture the scene's first textured part carries.
    pub color: Option<Handle<Image>>,
}

impl DetailMeshes {
    /// What a layer's asset path draws, or `None` until it has resolved.
    pub fn get(&self, asset: &str) -> Option<&BuiltDetailMesh> {
        self.0.get(asset)
    }
}

/// A card of `segments` stacked quads, one unit tall and wide, its foot at the
/// origin and its face on +Z. Straight-sided; the bake tapers it.
pub fn card_mesh(segments: u32) -> Mesh {
    let segments = segments.max(1);
    let mut positions = Vec::new();
    let mut normals = Vec::new();
    let mut uvs = Vec::new();
    let mut fractions = Vec::new();
    let mut indices = Vec::new();

    for row in 0..=segments {
        let v = row as f32 / segments as f32;
        for u in [0.0f32, 1.0] {
            positions.push([u - 0.5, v, 0.0]);
            normals.push([0.0, 0.0, 1.0]);
            uvs.push([u, v]);
            fractions.push(v);
        }
    }
    for row in 0..segments {
        let bottom = row * 2;
        indices.extend_from_slice(&[
            bottom,
            bottom + 1,
            bottom + 3,
            bottom,
            bottom + 3,
            bottom + 2,
        ]);
    }

    let readable_on_the_main_world = RenderAssetUsages::default();
    Mesh::new(PrimitiveTopology::TriangleList, readable_on_the_main_world)
        .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, positions)
        .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, normals)
        .with_inserted_attribute(Mesh::ATTRIBUTE_UV_0, uvs)
        .with_inserted_attribute(ATTRIBUTE_HEIGHT_FRACTION, fractions)
        .with_inserted_indices(Indices::U32(indices))
}

/// Seeds and retires the tiles around the viewer, and draws them. Independent
/// of [`super::TerrainRenderPlugin`]; either can be added alone.
pub struct DetailRenderPlugin;

/// The stages a host orders its own detail work against.
#[derive(SystemSet, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DetailSystems {
    /// Reseeding the tiles a dirty mark names and the ones the viewer has
    /// walked into. A host that writes [`TerrainDetailSource`] runs before this.
    Rebuild,
}

impl Plugin for DetailRenderPlugin {
    fn build(&self, app: &mut App) {
        super::scatter::add_asset_plugin(app);
        if !app.world().contains_resource::<Assets<Mesh>>() {
            app.init_asset::<Mesh>();
        }
        app.init_resource::<DetailSettings>()
            .init_resource::<SceneWind>()
            .init_resource::<DetailPressers>()
            .init_resource::<DetailMeshes>()
            .add_systems(Startup, init_detail_assets)
            .add_systems(
                Update,
                (
                    request_detail_assets.before(ScatterSystems::Resolve),
                    (
                        build_detail_meshes,
                        collect_detail_pressers,
                        rebuild_detail_tiles,
                    )
                        .chain()
                        .after(ScatterSystems::Resolve),
                )
                    .in_set(DetailSystems::Rebuild)
                    .run_if(resource_exists::<DetailAssets>)
                    .run_if(resource_exists::<Assets<AuroraMesh>>)
                    .run_if(resource_exists::<Assets<AuroraMaterial>>),
            );
    }
}

/// Build the card meshes once, for every layer to use.
fn init_detail_assets(mut commands: Commands, mut meshes: ResMut<Assets<Mesh>>) {
    commands.insert_resource(DetailAssets {
        near_card: meshes.add(card_mesh(NEAR_SEGMENTS)),
        far_card: meshes.add(card_mesh(FAR_SEGMENTS)),
    });
}

/// Start loading the scene every asset-backed layer names.
fn request_detail_assets(
    mut assets: ResMut<ScatterAssets>,
    server: Res<AssetServer>,
    terrains: Query<&TerrainDetailSource, Changed<TerrainDetailSource>>,
) {
    for source in &terrains {
        for entry in &source.source().layers {
            if let DetailMesh::Asset(path) = &entry.layer.mesh {
                assets.request(&server, path);
            }
        }
    }
}

/// Merge the primitives of every asset-backed layer whose scene has resolved.
fn build_detail_meshes(
    mut built: ResMut<DetailMeshes>,
    scatter: Res<ScatterAssets>,
    mut meshes: ResMut<Assets<Mesh>>,
    parts: Res<Assets<AuroraMesh>>,
    materials: Res<Assets<AuroraMaterial>>,
    terrains: Query<&TerrainDetailSource>,
) {
    let mut wanted: Vec<&String> = Vec::new();
    for source in &terrains {
        for entry in &source.source().layers {
            if let DetailMesh::Asset(path) = &entry.layer.mesh
                && !built.0.contains_key(path)
                && !wanted.contains(&path)
            {
                wanted.push(path);
            }
        }
    }
    for path in wanted {
        let Some(primitives) = scatter.primitives(path) else {
            continue;
        };
        let Some((merged, color)) = merge_primitives(path, primitives, &parts, &materials) else {
            continue;
        };
        let mesh = meshes.add(merged);
        built
            .0
            .insert(path.clone(), BuiltDetailMesh { mesh, color });
    }
}

/// The matrix that carries a normal through `affine`: the inverse transpose,
/// so that a non-uniform scale tilts the normal the way the surface turns.
fn normal_matrix_of(affine: Affine3A) -> Mat3A {
    affine.matrix3.inverse().transpose()
}

/// Every primitive of a flattened scene as one mesh, with the base colour
/// texture the first textured primitive carries. `None` while any is loading.
fn merge_primitives(
    asset: &str,
    primitives: &[ScatterPrimitive],
    meshes: &Assets<AuroraMesh>,
    materials: &Assets<AuroraMaterial>,
) -> Option<(Mesh, Option<Handle<Image>>)> {
    let mut positions: Vec<[f32; 3]> = Vec::new();
    let mut normals: Vec<[f32; 3]> = Vec::new();
    let mut uvs: Vec<[f32; 2]> = Vec::new();
    let mut indices: Vec<u32> = Vec::new();
    let mut color = None;
    let mut textures: Vec<Option<Handle<Image>>> = Vec::new();

    for primitive in primitives {
        if let Some(material) = materials.get(&primitive.material) {
            let texture = material.base_color_texture.clone();
            color = color.or_else(|| texture.clone());
            if !textures.contains(&texture) {
                textures.push(texture);
            }
        }
        let mesh = meshes.get(&primitive.mesh)?.flatten();
        let affine = primitive.local.compute_affine();
        let normal_matrix = normal_matrix_of(affine);
        let base = positions.len() as u32;
        for (vertex, point) in mesh.positions.iter().enumerate() {
            positions.push(affine.transform_point3(*point).to_array());
            let normal = mesh
                .normals
                .get(vertex)
                .map(|normal| normal_matrix.mul_vec3(*normal))
                .unwrap_or(Vec3::Y);
            normals.push(normal.normalize_or(Vec3::Y).to_array());
            uvs.push(mesh.uvs.get(vertex).copied().unwrap_or(Vec2::ZERO).to_array());
        }
        indices.extend(mesh.indices.iter().map(|index| base + index));
    }

    if positions.is_empty() {
        return None;
    }
    if textures.len() > 1 {
        warn!(
            "terrain detail: {asset} carries more than one base colour texture; \
             the whole layer samples the first, so every part shares that look"
        );
    }
    let lowest = positions
        .iter()
        .map(|point| point[1])
        .fold(f32::MAX, f32::min);
    let highest = positions
        .iter()
        .map(|point| point[1])
        .fold(f32::MIN, f32::max);
    let span = highest - lowest;
    let fractions: Vec<f32> = positions
        .iter()
        .map(|point| match span > f32::EPSILON {
            true => ((point[1] - lowest) / span).clamp(0.0, 1.0),
            false => 0.0,
        })
        .collect();

    let merged = Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::default(),
    )
    .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, positions)
    .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, normals)
    .with_inserted_attribute(Mesh::ATTRIBUTE_UV_0, uvs)
    .with_inserted_attribute(ATTRIBUTE_HEIGHT_FRACTION, fractions)
    .with_inserted_indices(Indices::U32(indices));
    Some((merged, color))
}

/// Cameras the field is laid out around, as both detail systems ask for them.
/// Marks the camera a terrain's detail layers are seeded around.
#[derive(Component, Clone, Copy, Debug, Default)]
pub struct DetailViewer;

type DetailViewers<'w, 's> = Query<
    'w,
    's,
    (
        Entity,
        &'static Camera,
        &'static GlobalTransform,
        Has<DetailViewer>,
        Option<&'static RenderTarget>,
    ),
    With<Camera3d>,
>;

/// Where the field is centred: an active [`DetailViewer`], else the highest-order active camera, window before image.
fn viewer_position(cameras: &DetailViewers<'_, '_>) -> Option<Vec3> {
    cameras
        .iter()
        .filter(|(_, camera, ..)| camera.is_active)
        .max_by_key(|(entity, camera, _, marked, target)| {
            let draws_to_image = matches!(target, Some(RenderTarget::Image(_)));
            (*marked, !draws_to_image, camera.order, *entity)
        })
        .map(|(_, _, transform, ..)| transform.translation())
}

/// Gather the pressers nearest the viewer.
fn collect_detail_pressers(
    mut pressers: ResMut<DetailPressers>,
    viewers: DetailViewers,
    pressing: Query<(&GlobalTransform, &DetailPresser)>,
) {
    let viewer = viewer_position(&viewers).unwrap_or(Vec3::ZERO);
    let mut found: Vec<(Vec3, f32)> = pressing
        .iter()
        .map(|(transform, presser)| (transform.translation(), presser.radius))
        .collect();
    found.sort_by(|a, b| {
        a.0.distance_squared(viewer)
            .total_cmp(&b.0.distance_squared(viewer))
    });
    found.truncate(MAX_DETAIL_PRESSERS);
    if pressers.pressers != found {
        pressers.pressers = found;
    }
}

/// The mesh one layer instances, or `None` while its asset is still resolving.
fn layer_mesh(
    assets: &DetailAssets,
    built: &DetailMeshes,
    layer: &DetailLayer,
    lod: DetailLod,
) -> Option<Handle<Mesh>> {
    match &layer.mesh {
        DetailMesh::Card => Some(assets.card(lod)),
        DetailMesh::Asset(path) => built.get(path).map(|built| built.mesh.clone()),
    }
}

/// Reseed the tiles around the viewer, layer by layer. Retiring is unbudgeted;
/// seeding is capped at [`DETAIL_TILE_BUDGET`] a frame.
fn rebuild_detail_tiles(
    mut commands: Commands,
    assets: Res<DetailAssets>,
    built: Res<DetailMeshes>,
    meshes: Res<Assets<Mesh>>,
    mut baked: ResMut<Assets<AuroraMesh>>,
    mut materials: ResMut<Assets<AuroraMaterial>>,
    settings: Res<DetailSettings>,
    wind: Res<SceneWind>,
    viewers: DetailViewers,
    mut terrains: Query<(
        Entity,
        &TerrainDetailSource,
        &GlobalTransform,
        &mut DetailDirty,
    )>,
    tiles: Query<(Entity, &DetailTile)>,
) {
    for (tile_entity, tile) in &tiles {
        let layers = terrains
            .get(tile.terrain)
            .map(|(_, source, _, _)| source.source().layers.len())
            .unwrap_or(0);
        if tile.layer >= layers {
            commands.entity(tile_entity).despawn();
        }
    }

    let Some(viewer) = viewer_position(&viewers) else {
        return;
    };

    for (entity, source, placed, mut dirty) in &mut terrains {
        let cell_size = source.cell_size();
        if cell_size <= 0.0 || !cell_size.is_finite() {
            continue;
        }
        let world_from_local = placed.affine();
        let local = world_from_local.inverse().transform_point3(viewer);
        let grid = &source.source().grid;
        let viewer_cell = ((Vec2::new(local.x, local.z) - grid.origin) / cell_size)
            .round()
            .as_ivec2();
        let stale = std::mem::take(&mut dirty.rect);
        let mut spent = 0;

        for (index, entry) in source.source().layers.iter().enumerate() {
            let cull_cells = entry.layer.cull_distance * settings.cull_scale / cell_size;
            let wanted = detail_tiles_around(viewer_cell, cull_cells, DETAIL_TILE_CELLS);

            let mut standing = HashSet::new();
            for (tile_entity, tile) in &tiles {
                if tile.terrain != entity || tile.layer != index {
                    continue;
                }
                let level = detail_lod_at(
                    tile_centre_distance(viewer_cell, tile.tile, DETAIL_TILE_CELLS),
                    cull_cells,
                    Some(tile.lod),
                );
                let retired = !wanted.iter().any(|(coord, _)| *coord == tile.tile)
                    || level != tile.lod
                    || stale.is_some_and(|rect| touches(rect, tile.tile));
                if retired {
                    commands.entity(tile_entity).despawn();
                } else {
                    standing.insert(tile.tile);
                }
            }

            for (coord, lod) in wanted {
                if spent >= DETAIL_TILE_BUDGET {
                    break;
                }
                if standing.contains(&coord) {
                    continue;
                }
                let Some(mesh) = layer_mesh(&assets, &built, &entry.layer, lod) else {
                    break;
                };
                spent += 1;
                let instances = seed_tile(source, &settings, index, coord, lod, world_from_local);
                let bounds = match instances.is_empty() {
                    true => footprint(source, coord, world_from_local),
                    false => tile_bounds(&instances, &entry.layer, &wind.0),
                };
                let drawn = meshes
                    .get(&mesh)
                    .and_then(|mesh| bake_tile(mesh, &instances, &entry.layer))
                    .and_then(|mesh| AuroraMesh::clustered(&mesh).ok())
                    .map(|mesh| {
                        let color = match &entry.layer.mesh {
                            DetailMesh::Card => None,
                            DetailMesh::Asset(path) => {
                                built.get(path).and_then(|built| built.color.clone())
                            }
                        };
                        (
                            AuroraMesh3d(baked.add(mesh)),
                            AuroraMaterial3d(materials.add(tile_material(&entry.layer, color))),
                        )
                    });
                let mut tile = commands.spawn((
                    DetailTile {
                        terrain: entity,
                        layer: index,
                        tile: coord,
                        lod,
                        instances: Arc::new(instances),
                        bounds,
                    },
                    Transform::IDENTITY,
                    Visibility::default(),
                    bounds,
                    NoAutoAabb,
                    NavmeshExclude,
                ));
                if let Some(drawn) = drawn {
                    tile.insert(drawn);
                }
            }
        }
    }
}

/// Whether a tile covers any of a dirty rect.
fn touches(rect: GridRect, tile: IVec2) -> bool {
    let min = tile * DETAIL_TILE_CELLS as i32;
    let max = min + IVec2::splat(DETAIL_TILE_CELLS as i32);
    let rect_max = IVec2::new((rect.x + rect.width) as i32, (rect.z + rect.height) as i32);
    min.x < rect_max.x && max.x > rect.x as i32 && min.y < rect_max.y && max.y > rect.z as i32
}

/// Seed one tile of one layer and lift its instances into world space.
fn seed_tile(
    source: &TerrainDetailSource,
    settings: &DetailSettings,
    index: usize,
    tile: IVec2,
    lod: DetailLod,
    world_from_local: Affine3A,
) -> Vec<DetailInstance> {
    let body = source.source();
    let Some(entry) = body.layers.get(index) else {
        return Vec::new();
    };
    let mut instances = place_detail(
        &entry.density,
        entry.max,
        &body.heightmap,
        &entry.layer,
        index,
        tile,
        DETAIL_TILE_CELLS,
        source.cell_size(),
        settings.density_scale,
        lod.density_scale(),
        entry.seed,
    );
    if world_from_local != Affine3A::IDENTITY {
        for instance in &mut instances {
            instance.position = world_from_local.transform_point3(instance.position);
        }
    }
    instances
}

/// The ground one tile covers, for a tile with no instances.
fn footprint(source: &TerrainDetailSource, tile: IVec2, world_from_local: Affine3A) -> Aabb {
    let cell_size = source.cell_size();
    let origin = source.source().grid.origin;
    let min = origin + tile.as_vec2() * DETAIL_TILE_CELLS as f32 * cell_size;
    let max = min + Vec2::splat(DETAIL_TILE_CELLS as f32 * cell_size);
    let corner = |at: Vec2| world_from_local.transform_point3(Vec3::new(at.x, 0.0, at.y));
    Aabb::from_min_max(corner(min), corner(max))
}

/// What a tile's instances occupy, widened for the strongest wind a scene is
/// likely to blow, bend and presser lean.
fn tile_bounds(instances: &[DetailInstance], layer: &DetailLayer, wind: &Wind) -> Aabb {
    let mut min = Vec3::splat(f32::INFINITY);
    let mut max = Vec3::splat(f32::NEG_INFINITY);
    for instance in instances {
        min = min.min(instance.position);
        max = max.max(instance.position);
    }
    let tall = layer.height[0].max(layer.height[1]);
    let leaned_by_the_wind = layer.wind_response * wind.strength * DetailLayer::BREEZE_LEAN;
    let lean = layer.bend + leaned_by_the_wind.abs() + layer.push_strength;
    Aabb::from_min_max(
        min - Vec3::new(lean, tall, lean),
        max + Vec3::new(lean, tall, lean),
    )
}

/// One tile's instances stood up around the layer mesh, as one world-space
/// mesh: the raster vertex stage's pose with the wind, pressers and fade left
/// out. A card is tapered to the blade the fragment stage used to carve.
fn bake_tile(mesh: &Mesh, instances: &[DetailInstance], layer: &DetailLayer) -> Option<Mesh> {
    if instances.is_empty() {
        return None;
    }
    let source = mesh
        .attribute(Mesh::ATTRIBUTE_POSITION)
        .and_then(|values| values.as_float3())?;
    let normals = mesh
        .attribute(Mesh::ATTRIBUTE_NORMAL)
        .and_then(|values| values.as_float3());
    let uvs = match mesh.attribute(Mesh::ATTRIBUTE_UV_0) {
        Some(bevy::mesh::VertexAttributeValues::Float32x2(values)) => Some(values.as_slice()),
        _ => None,
    };
    let fractions = match mesh.attribute(ATTRIBUTE_HEIGHT_FRACTION) {
        Some(bevy::mesh::VertexAttributeValues::Float32(values)) => Some(values.as_slice()),
        _ => None,
    };
    let local_indices: Vec<u32> = match mesh.indices() {
        Some(read) => read.iter().map(|index| index as u32).collect(),
        None => (0..source.len() as u32).collect(),
    };
    let is_card = layer.mesh == DetailMesh::Card;

    let count = instances.len() * source.len();
    let mut positions = Vec::with_capacity(count);
    let mut out_normals = Vec::with_capacity(count);
    let mut out_uvs = Vec::with_capacity(count);
    let mut indices = Vec::with_capacity(instances.len() * local_indices.len());
    for instance in instances {
        let [height, width, yaw, _tint] = instance.unpack().map(|byte| f32::from(byte) / 255.0);
        let height = layer.height[0] + (layer.height[1] - layer.height[0]) * height;
        let width = layer.width[0] + (layer.width[1] - layer.width[0]) * width;
        let spin = Quat::from_rotation_y(yaw * core::f32::consts::TAU);
        let tilt = instance.tilt;
        let ground = Vec3::new(tilt.x, (1.0 - tilt.length_squared()).max(0.0).sqrt(), tilt.y);
        let lean = Quat::from_rotation_arc(Vec3::Y, ground);
        let base = positions.len() as u32;
        for (vertex, point) in source.iter().enumerate() {
            let uv = uvs.and_then(|values| values.get(vertex)).copied().unwrap_or([0.0, 0.0]);
            let up = fractions
                .and_then(|values| values.get(vertex))
                .copied()
                .unwrap_or(uv[1]);
            let taper = match is_card {
                true => 1.0 - uv[1] * uv[1],
                false => 1.0,
            };
            let local = Vec3::new(
                point[0] * width * taper,
                up * height,
                point[2] * width + up * up * layer.bend,
            );
            let normal = normals
                .and_then(|values| values.get(vertex))
                .map_or(Vec3::Y, |normal| Vec3::from(*normal))
                .lerp(Vec3::Y, up)
                .normalize_or(Vec3::Y);
            positions.push((instance.position + lean * (spin * local)).to_array());
            out_normals.push((lean * (spin * normal)).to_array());
            out_uvs.push(uv);
        }
        indices.extend(local_indices.iter().map(|index| base + index));
    }
    Some(
        Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default())
            .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, positions)
            .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, out_normals)
            .with_inserted_attribute(Mesh::ATTRIBUTE_UV_0, out_uvs)
            .with_inserted_indices(Indices::U32(indices)),
    )
}

/// What a layer's tiles wear: the midpoint of its foot and tip colours over
/// its texture, both faces lit.
fn tile_material(layer: &DetailLayer, texture: Option<Handle<Image>>) -> AuroraMaterial {
    let mid = (linear_of(layer.color_base) + linear_of(layer.color_tip)) * 0.5;
    AuroraMaterial {
        base_color: Color::linear_rgb(mid.x, mid.y, mid.z),
        alpha_mode: match texture.is_some() {
            true => AlphaMode::Mask(0.5),
            false => AlphaMode::Opaque,
        },
        base_color_texture: texture,
        perceptual_roughness: 0.8,
        double_sided: true,
        cull_mode: None,
        ..default()
    }
}

/// An authored sRGB colour as a linear vector.
fn linear_of(rgb: [f32; 3]) -> Vec4 {
    let linear = Color::srgb(rgb[0], rgb[1], rgb[2]).to_linear();
    Vec4::new(linear.red, linear.green, linear.blue, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channel::ChannelDescriptor;
    use crate::region::{RegionSize, TerrainRegions};

    /// A terrain of flat ground, `side` cells on an edge, with one fully
    /// painted channel per name given.
    fn document(side: u32, channels: &[&str]) -> RegionTerrainData {
        let mut data = RegionTerrainData {
            channels: channels
                .iter()
                .map(|name| ChannelDescriptor::new(*name, ChannelElement::U8))
                .collect(),
            regions: TerrainRegions::new(RegionSize::new(side).unwrap()),
            ..RegionTerrainData::default()
        };
        data.regions.set_channel_count(channels.len());
        for z in 0..side as i32 {
            for x in 0..side as i32 {
                data.regions.set_height(x, z, 0.0);
                for channel in 0..channels.len() {
                    data.regions.set_channel(channel, x, z, 255);
                }
            }
        }
        data
    }

    fn layer(name: &str, channel: &str) -> DetailLayer {
        DetailLayer {
            name: name.to_string(),
            density_channel: channel.to_string(),
            cull_distance: 24.0,
            density_per_m2: 4.0,
            ..DetailLayer::default()
        }
    }

    fn terrain(layers: Vec<DetailLayer>) -> Terrain {
        Terrain {
            cell_size: 1.0,
            detail: layers,
            ..Terrain::default()
        }
    }

    /// An app with everything the rebuild reads and no renderer at all.
    fn detail_app() -> App {
        let mut app = App::new();
        app.add_plugins(bevy::asset::AssetPlugin::default())
            .init_asset::<Mesh>()
            .init_asset::<Image>()
            .init_asset::<AuroraMesh>()
            .init_asset::<AuroraMaterial>()
            .init_resource::<DetailSettings>()
            .init_resource::<SceneWind>()
            .init_resource::<DetailPressers>()
            .init_resource::<DetailMeshes>()
            .add_systems(Startup, init_detail_assets)
            .add_systems(Update, rebuild_detail_tiles);
        app
    }

    fn spawn_terrain(app: &mut App, terrain: &Terrain, data: &RegionTerrainData) -> Entity {
        let source =
            TerrainDetailSource::from_document(data, terrain).expect("the terrain grows detail");
        app.world_mut()
            .spawn((
                source,
                DetailDirty::default(),
                Transform::IDENTITY,
                GlobalTransform::IDENTITY,
            ))
            .id()
    }

    fn spawn_grassy_terrain(app: &mut App) -> Entity {
        spawn_terrain(
            app,
            &terrain(vec![layer("grass", "grass")]),
            &document(64, &["grass"]),
        )
    }

    fn spawn_viewer(app: &mut App, at: Vec3) -> Entity {
        app.world_mut()
            .spawn((
                Camera3d::default(),
                Transform::from_translation(at),
                GlobalTransform::from_translation(at),
            ))
            .id()
    }

    fn tiles(app: &mut App) -> Vec<DetailTile> {
        app.world_mut()
            .query::<&DetailTile>()
            .iter(app.world())
            .cloned()
            .collect()
    }

    /// Enough frames for the per-frame budget to fill the field.
    fn settle(app: &mut App) {
        for _ in 0..400 {
            app.update();
        }
    }

    #[test]
    fn a_terrain_with_no_layers_has_no_source() {
        assert!(
            TerrainDetailSource::from_document(&document(8, &["grass"]), &Terrain::default())
                .is_none()
        );
    }

    #[test]
    fn an_unpainted_density_channel_is_bare_ground() {
        let data = document(8, &[]);
        let source = TerrainDetailSource::from_document(&data, &terrain(vec![layer("g", "grass")]))
            .expect("the terrain still grows detail");
        assert!(
            source.source().layers[0]
                .density
                .iter()
                .all(|value| *value == 0)
        );
    }

    #[test]
    fn a_rebuild_seeds_the_tiles_inside_the_cull_distance_and_no_further() {
        let mut app = detail_app();
        spawn_grassy_terrain(&mut app);
        spawn_viewer(&mut app, Vec3::new(32.0, 5.0, 32.0));
        settle(&mut app);

        let placed = tiles(&mut app);
        assert!(!placed.is_empty(), "the field is seeded");
        let cull = layer("grass", "grass").cull_distance;
        for tile in &placed {
            let min = tile.tile.as_vec2() * DETAIL_TILE_CELLS as f32;
            let nearest = Vec2::splat(32.0).clamp(min, min + Vec2::splat(DETAIL_TILE_CELLS as f32));
            assert!(
                Vec2::splat(32.0).distance(nearest) <= cull + 1.0,
                "tile {} stands inside the cull distance",
                tile.tile
            );
            assert!(!tile.instances.is_empty(), "a seeded tile has instances");
        }
    }

    #[test]
    fn a_seeded_tile_keeps_its_own_bounds_after_the_mesh_bounds_pass() {
        let mut app = detail_app();
        app.add_systems(PostUpdate, bevy::camera::visibility::calculate_bounds);
        spawn_grassy_terrain(&mut app);
        spawn_viewer(&mut app, Vec3::new(32.0, 5.0, 32.0));
        settle(&mut app);

        let mut placed = app.world_mut().query::<(&DetailTile, &Aabb)>();
        let mut seen = 0;
        for (tile, bounds) in placed.iter(app.world()) {
            assert_eq!(*bounds, tile.bounds, "tile {} keeps its bounds", tile.tile);
            seen += 1;
        }
        assert!(seen > 0, "the field is seeded");
    }

    #[test]
    fn tiles_are_seeded_around_the_marked_viewer_when_another_camera_ties_on_order() {
        let mut app = detail_app();
        spawn_terrain(
            &mut app,
            &terrain(vec![layer("grass", "grass")]),
            &document(128, &["grass"]),
        );
        let marked = Vec2::new(100.0, 100.0);
        let first = spawn_viewer(&mut app, Vec3::ZERO);
        let second = spawn_viewer(&mut app, Vec3::ZERO);
        let (viewer, decoy) = (first.min(second), first.max(second));
        let at = Vec3::new(marked.x, 5.0, marked.y);
        app.world_mut().entity_mut(viewer).insert((
            DetailViewer,
            Transform::from_translation(at),
            GlobalTransform::from_translation(at),
        ));
        assert!(
            decoy > viewer,
            "the unmarked camera wins a tie on order by entity"
        );
        settle(&mut app);

        let placed = tiles(&mut app);
        assert!(!placed.is_empty(), "the field is seeded");
        let cull = layer("grass", "grass").cull_distance;
        for tile in &placed {
            let min = tile.tile.as_vec2() * DETAIL_TILE_CELLS as f32;
            let nearest = marked.clamp(min, min + Vec2::splat(DETAIL_TILE_CELLS as f32));
            assert!(
                marked.distance(nearest) <= cull + 1.0,
                "tile {} stands around the marked viewer",
                tile.tile
            );
        }
    }

    #[test]
    fn two_layers_seed_their_own_tiles_from_their_own_channels() {
        let mut app = detail_app();
        let mut data = document(64, &["grass", "flowers"]);
        let painted_corner = 32;
        for z in 0..64 {
            for x in 0..64 {
                if x >= painted_corner || z >= painted_corner {
                    data.regions.set_channel(1, x, z, 0);
                }
            }
        }
        spawn_terrain(
            &mut app,
            &terrain(vec![layer("grass", "grass"), layer("flowers", "flowers")]),
            &data,
        );
        spawn_viewer(&mut app, Vec3::new(32.0, 5.0, 32.0));
        settle(&mut app);

        let placed = tiles(&mut app);
        assert!(placed.iter().any(|tile| tile.layer == 0));
        assert!(placed.iter().any(|tile| tile.layer == 1));
        let grown = |layer: usize| -> usize {
            placed
                .iter()
                .filter(|tile| tile.layer == layer)
                .map(|tile| tile.instances.len())
                .sum()
        };
        assert!(
            grown(1) > 0 && grown(1) < grown(0),
            "the second layer grows only where its own channel is painted: \
             {} against {}",
            grown(1),
            grown(0)
        );
    }

    #[test]
    fn removing_a_layer_retires_its_tiles() {
        let mut app = detail_app();
        let data = document(64, &["grass", "flowers"]);
        let entity = spawn_terrain(
            &mut app,
            &terrain(vec![layer("grass", "grass"), layer("flowers", "flowers")]),
            &data,
        );
        spawn_viewer(&mut app, Vec3::new(32.0, 5.0, 32.0));
        settle(&mut app);
        assert!(tiles(&mut app).iter().any(|tile| tile.layer == 1));

        let trimmed =
            TerrainDetailSource::from_document(&data, &terrain(vec![layer("grass", "grass")]))
                .expect("the terrain still grows detail");
        app.world_mut().entity_mut(entity).insert(trimmed);
        app.update();

        let placed = tiles(&mut app);
        assert!(!placed.is_empty(), "the layer that stayed still draws");
        assert!(
            placed.iter().all(|tile| tile.layer == 0),
            "no tile outlives the layer it grew from"
        );
    }

    #[test]
    fn an_asset_layer_draws_nothing_until_its_mesh_has_resolved() {
        let mut app = detail_app();
        let mut grown = layer("ferns", "grass");
        grown.mesh = DetailMesh::Asset("models/fern.bsn".to_string());
        spawn_terrain(&mut app, &terrain(vec![grown]), &document(64, &["grass"]));
        spawn_viewer(&mut app, Vec3::new(32.0, 5.0, 32.0));
        settle(&mut app);
        assert!(
            tiles(&mut app).is_empty(),
            "a layer whose asset has not loaded draws nothing"
        );

        let mesh = app
            .world_mut()
            .resource_mut::<Assets<Mesh>>()
            .add(card_mesh(1));
        app.world_mut().resource_mut::<DetailMeshes>().0.insert(
            "models/fern.bsn".to_string(),
            BuiltDetailMesh { mesh, color: None },
        );
        settle(&mut app);
        assert!(
            !tiles(&mut app).is_empty(),
            "and draws once the asset has resolved"
        );
    }

    #[test]
    fn an_asset_layer_merges_the_scatter_primitives_into_one_mesh() {
        let mut meshes = Assets::<AuroraMesh>::default();
        let materials = Assets::<AuroraMaterial>::default();
        let mut part = |offset: f32| {
            let mesh = Mesh::new(
                PrimitiveTopology::TriangleList,
                RenderAssetUsages::default(),
            )
            .with_inserted_attribute(
                Mesh::ATTRIBUTE_POSITION,
                vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
            )
            .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, vec![[0.0, 0.0, 1.0]; 3])
            .with_inserted_indices(Indices::U32(vec![0, 1, 2]));
            ScatterPrimitive {
                mesh: meshes.add(AuroraMesh::from_mesh(&mesh).unwrap()),
                material: Handle::default(),
                material_name: None,
                local: Transform::from_xyz(0.0, offset, 0.0),
            }
        };
        let primitives = vec![part(0.0), part(1.0)];

        let (merged, color) =
            merge_primitives("models/fern.bsn", &primitives, &meshes, &materials)
                .expect("both parts are loaded");
        assert!(color.is_none(), "neither part carries a material");
        assert_eq!(merged.count_vertices(), 6);
        assert_eq!(
            merged.indices().expect("the merge is indexed").len(),
            6,
            "both parts keep their own triangle"
        );
        let uvs = merged
            .attribute(Mesh::ATTRIBUTE_UV_0)
            .expect("the merge carries a uv set");
        assert_eq!(uvs.len(), 6, "a part with no uvs still gets one per vertex");

        let Some(bevy::mesh::VertexAttributeValues::Float32(fractions)) =
            merged.attribute(ATTRIBUTE_HEIGHT_FRACTION)
        else {
            panic!("the merge carries a height fraction per vertex");
        };
        assert_eq!(fractions[0], 0.0);
        assert_eq!(
            fractions[2], 0.5,
            "the lower of two stacked units reaches halfway up"
        );
        assert_eq!(fractions[5], 1.0);
    }

    #[test]
    fn a_merged_normal_survives_a_non_uniform_scale() {
        let mut meshes = Assets::<AuroraMesh>::default();
        let materials = Assets::<AuroraMaterial>::default();
        let diagonal = Vec3::new(1.0, 1.0, 0.0).normalize().to_array();
        let leaning = Mesh::new(
            PrimitiveTopology::TriangleList,
            RenderAssetUsages::default(),
        )
        .with_inserted_attribute(
            Mesh::ATTRIBUTE_POSITION,
            vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
        )
        .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, vec![diagonal; 3])
        .with_inserted_indices(Indices::U32(vec![0, 1, 2]));
        let stretched_fourfold_up = Transform::from_scale(Vec3::new(1.0, 4.0, 1.0));
        let primitives = vec![ScatterPrimitive {
            mesh: meshes.add(AuroraMesh::from_mesh(&leaning).unwrap()),
            material: Handle::default(),
            material_name: None,
            local: stretched_fourfold_up,
        }];

        let (merged, _) = merge_primitives("models/leaning.bsn", &primitives, &meshes, &materials)
            .expect("the one part is loaded");
        let normals = merged
            .attribute(Mesh::ATTRIBUTE_NORMAL)
            .expect("the merge carries normals")
            .as_float3()
            .expect("normals are three floats");
        let normal = Vec3::from(normals[0]);
        assert!((normal.length() - 1.0).abs() < 1e-5);
        assert!(
            normal.x > normal.y,
            "the scale shortens the normal's y rather than lengthening it, but it is {normal}"
        );
    }

    #[test]
    fn a_flat_asset_gives_every_vertex_the_same_height_fraction() {
        let mut meshes = Assets::<AuroraMesh>::default();
        let materials = Assets::<AuroraMaterial>::default();
        let flat = Mesh::new(
            PrimitiveTopology::TriangleList,
            RenderAssetUsages::default(),
        )
        .with_inserted_attribute(
            Mesh::ATTRIBUTE_POSITION,
            vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 0.0, 1.0]],
        )
        .with_inserted_indices(Indices::U32(vec![0, 1, 2]));
        let primitives = vec![ScatterPrimitive {
            mesh: meshes.add(AuroraMesh::from_mesh(&flat).unwrap()),
            material: Handle::default(),
            material_name: None,
            local: Transform::IDENTITY,
        }];

        let (merged, _) = merge_primitives("models/flat.bsn", &primitives, &meshes, &materials)
            .expect("the one part is loaded");
        let Some(bevy::mesh::VertexAttributeValues::Float32(fractions)) =
            merged.attribute(ATTRIBUTE_HEIGHT_FRACTION)
        else {
            panic!("the merge carries a height fraction per vertex");
        };
        assert_eq!(
            fractions,
            &vec![0.0; 3],
            "an asset with no height stands flat"
        );
    }

    #[test]
    fn two_layers_over_one_channel_stand_their_instances_apart() {
        let mut app = detail_app();
        spawn_terrain(
            &mut app,
            &terrain(vec![layer("near", "grass"), layer("far", "grass")]),
            &document(64, &["grass"]),
        );
        spawn_viewer(&mut app, Vec3::new(32.0, 5.0, 32.0));
        settle(&mut app);

        let placed = tiles(&mut app);
        let of = |index: usize, at: IVec2| {
            placed
                .iter()
                .find(|tile| tile.layer == index && tile.tile == at)
                .map(|tile| tile.instances.clone())
        };
        let at = placed
            .iter()
            .find(|tile| tile.layer == 0 && !tile.instances.is_empty())
            .expect("the first layer grew somewhere")
            .tile;
        assert_ne!(
            of(0, at),
            of(1, at),
            "two layers sharing a channel cover the same ground without standing in one another"
        );
    }

    #[test]
    fn a_rebuild_clears_the_dirty_mark_it_caught_up_with() {
        let mut app = detail_app();
        let terrain_entity = spawn_grassy_terrain(&mut app);
        spawn_viewer(&mut app, Vec3::new(32.0, 5.0, 32.0));
        settle(&mut app);
        let before = tiles(&mut app).len();

        app.world_mut()
            .get_mut::<DetailDirty>(terrain_entity)
            .expect("the terrain is marked")
            .touch(GridRect::whole(64));
        app.update();

        assert!(
            app.world()
                .get::<DetailDirty>(terrain_entity)
                .expect("the terrain is marked")
                .rect
                .is_none(),
            "the mark is cleared once the tiles it named are retired"
        );
        assert!(
            tiles(&mut app).len() < before,
            "the marked tiles were retired"
        );
        settle(&mut app);
        assert_eq!(tiles(&mut app).len(), before, "and then seeded again");
    }

    #[test]
    fn walking_away_retires_the_tiles_left_behind() {
        let mut app = detail_app();
        spawn_grassy_terrain(&mut app);
        let viewer = spawn_viewer(&mut app, Vec3::new(16.0, 5.0, 16.0));
        settle(&mut app);
        let near_first = tiles(&mut app);
        assert!(!near_first.is_empty());

        let moved = Vec3::new(56.0, 5.0, 56.0);
        *app.world_mut()
            .get_mut::<GlobalTransform>(viewer)
            .expect("the viewer has a transform") = GlobalTransform::from_translation(moved);
        settle(&mut app);

        let after = tiles(&mut app);
        assert!(!after.is_empty());
        let kept: Vec<IVec2> = after.iter().map(|tile| tile.tile).collect();
        assert!(
            near_first.iter().any(|tile| !kept.contains(&tile.tile)),
            "a tile the viewer walked away from was retired"
        );
        for tile in &after {
            let min = tile.tile.as_vec2() * DETAIL_TILE_CELLS as f32;
            let nearest = Vec2::new(56.0, 56.0).clamp(min, min + Vec2::splat(16.0));
            assert!(Vec2::new(56.0, 56.0).distance(nearest) <= 25.0);
        }
    }

    #[test]
    fn bare_ground_is_tiled_once_rather_than_reseeded_every_frame() {
        let mut app = detail_app();
        let mut data = document(64, &["grass"]);
        for z in 0..64 {
            for x in 0..64 {
                data.regions.set_channel(0, x, z, 0);
            }
        }
        spawn_terrain(&mut app, &terrain(vec![layer("grass", "grass")]), &data);
        spawn_viewer(&mut app, Vec3::new(32.0, 5.0, 32.0));
        settle(&mut app);

        let placed = tiles(&mut app);
        assert!(!placed.is_empty(), "the bare ground is tiled");
        assert!(
            placed.iter().all(|tile| tile.instances.is_empty()),
            "and every tile of it draws nothing"
        );
        let settled = placed.len();
        app.update();
        assert_eq!(
            tiles(&mut app).len(),
            settled,
            "a settled field seeds nothing further"
        );
    }

    #[test]
    fn a_terrain_that_stops_growing_detail_retires_its_tiles() {
        let mut app = detail_app();
        let terrain_entity = spawn_grassy_terrain(&mut app);
        spawn_viewer(&mut app, Vec3::new(32.0, 5.0, 32.0));
        settle(&mut app);
        assert!(!tiles(&mut app).is_empty());

        app.world_mut()
            .entity_mut(terrain_entity)
            .remove::<TerrainDetailSource>();
        app.update();
        assert!(
            tiles(&mut app).is_empty(),
            "no tile outlives the terrain it grew from"
        );
    }

    #[test]
    fn a_card_stands_one_unit_tall_from_its_foot() {
        for segments in [1, 3] {
            let mesh = card_mesh(segments);
            let positions = mesh
                .attribute(Mesh::ATTRIBUTE_POSITION)
                .expect("the card has positions")
                .as_float3()
                .expect("positions are three floats");
            assert_eq!(positions.len() as u32, (segments + 1) * 2);
            let lowest = positions.iter().map(|p| p[1]).fold(f32::MAX, f32::min);
            let highest = positions.iter().map(|p| p[1]).fold(f32::MIN, f32::max);
            assert_eq!(lowest, 0.0);
            assert_eq!(highest, 1.0);
            assert_eq!(
                mesh.indices().expect("the card is indexed").len(),
                segments as usize * 6
            );
            let Some(bevy::mesh::VertexAttributeValues::Float32(fractions)) =
                mesh.attribute(ATTRIBUTE_HEIGHT_FRACTION)
            else {
                panic!("the card carries a height fraction per vertex");
            };
            assert_eq!(fractions.first().copied(), Some(0.0));
            assert_eq!(fractions.last().copied(), Some(1.0));
        }
    }

    #[test]
    fn two_edits_before_a_rebuild_cover_both() {
        let mut dirty = DetailDirty::default();
        dirty.touch(GridRect {
            x: 0,
            z: 0,
            width: 4,
            height: 4,
        });
        dirty.touch(GridRect {
            x: 20,
            z: 20,
            width: 4,
            height: 4,
        });
        let rect = dirty.rect.expect("both edits are marked");
        assert_eq!(rect.x, 0);
        assert_eq!(rect.z, 0);
        assert_eq!(rect.width, 24);
        assert_eq!(rect.height, 24);
    }

    #[test]
    fn a_baked_card_stands_at_its_instance_and_tapers_to_a_point() {
        let mut grass = layer("grass", "grass");
        grass.height = [2.0, 2.0];
        grass.width = [0.5, 0.5];
        grass.bend = 0.0;
        let instances = [
            DetailInstance {
                position: Vec3::new(3.0, 1.0, 4.0),
                packed: DetailInstance::pack(0, 0, 0, 255),
                tilt: Vec2::ZERO,
            },
            DetailInstance {
                position: Vec3::new(-2.0, 0.0, 0.0),
                packed: DetailInstance::pack(0, 0, 0, 255),
                tilt: Vec2::ZERO,
            },
        ];
        let card = card_mesh(2);
        let baked = bake_tile(&card, &instances, &grass).expect("two instances bake");
        assert_eq!(baked.count_vertices(), card.count_vertices() * 2);
        let positions = baked
            .attribute(Mesh::ATTRIBUTE_POSITION)
            .and_then(|values| values.as_float3())
            .expect("positions");
        assert_eq!(positions[0], [3.0 - 0.25, 1.0, 4.0], "the foot is full width");
        let tip = positions[card.count_vertices() - 1];
        assert!((tip[0] - 3.0).abs() < 1e-5, "the tip tapers to the stem");
        assert!((tip[1] - 3.0).abs() < 1e-5, "the tip stands the layer's height up");
        assert!(bake_tile(&card, &[], &grass).is_none(), "an empty tile bakes nothing");
    }
}
