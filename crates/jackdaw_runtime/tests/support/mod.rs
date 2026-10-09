//! A headless game over an asset folder: bevy's `.bsn` loader plus `JackdawPlugin`.
#![allow(dead_code)]

use std::path::Path;

use bevy::asset::LoadState;
use bevy::bsn_asset::BsnAssetPlugin;
use bevy::prelude::*;
use bevy::scene::{SceneInstanceState, ScenePatch, ScenePatchInstance, ScenePlugin};

/// The plugins a scene needs to load, reading assets from `dir`; `JackdawPlugin` is the caller's.
pub fn base_app(dir: &Path) -> App {
    let mut app = App::new();
    app.add_plugins(MinimalPlugins);
    app.add_plugins(bevy::transform::TransformPlugin);
    app.add_plugins(AssetPlugin {
        file_path: dir.to_string_lossy().into_owned(),
        ..default()
    });
    app.add_plugins(bevy::world_serialization::WorldSerializationPlugin);
    app.add_plugins((ScenePlugin, BsnAssetPlugin));
    app
}

/// [`base_app`] with `JackdawPlugin`.
pub fn game(dir: &Path) -> App {
    let mut app = base_app(dir);
    app.add_plugins(jackdaw_runtime::JackdawPlugin);
    app
}

/// Spawn `path` as a `ScenePatchInstance` and pump until it has been applied.
pub fn spawn_scene(app: &mut App, path: &str) -> Entity {
    let handle: Handle<ScenePatch> = app.world().resource::<AssetServer>().load(path.to_owned());
    let entity = app
        .world_mut()
        .spawn(ScenePatchInstance(handle.clone()))
        .id();
    for frame in 0..2000 {
        app.update();
        if app
            .world()
            .get::<SceneInstanceState>(entity)
            .is_some_and(|state| state.applied)
        {
            app.update();
            return entity;
        }
        if let Some(LoadState::Failed(err)) = app
            .world()
            .resource::<AssetServer>()
            .get_load_state(&handle)
        {
            panic!("{path} failed to load: {err}");
        }
        if frame > 50 {
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }
    panic!("{path} never spawned");
}

/// Write `text` to `dir/path` and spawn it.
pub fn spawn_text(app: &mut App, dir: &Path, path: &str, text: &str) -> Entity {
    let file = dir.join(path);
    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(file, text).unwrap();
    spawn_scene(app, path)
}

pub fn named(world: &mut World, target: &str) -> Entity {
    try_named(world, target).unwrap_or_else(|| panic!("no entity named {target}"))
}

pub fn try_named(world: &mut World, target: &str) -> Option<Entity> {
    let mut names = world.query::<(Entity, &Name)>();
    names
        .iter(world)
        .find_map(|(entity, name)| (name.as_str() == target).then_some(entity))
}
