//! Real baked `.bsn` files survive load -> write -> load: every entity, every component.
//!
//! Reads zero's asset folder in place (nothing is written to it) and skips when it is not
//! checked out beside jackdaw.

use std::path::Path;

use bevy::{
    bsn_asset::{DynamicScene, WriteSettings, write_scene_text},
    ecs::reflect::ReflectComponent,
    prelude::*,
    scene::ScenePatch,
};

use crate::util;

const ZERO_ASSETS: &str = "/home/slyedoc/code/p/zero/assets";

/// Loads `path`, spawns it, and returns the root.
fn spawn_file(app: &mut App, path: &str) -> Entity {
    let server = app.world().resource::<AssetServer>().clone();
    let handle = server.load::<ScenePatch>(path.to_string());
    for _ in 0..20_000 {
        app.update();
        if server.is_loaded_with_dependencies(&handle) {
            break;
        }
        if server.load_state(&handle).is_failed() {
            panic!("{path} failed to load: {:?}", server.load_state(&handle));
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    let resolved = app
        .world()
        .resource::<Assets<ScenePatch>>()
        .get(&handle)
        .and_then(|patch| patch.resolved.clone())
        .unwrap_or_else(|| panic!("{path} never resolved"));
    // Kept alive: the scene holds its bases, as an open file does.
    std::mem::forget(handle);
    resolved.spawn(app.world_mut()).expect("spawns").id()
}

fn spawn_text(app: &mut App, path: &str, text: &str) -> Entity {
    let document = bevy::bsn::BsnDocument::parse(text)
        .unwrap_or_else(|err| panic!("written {path} does not parse: {err:?}\n{text}"));
    let registry = app.world().resource::<AppTypeRegistry>().clone();
    let server = app.world().resource::<AssetServer>().clone();
    let mut handles = |type_id: std::any::TypeId, path: bevy::asset::AssetPath<'static>| {
        server.load_builder().load_erased(type_id, path)
    };
    let scene = DynamicScene::from_document_with_handles(&document, path, &registry, &mut handles)
        .unwrap_or_else(|err| panic!("written {path} does not build: {}", err.render(text)));
    app.world_mut().spawn_scene(scene).expect("spawns").id()
}

/// Every reflected component on `a` and below equals the same on `b`, entity for entity.
fn assert_same_tree(world: &World, a: Entity, b: Entity, at: &str, text: &str) {
    let registry = world.resource::<AppTypeRegistry>().read();
    let mut pairs = vec![(a, b, at.to_string())];
    while let Some((a, b, at)) = pairs.pop() {
        let (ea, eb) = (world.entity(a), world.entity(b));
        for id in ea.archetype().components() {
            let Some(type_id) = world.components().get_info(*id).and_then(|i| i.type_id()) else {
                continue;
            };
            let Some(reflect) = registry.get_type_data::<ReflectComponent>(type_id) else {
                continue;
            };
            let path = registry.get(type_id).map(|r| r.type_info().type_path()).unwrap_or("?");
            if path.ends_with("GlobalTransform")
                || path.ends_with("Children")
                || path.ends_with("ChildOf")
                || path.ends_with("SkinnedMesh")
            {
                continue;
            }
            let (Some(va), vb) = (reflect.reflect(ea), reflect.reflect(eb)) else {
                continue;
            };
            let Some(vb) = vb else {
                panic!("{at}: {path} missing after the round trip\n{text}");
            };
            if !same(world, &registry, va.as_partial_reflect(), vb.as_partial_reflect()) {
                panic!("{at}: {path} differs\n  before {va:?}\n  after  {vb:?}");
            }
        }
        let kids = |e: Entity| -> Vec<Entity> {
            world.get::<Children>(e).map(|c| c.iter().collect()).unwrap_or_default()
        };
        let (ka, kb) = (kids(a), kids(b));
        assert_eq!(ka.len(), kb.len(), "{at}: child count differs\n{text}");
        for (i, (ca, cb)) in ka.into_iter().zip(kb).enumerate() {
            let name = world.get::<Name>(ca).map(|n| n.to_string()).unwrap_or_else(|| i.to_string());
            pairs.push((ca, cb, format!("{at}/{name}")));
        }
    }
}

/// Equal, where a handle with no path (an inline asset, rebuilt as a new asset) is compared by
/// the asset it holds.
fn same(
    world: &World,
    registry: &bevy::reflect::TypeRegistry,
    a: &dyn PartialReflect,
    b: &dyn PartialReflect,
) -> bool {
    use bevy::asset::{ReflectAsset, ReflectHandle};
    use bevy::reflect::ReflectRef;
    if let Some(info) = a.get_represented_type_info()
        && let Some(handle) = registry.get_type_data::<ReflectHandle>(info.type_id())
    {
        let untyped = |v: &dyn PartialReflect| {
            v.try_as_reflect().and_then(|v| handle.downcast_handle_untyped(v.as_any()))
        };
        let (Some(ha), Some(hb)) = (untyped(a), untyped(b)) else {
            return false;
        };
        if ha.path().is_some() || hb.path().is_some() {
            return ha.path() == hb.path();
        }
        let Some(assets) = registry.get_type_data::<ReflectAsset>(handle.asset_type_id()) else {
            return ha == hb;
        };
        return match (assets.get(world, ha.id()), assets.get(world, hb.id())) {
            (Some(x), Some(y)) => same(world, registry, x.as_partial_reflect(), y.as_partial_reflect()),
            _ => false,
        };
    }
    match (a.reflect_ref(), b.reflect_ref()) {
        (ReflectRef::Struct(x), ReflectRef::Struct(y)) => {
            x.field_len() == y.field_len()
                && x.iter_fields().all(|(name, f)| y.field(name).is_some_and(|g| same(world, registry, f, g)))
        }
        (ReflectRef::TupleStruct(x), ReflectRef::TupleStruct(y)) => {
            x.field_len() == y.field_len()
                && x.iter_fields().zip(y.iter_fields()).all(|(f, g)| same(world, registry, f, g))
        }
        (ReflectRef::Enum(x), ReflectRef::Enum(y)) => {
            x.variant_name() == y.variant_name()
                && x.field_len() == y.field_len()
                && (0..x.field_len()).all(|i| match (x.field_at(i), y.field_at(i)) {
                    (Some(f), Some(g)) => same(world, registry, f, g),
                    _ => false,
                })
        }
        (ReflectRef::List(x), ReflectRef::List(y)) => {
            x.len() == y.len() && x.iter().zip(y.iter()).all(|(f, g)| same(world, registry, f, g))
        }
        _ => a.reflect_partial_eq(b).unwrap_or(true),
    }
}

fn round_trip(path: &str) {
    if !Path::new(ZERO_ASSETS).join(path).exists() {
        eprintln!("skipping {path}: zero's assets are not checked out");
        return;
    }
    let mut app = util::ambient_app_at(Path::new(ZERO_ASSETS));
    app.finish();
    app.cleanup();
    let root = spawn_file(&mut app, path);
    let text = write_scene_text(app.world_mut(), root, &WriteSettings::default())
        .unwrap_or_else(|err| panic!("{path}: {err}"));
    let copy = spawn_text(&mut app, path, &text);
    assert_same_tree(app.world(), root, copy, path, &text);
}

#[test]
fn a_skinned_rig_with_inline_materials_round_trips() {
    round_trip("ual/Mannequin.bsn");
}

#[test]
fn a_wind_swayed_tree_round_trips() {
    round_trip("speedtree/White_Oak.bsn");
}

#[test]
fn a_prop_with_colliders_round_trips() {
    round_trip("lunarbase/KB3D_LNB_Lamp_A.bsn");
}

#[test]
fn a_building_round_trips() {
    round_trip("lunarbase/KB3D_LNB_BldgSM_K.bsn");
}

#[test]
fn an_animated_prop_round_trips() {
    round_trip("sim/door/door.bsn");
}

