//! `JackdawPlugin` reads the asset files under the asset folder at `Startup` and exposes them
//! through `JackdawCatalog`, keyed by the path each file sits at and by the `@stem` older
//! references spell.

mod support;

use std::path::Path;

use bevy::asset::{Asset, AssetApp};
use bevy::prelude::*;
use bevy::reflect::TypePath;
use jackdaw_runtime::JackdawCatalog;
use jackdaw_runtime::bsn_files::with_asset_header;

/// A reflectable stand-in for a material, so the walk has an asset type to load without the
/// render stack.
#[derive(Asset, Reflect, Default)]
#[reflect(Default)]
struct CatalogMaterial {
    tint: f32,
}

fn catalog_app(dir: &Path) -> App {
    let mut app = support::base_app(dir);
    app.add_plugins(bevy::image::ImagePlugin::default());
    app.init_asset::<CatalogMaterial>();
    app.register_asset_reflect::<CatalogMaterial>();
    app.add_plugins(jackdaw_runtime::JackdawPlugin);
    // `Startup` walks the folder.
    app.update();
    app
}

fn write(dir: &Path, path: &str, text: &str) {
    let file = dir.join(path);
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(file, text).unwrap();
}

#[test]
fn an_asset_file_answers_to_its_path_and_its_stem() {
    let type_path = <CatalogMaterial as TypePath>::type_path();
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "materials/brick.bsn",
        &format!("#brick\n{type_path} {{ tint: 0.5 }}\n"),
    );

    let app = catalog_app(dir.path());

    let catalog = app.world().resource::<JackdawCatalog>();
    let handle = catalog
        .get("materials/brick.bsn")
        .expect("keyed by its path");
    assert_eq!(catalog.get("@brick").map(|h| h.id()), Some(handle.id()));
    let tint = app
        .world()
        .resource::<Assets<CatalogMaterial>>()
        .get(handle.id().typed::<CatalogMaterial>())
        .map(|m| m.tint);
    assert_eq!(tint, Some(0.5), "the value is the one the file holds");
}

#[test]
fn a_missing_asset_folder_leaves_the_catalog_empty() {
    let dir = tempfile::tempdir().unwrap();
    let app = catalog_app(&dir.path().join("nowhere"));

    assert!(app.world().resource::<JackdawCatalog>().is_empty());
}

/// The stem counts over every document the walk saw, scenes included, so a name the editor
/// calls ambiguous is ambiguous here too.
#[test]
fn a_name_a_scene_and_an_asset_share_stands_for_neither() {
    let type_path = <CatalogMaterial as TypePath>::type_path();
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "materials/grass.bsn",
        &format!("#grass\n{type_path}\n"),
    );
    write(
        dir.path(),
        "zones/grass.bsn",
        "#Root\nbevy_transform::components::transform::Transform\n\
         Children [\n    bevy_transform::components::transform::Transform\n]\n",
    );

    let app = catalog_app(dir.path());

    let catalog = app.world().resource::<JackdawCatalog>();
    assert!(
        catalog.get("@grass").is_none(),
        "a shared name stands for neither file"
    );
    assert!(
        catalog.get("materials/grass.bsn").is_some(),
        "the file is still named by its path"
    );
    assert!(
        catalog.get("zones/grass.bsn").is_none(),
        "a scene is no asset"
    );
}

/// The header is what a file says it holds when its root says nothing: a type this app does not
/// register is skipped.
#[test]
fn a_file_whose_type_this_app_does_not_load_is_skipped() {
    let type_path = <CatalogMaterial as TypePath>::type_path();
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "content/item.bsn",
        &with_asset_header(
            "my_game::content::ItemDef",
            "#item\nmy_game::content::ItemDef\n",
        ),
    );
    write(
        dir.path(),
        "content/slate.bsn",
        &format!("#slate\n{type_path}\n"),
    );

    let app = catalog_app(dir.path());

    let catalog = app.world().resource::<JackdawCatalog>();
    assert!(catalog.get("content/item.bsn").is_none());
    assert!(
        catalog.get("content/slate.bsn").is_some(),
        "a file with no header is known by its root"
    );
}
