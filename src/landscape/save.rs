//! Brush layers' pages outside the scene: a store that survives snapshot undo, and the
//! `.sculpt` / `.paint` files written when the scene saves.
//!
//! A brush layer's pages are GPU state, not a component, so neither the scene text nor an
//! undo snapshot carries them. Every stroke copies its layer into [`LandscapePageStore`] by
//! the layer's `SceneNodeId`; a layer entity that reappears empty (an undo re-minted it)
//! takes them back from there. Saving writes each layer's pages to a file next to the scene
//! and points the layer's `file` handle at it, so the scene names its files and loading
//! brings them back through the asset server.

use std::path::Path;

use aurora_landscape::{
    BakedPages, ErosionLayer, HeightLayer, Landscape, LandscapeBake, LandscapePages, MaterialLayer,
    PaintLayer, PaintPages, SculptLayer, SculptPages,
    sculpt::{LayerPagesFile, LoadedPagesFile},
};
use bevy::{platform::collections::HashMap, prelude::*};
use aurora::render_device::RenderDevice;
use jackdaw_scene_types::SceneNodeId;

use crate::scene_io::SceneEntity;

/// Each brush layer's pages as of its last stroke, by scene node.
#[derive(Resource, Default)]
pub(crate) struct LandscapePageStore {
    by_node: HashMap<SceneNodeId, Vec<u8>>,
}

/// The layer's pages, into the store.
pub(super) fn store_layer(world: &mut World, layer: Entity) {
    let Some(&node) = world.get::<SceneNodeId>(layer) else {
        return;
    };
    let bytes = if let Some(pages) = world.get::<SculptPages>(layer) {
        pages.to_bytes()
    } else if let Some(pages) = world.get::<PaintPages>(layer) {
        pages.to_bytes()
    } else {
        return;
    };
    world
        .resource_mut::<LandscapePageStore>()
        .by_node
        .insert(node, bytes);
}

/// The store has been consulted for this entity.
#[derive(Component)]
pub(super) struct StoreChecked;

