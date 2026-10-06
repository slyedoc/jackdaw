//! The navmesh baked beside a scene, loaded with that scene.
//!
//! A bake is saved as `<scene>.jdnav` next to `<scene>.bsn`, and arrives as a
//! [`JackdawNavmesh`] component on the scene root, so two scenes loaded at
//! once do not share one. Its queries, [`NavmeshArtifact::contains_point`] and
//! [`NavmeshArtifact::height_at`], come from `jackdaw_terrain`, so a server
//! can validate moves against the baked artifact without the `terrain`
//! feature's mesher and shader.
//!
//! A missing file is the unbaked scene and is not reported. A file that does
//! not decode is reported and left on disk.
//!
//! A scene reloads onto the root it already spawned from, so the component is
//! removed before the file is read again: a reload that finds the bake deleted
//! or broken leaves the root bare rather than answering moves from ground the
//! world no longer has.

use bevy::prelude::*;
use bevy::scene::ScenePatchInstance;
use jackdaw_terrain::navmesh::{self, NavmeshArtifact};

/// The navmesh baked for a scene, on that scene's root entity.
///
/// Dereferences to the artifact, so a game asks it directly:
///
/// ```ignore
/// fn can_step_to(point: Vec2, nav: Single<&JackdawNavmesh>) -> bool {
///     nav.contains_point(point)
/// }
/// ```
#[derive(Component, Debug, Deref)]
pub struct JackdawNavmesh(pub NavmeshArtifact);

/// Read the navmesh baked beside each scene instance as it spawns.
pub(crate) fn attach_navmeshes(
    mut commands: Commands,
    added: Query<(Entity, &ScenePatchInstance), Changed<ScenePatchInstance>>,
    folder: Res<crate::AssetFolder>,
) {
    for (entity, instance) in &added {
        commands.entity(entity).remove::<JackdawNavmesh>();
        let (Some(path), Some(assets)) = (instance.0.path(), folder.0.as_ref()) else {
            continue;
        };
        let scene = assets.join(path.path());
        let Some(stem) = scene.file_name().and_then(|n| n.to_str()).map(crate::bsn_files::asset_stem) else {
            continue;
        };
        let path = scene.with_file_name(format!("{stem}.{}", navmesh::EXTENSION));
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        match navmesh::decode(&bytes) {
            Ok(artifact) => {
                commands.entity(entity).insert(JackdawNavmesh(artifact));
            }
            Err(err) => error!(
                "navmesh {} is unreadable ({err}); this scene loads without one",
                path.display()
            ),
        }
    }
}
