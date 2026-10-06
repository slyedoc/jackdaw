//! Bridge between editor collider configuration and avian `Collider` components.
//!
//! New brushes spawn with [`RigidBody::Static`] and [`AvianCollider`]. This
//! module builds the runtime `Collider` from that wrapper  -- handling both
//! mesh-backed entities and brushes (which have `BrushMeshCache` instead of
//! `AuroraMesh3d`).
//!
//! `ColliderConstructor` is never placed on entities, so avian's
//! `init_collider_constructors` system never fires and can't interfere.

use avian3d::prelude::*;
use bevy::prelude::*;
use bevy_aurora::mesh::{AuroraMesh, AuroraMesh3d};
use jackdaw_avian_integration::AvianCollider;
use jackdaw_geometry::{is_convex_topology, triangulate_polygons};

use crate::brush::{Brush, BrushMeshCache};

pub struct PhysicsBrushBridgePlugin;

impl Plugin for PhysicsBrushBridgePlugin {
    fn build(&self, app: &mut App) {
        app.add_observer(remove_collider_when_avian_collider_removed);
    }
}

/// Insert the default authored physics pair on a newly spawned brush.
pub(crate) fn insert_default_brush_physics(world: &mut World, entity: Entity) {
    let Ok(entity_ref) = world.get_entity(entity) else {
        return;
    };
    if entity_ref.contains::<RigidBody>() || entity_ref.contains::<AvianCollider>() {
        return;
    }

    if let Ok(mut entity_mut) = world.get_entity_mut(entity) {
        entity_mut
            .insert(AvianCollider::default())
            .insert(RigidBody::Static);
    }

}

/// Copy a recentered brush `Transform` into avian `Position` / `Rotation`.
///
/// Avian treats `Position` as the physics pose. Recenter writes `Transform`
/// in `Update`, and without this copy a collider insert or
/// `position_to_transform` writeback can snap the entity back to the
/// pre-recenter pose for a frame.
pub(crate) fn sync_avian_position_from_brush_transform(
    mut helper: PhysicsTransformHelper,
    changed: Query<Entity, (With<Brush>, With<AvianCollider>, Changed<Transform>)>,
) {
    for entity in &changed {
        let _ = helper.update_physics_transform(entity);
    }
}

/// When the user-facing `AvianCollider` is removed (e.g. physics toggle off,
/// or undo of enable-physics), also remove the runtime `Collider` we built
/// from it. Without this, the collider gizmo keeps being drawn after undo.
fn remove_collider_when_avian_collider_removed(
    trigger: On<Remove<AvianCollider>>,
    mut commands: Commands,
) {
    let entity = trigger.event_target();
    if let Ok(mut ec) = commands.get_entity(entity) {
        ec.try_remove::<Collider>();
    }
}

/// When `AvianCollider` is added/changed, or the underlying brush
/// geometry rebuilds, build a `Collider` from the inner
/// `ColliderConstructor` and insert it directly. Watching
/// `Changed<BrushMeshCache>` is what makes the collider track face
/// drags / vertex edits: extending a brush updates `BrushMeshCache`,
/// which fires this system, which rebuilds the trimesh collider so
/// the green wireframe matches the new geometry. Handles both
/// mesh-backed entities (reads from `AuroraMesh3d`) and brushes (reads
/// from `BrushMeshCache`).
pub(crate) fn sync_editor_collider_config(
    mut commands: Commands,
    changed: Query<
        (
            Entity,
            &AvianCollider,
            Option<&BrushMeshCache>,
            Option<&AuroraMesh3d>,
        ),
        Or<(Changed<AvianCollider>, Changed<BrushMeshCache>)>,
    >,
    brushes: Query<&Brush>,
    meshes: Res<Assets<AuroraMesh>>,
) {
    for (entity, config, brush_cache, mesh3d) in &changed {
        let constructor = if let Ok(brush) = brushes.get(entity) {
            // CONVEX_FUNCTIONAL: different behavior is intentional (collider type)
            if !is_convex_topology(&brush.topology) {
                // Force TriMesh for non-convex brushes; ConvexHull/AABB would mis-simulate.
                ColliderConstructor::TrimeshFromMesh
            } else {
                config.0.clone()
            }
        } else {
            config.0.clone()
        };

        let collider = if constructor.requires_mesh() {
            // Try brush geometry first, then mesh asset
            if let Some(brush_cache) = brush_cache {
                let Some((positions, triangles)) = brush_triangles(brush_cache) else {
                    continue;
                };
                collider_from_triangles(&constructor, positions, triangles)
            } else if let Some(mesh3d) = mesh3d {
                let Some(mesh) = meshes.get(&mesh3d.0) else {
                    continue;
                };
                let flat = mesh.flatten();
                let triangles = flat
                    .indices
                    .chunks_exact(3)
                    .map(|t| [t[0], t[1], t[2]])
                    .collect();
                collider_from_triangles(&constructor, flat.positions, triangles)
            } else {
                continue;
            }
        } else {
            Collider::try_from_constructor(constructor.clone(), None)
        };

        if let Some(collider) = collider {
            commands.entity(entity).insert(collider);
        }
    }
}

/// A brush's triangulated geometry from its `BrushMeshCache`.
fn brush_triangles(cache: &BrushMeshCache) -> Option<(Vec<Vec3>, Vec<[u32; 3]>)> {
    if cache.vertices.is_empty() {
        return None;
    }
    let tris = triangulate_polygons(
        &cache.vertices,
        &cache.face_polygons,
        cache.face_normals.iter().copied(),
    );
    if tris.is_empty() {
        return None;
    }
    Some((cache.vertices.clone(), tris))
}

/// The collider a mesh-shaped `constructor` builds from triangle-list geometry.
fn collider_from_triangles(
    constructor: &ColliderConstructor,
    positions: Vec<Vec3>,
    triangles: Vec<[u32; 3]>,
) -> Option<Collider> {
    match constructor {
        ColliderConstructor::TrimeshFromMesh => Collider::try_trimesh(positions, triangles).ok(),
        ColliderConstructor::TrimeshFromMeshWithConfig(flags) => {
            Collider::try_trimesh_with_config(positions, triangles, *flags).ok()
        }
        ColliderConstructor::ConvexDecompositionFromMesh => {
            Some(Collider::convex_decomposition(positions, triangles))
        }
        ColliderConstructor::ConvexDecompositionFromMeshWithConfig(params) => Some(
            Collider::convex_decomposition_with_config(positions, triangles, params.clone()),
        ),
        ColliderConstructor::ConvexHullFromMesh => Collider::convex_hull(positions),
        ColliderConstructor::VoxelizedTrimeshFromMesh {
            voxel_size,
            fill_mode,
        } => Some(Collider::voxelized_trimesh(
            &positions,
            &triangles,
            *voxel_size,
            *fill_mode,
        )),
        other => Collider::try_from_constructor(other.clone(), None),
    }
}
