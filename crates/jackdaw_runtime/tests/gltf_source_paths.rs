//! An authored `GltfSource` path names a file under the assets root, not one
//! beside the scene that wrote it, and a source game code inserts later is read
//! the same way.
#![cfg(feature = "render")]

mod support;

use bevy::prelude::*;
use bevy::world_serialization::WorldAssetRoot;
use jackdaw_scene_types::GltfSource;

#[test]
fn a_prefab_in_a_subdirectory_loads_its_gltf_from_the_assets_root() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = support::game(dir.path());
    support::spawn_text(
        &mut app,
        dir.path(),
        "prefabs/fox.bsn",
        "#Fox\n\
         jackdaw_scene_types::types::GltfSource { path: \"characters/fox.gltf\", scene_index: 0 }\n",
    );

    assert_eq!(
        loaded_gltf_path(app.world_mut()).as_deref(),
        Some("characters/fox.gltf#Scene0"),
        "the scene's own directory must not be prepended to the authored path"
    );
}

#[test]
fn a_gltf_source_inserted_at_runtime_spawns_its_model() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = support::game(dir.path());

    app.world_mut().spawn(GltfSource {
        path: "characters/fox.gltf".to_owned(),
        scene_index: 2,
    });
    app.update();

    assert_eq!(
        loaded_gltf_path(app.world_mut()).as_deref(),
        Some("characters/fox.gltf#Scene2"),
        "a source game code inserted gets the same model an authored one gets"
    );
}

/// The glTF the one `GltfSource` entity points the world-asset spawner at.
fn loaded_gltf_path(world: &mut World) -> Option<String> {
    let mut sources = world.query::<(&GltfSource, &WorldAssetRoot)>();
    let (_, root) = sources.iter(world).next()?;
    Some(root.0.path()?.to_string())
}