/// A brush layer that appears (loaded, or re-minted by an undo) takes its pages from the store
/// when the store has them: they are newer than any file.
#[allow(clippy::type_complexity)]
pub(super) fn restore_from_store(
    mut commands: Commands,
    rd: Option<Res<RenderDevice>>,
    store: Res<LandscapePageStore>,
    mut sculpts: Query<
        (
            Entity,
            &SceneNodeId,
            &SculptLayer,
            &mut SculptPages,
            &mut HeightLayer,
        ),
        Without<StoreChecked>,
    >,
    mut paints: Query<
        (
            Entity,
            &SceneNodeId,
            &PaintLayer,
            &mut PaintPages,
            &mut MaterialLayer,
        ),
        (Without<StoreChecked>, Without<SculptLayer>),
    >,
) {
    let Some(rd) = rd else {
        return;
    };
    for (entity, node, layer, mut pages, mut stack) in &mut sculpts {
        commands.entity(entity).insert(StoreChecked);
        if let Some(bytes) = store.by_node.get(node)
            && pages.load(&rd, bytes).is_ok()
        {
            stack.set_changed();
            // Keep the (older) file from loading over it.
            commands
                .entity(entity)
                .insert(LoadedPagesFile(layer.file.id()));
        }
    }
    for (entity, node, layer, mut pages, mut stack) in &mut paints {
        commands.entity(entity).insert(StoreChecked);
        if let Some(bytes) = store.by_node.get(node)
            && pages.load(&rd, bytes).is_ok()
        {
            stack.set_changed();
            commands
                .entity(entity)
                .insert(LoadedPagesFile(layer.file.id()));
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PagesKind {
    Sculpt,
    Paint,
    Erosion,
}

impl PagesKind {
    fn ext(self) -> &'static str {
        match self {
            Self::Sculpt => "sculpt",
            Self::Paint => "paint",
            Self::Erosion => "erosion",
        }
    }

    fn file(self, world: &World, layer: Entity) -> Option<&Handle<LayerPagesFile>> {
        match self {
            Self::Sculpt => world.get::<SculptLayer>(layer).map(|l| &l.file),
            Self::Paint => world.get::<PaintLayer>(layer).map(|l| &l.file),
            Self::Erosion => world.get::<ErosionLayer>(layer).map(|l| &l.file),
        }
    }

    fn set_file(self, world: &mut World, layer: Entity, handle: Handle<LayerPagesFile>) {
        match self {
            Self::Sculpt => {
                if let Some(mut l) = world.get_mut::<SculptLayer>(layer) {
                    l.file = handle;
                }
            }
            Self::Paint => {
                if let Some(mut l) = world.get_mut::<PaintLayer>(layer) {
                    l.file = handle;
                }
            }
            Self::Erosion => {
                if let Some(mut l) = world.get_mut::<ErosionLayer>(layer) {
                    l.file = handle;
                }
            }
        }
    }

    /// The layer's pages as file bytes, and whether it has none.
    fn bytes(self, world: &World, layer: Entity) -> Option<(Vec<u8>, bool)> {
        if self == Self::Paint {
            let pages = world.get::<PaintPages>(layer)?;
            Some((pages.to_bytes(), pages.pages().next().is_none()))
        } else {
            let pages = world.get::<SculptPages>(layer)?;
            Some((pages.to_bytes(), pages.pages().next().is_none()))
        }
    }
}

/// Where a file beside the scene goes: its asset path (reusing `named` once set) and its
/// path on disk.
fn beside_scene(
    assets: &Path,
    relative_dir: &Path,
    stem: &str,
    named: Option<&Handle<impl Asset>>,
    node: u64,
    ext: &str,
) -> (String, std::path::PathBuf) {
    let asset_path = match named.and_then(|h| h.path()) {
        Some(path) => path.path().to_string_lossy().replace('\\', "/"),
        None => relative_dir
            .join(format!("{stem}.landscape-{node:x}.{ext}"))
            .to_string_lossy()
            .replace('\\', "/"),
    };
    let file = assets.join(&asset_path);
    (asset_path, file)
}

/// Before the scene text is written: every brush or erosion layer with pages gets a file
/// beside the scene (`<scene>.landscape-<node>.sculpt` / `.paint` / `.erosion`, reused once
/// named), and every landscape its evaluated pages (`.pages`); each `file` handle points at
/// its file, so the scene names it.
pub(crate) fn export_landscape_pages(world: &mut World, scene_path: &Path) -> std::io::Result<()> {
    let Some(assets) = crate::asset_index::assets_dir(world) else {
        return Ok(());
    };
    let Some(scene_dir) = scene_path.parent() else {
        return Ok(());
    };
    let stem = scene_path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "untitled".into());
    // Asset paths are relative to the project's assets folder.
    let Ok(relative_dir) = scene_dir.strip_prefix(&assets).map(Path::to_path_buf) else {
        warn!(
            "landscape: {} is outside the assets folder; landscapes not saved",
            scene_path.display()
        );
        return Ok(());
    };
    let node_of = |world: &World, e: Entity| world.get::<SceneNodeId>(e).map_or(0, |n| n.0);

    let mut layers: Vec<(Entity, PagesKind)> = Vec::new();
    for (kind, entities) in [
        (
            PagesKind::Sculpt,
            world
                .query_filtered::<Entity, (With<SculptLayer>, With<SceneEntity>)>()
                .iter(world)
                .collect::<Vec<_>>(),
        ),
        (
            PagesKind::Paint,
            world
                .query_filtered::<Entity, (With<PaintLayer>, With<SceneEntity>)>()
                .iter(world)
                .collect(),
        ),
        (
            PagesKind::Erosion,
            world
                .query_filtered::<Entity, (With<ErosionLayer>, With<SceneEntity>)>()
                .iter(world)
                .collect(),
        ),
    ] {
        layers.extend(entities.into_iter().map(|e| (e, kind)));
    }
    for (layer, kind) in layers {
        let Some((bytes, empty)) = kind.bytes(world, layer) else {
            continue;
        };
        let named = kind
            .file(world, layer)
            .filter(|h| h.path().is_some())
            .cloned();
        if empty && named.is_none() {
            continue;
        }
        let (asset_path, file) = beside_scene(
            &assets,
            &relative_dir,
            &stem,
            named.as_ref(),
            node_of(world, layer),
            kind.ext(),
        );
        write_beside(&file, &bytes)?;
        let handle = world
            .resource::<AssetServer>()
            .load::<LayerPagesFile>(asset_path);
        let id = handle.id();
        kind.set_file(world, layer, handle);
        // The pages are already what the file holds.
        world.entity_mut(layer).insert(LoadedPagesFile(id));
    }

    let landscapes: Vec<Entity> = world
        .query_filtered::<Entity, (With<Landscape>, With<SceneEntity>)>()
        .iter(world)
        .collect();
    for landscape in landscapes {
        let Some(pages) = world.get::<LandscapePages>(landscape) else {
            continue;
        };
        let Some(bake) = pages.bake() else {
            warn!("landscape {landscape}: pages still evaluating; its bake was not written");
            continue;
        };
        let named = world
            .get::<LandscapeBake>(landscape)
            .map(|b| b.file.clone())
            .filter(|h| h.path().is_some());
        let (asset_path, file) = beside_scene(
            &assets,
            &relative_dir,
            &stem,
            named.as_ref(),
            node_of(world, landscape),
            "pages",
        );
        write_beside(&file, &bake.to_bytes())?;
        let handle = world
            .resource::<AssetServer>()
            .load::<BakedPages>(asset_path);
        if let Some(mut pages) = world.get_mut::<LandscapePages>(landscape) {
            pages.adopt_bake(handle.id());
        }
        world
            .entity_mut(landscape)
            .insert(LandscapeBake { file: handle });
        crate::scene_io::author(world, landscape, std::any::TypeId::of::<LandscapeBake>());
    }
    Ok(())
}

fn write_beside(file: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir)?;
    }
    crate::scene_io::save::write_atomic(file, bytes)
}
