//! A relation other than `Children` spawns its entities under the scene root, related to their
//! owner through the relationship registered with `#[reflect(RelationshipTarget)]`.

use std::path::PathBuf;

use bevy::prelude::*;
use jackdaw_runtime::{JackdawPlugin, JackdawScene, JackdawSceneRoot};

#[derive(Component, Reflect)]
#[relationship(relationship_target = Items)]
#[reflect(Component)]
struct ItemOf(Entity);

#[derive(Component, Reflect)]
#[relationship_target(relationship = ItemOf)]
#[reflect(Component, RelationshipTarget)]
struct Items(Vec<Entity>);

fn named(app: &mut App, name: &str) -> Entity {
    app.world_mut()
        .query::<(Entity, &Name)>()
        .iter(app.world())
        .find(|(_, n)| n.as_str() == name)
        .map(|(e, _)| e)
        .unwrap_or_else(|| panic!("no entity named {name}"))
}

#[test]
fn a_custom_relation_relates_its_entities_to_the_owner() {
    let mut app = App::new();
    app.add_plugins(MinimalPlugins);
    app.add_plugins(bevy::transform::TransformPlugin);
    app.add_plugins(bevy::asset::AssetPlugin::default());
    app.add_plugins(bevy::world_serialization::WorldSerializationPlugin);
    app.add_plugins(JackdawPlugin);
    app.register_type::<Items>();

    let items = std::any::type_name::<Items>();
    let text = format!(
        "#Chest\n{items} [\n    #Sword\n    --\n    #Shield\n]\nChildren [ #Lid ]\n"
    );
    let scene = app
        .world_mut()
        .resource_mut::<Assets<JackdawScene>>()
        .add(JackdawScene::with_stem(text, PathBuf::new(), None));
    let root = app.world_mut().spawn(JackdawSceneRoot(scene)).id();

    app.update();
    app.update();

    let chest = named(&mut app, "Chest");
    let sword = named(&mut app, "Sword");
    let shield = named(&mut app, "Shield");
    let lid = named(&mut app, "Lid");
    assert_eq!(
        app.world().get::<Items>(chest).map(|i| i.0.clone()),
        Some(vec![sword, shield])
    );
    assert_eq!(app.world().get::<ChildOf>(sword).map(ChildOf::parent), Some(root));
    assert_eq!(app.world().get::<ChildOf>(lid).map(ChildOf::parent), Some(chest));
}
