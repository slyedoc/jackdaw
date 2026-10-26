//! A dedicated server compiles with the `render` feature (for the scene types)
//! but adds none of the rendering plugins. Under `render`, a brush face's
//! `material` is a `Handle<StandardMaterial>`, and an unassigned face serializes
//! as `material: null`. The runtime must still load such a brush headless:
//!
//! 1. `JackdawPlugin` registers `StandardMaterial`/`Image` asset reflection, so
//!    the deserializer's handle processor turns `null` into a default handle
//!    instead of failing (which would drop the whole brush).
//! 2. The `On<Insert<Brush>>` mesh-rebuild observer no-ops when the mesh/material
//!    asset stores are absent, so loading the brush does not panic on the missing
//!    `Assets<AuroraMesh>`.
//!
//! Run: `cargo test -p jackdaw_runtime --test headless_render_brush_load`
#![cfg(feature = "render")]

mod support;

use bevy::prelude::*;
use bevy::reflect::TypePath;
use jackdaw_runtime::JackdawPlugin;

/// One face with an UNASSIGNED material: the `material` field is omitted so it
/// reflects to the default `Handle<StandardMaterial>`, exactly as a face that
/// has no material applied. Only the plane geometry is authored.
fn unassigned_face(normal: [f32; 3], distance: f32) -> String {
    format!(
        "jackdaw_geometry::BrushFaceData {{ plane: jackdaw_geometry::BrushPlane {{ \
normal: glam::Vec3 {{ x: {:?}, y: {:?}, z: {:?} }}, distance: {:?} }} }}",
        normal[0] as f64, normal[1] as f64, normal[2] as f64, distance as f64,
    )
}

#[test]
fn null_material_brush_loads_headless_under_render_feature() {
    // `render` feature is ON, but NO rendering plugins are added: the dedicated-server shape.
    let dir = tempfile::tempdir().unwrap();
    let mut app = support::game(dir.path());

    let brush_type_path = <jackdaw_scene_types::Brush as TypePath>::type_path();

    let faces = [
        unassigned_face([1.0, 0.0, 0.0], 1.0),
        unassigned_face([-1.0, 0.0, 0.0], 1.0),
        unassigned_face([0.0, 1.0, 0.0], 1.0),
        unassigned_face([0.0, -1.0, 0.0], 1.0),
        unassigned_face([0.0, 0.0, 1.0], 1.0),
        unassigned_face([0.0, 0.0, -1.0], 1.0),
    ]
    .join(",\n        ");
    let scene = format!("{brush_type_path} {{\n    faces: [\n        {faces},\n    ],\n}}\n");

    // Neither the load nor the insert observer may panic on the absent render asset stores.
    support::spawn_text(&mut app, dir.path(), "brush.bsn", &scene);

    let mut brush_query = app.world_mut().query::<&jackdaw_scene_types::Brush>();
    let brushes: Vec<_> = brush_query.iter(app.world()).collect();
    assert_eq!(
        brushes.len(),
        1,
        "a brush with null face materials must survive a headless render-feature load",
    );
    assert_eq!(brushes[0].faces.len(), 6, "all six faces must load");
}

/// When the rendering plugins are already present (a windowed client, the editor)
/// they own the `AuroraMaterial` asset machinery. Re-running `init_asset` over it
/// orphans existing handles and corrupts the asset storage, which is what crashed
/// the client. The plugin must detect the existing registration and leave it alone.
#[test]
fn existing_material_storage_is_left_intact() {
    use aurora::material::AuroraMaterial;

    let mut app = App::new();
    app.add_plugins(MinimalPlugins);
    app.add_plugins(bevy::asset::AssetPlugin::default());
    app.add_plugins(bevy::world_serialization::WorldSerializationPlugin);

    // Stand in for the render plugins owning the material asset, and mint a handle
    // against that storage before the jackdaw plugins are added.
    app.init_asset::<AuroraMaterial>();
    let handle = app
        .world_mut()
        .resource_mut::<Assets<AuroraMaterial>>()
        .add(AuroraMaterial::default());

    app.add_plugins(JackdawPlugin);

    assert!(
        app.world()
            .resource::<Assets<AuroraMaterial>>()
            .get(&handle)
            .is_some(),
        "the pre-existing material storage must survive plugin init, not be re-registered",
    );
}
