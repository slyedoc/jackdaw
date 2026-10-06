//! Applies a scene's [`Environment`] to aurora's sky and exposure.
//!
//! The raster back end dressed each 3D camera in components -- `DistanceFog`, `Bloom`,
//! `Tonemapping`, `ColorGrading`, `Fxaa`/`Smaa`/`Taa`, `Msaa` -- and drew the sky as a
//! screen triangle wearing `SkyMaterial`. None of those exist on a ray tracer: the sky is
//! evaluated in the miss shader, antialiasing is DLSS, and exposure is one camera component.
//! So this reads the same authored `Environment` and writes aurora's sky (on its world) and
//! camera exposure instead.

use bevy::prelude::*;
use bevy_aurora::{
    auto_exposure::{AuroraExposure, FixedExposure},
    sky::{GradientSky, Sky as AuroraSky},
    world::{MainPhysicsWorldEntity, PhysicsWorld},
};
use jackdaw_scene_types::{Environment, Sky};

/// Reads the scene's environment and writes aurora's sky and exposure.
pub struct EnvironmentPlugin;

impl Plugin for EnvironmentPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(
            PostUpdate,
            (
                follow_the_scene_sky.run_if(resource_exists::<MainPhysicsWorldEntity>),
                follow_the_scene_exposure,
            ),
        );
    }
}

/// jackdaw's gradient sky as aurora's. `brightness` is candela per square metre with 1000
/// showing the colours as authored, which is the nits the three bands take.
/// `horizon_softness` has no aurora term: the gradient's falloff is fixed.
pub fn gradient_sky(sky: &Sky) -> GradientSky {
    let nits = sky.brightness;
    GradientSky {
        zenith: sky.zenith,
        zenith_nits: nits,
        horizon: sky.horizon,
        horizon_nits: nits,
        ground: sky.ground,
        ground_nits: nits,
    }
}

/// Each scene's sky onto its world: the nearest `PhysicsWorld` above its `Environment`, else
/// the main world. The sun needs nothing here: aurora reads the scene's `DirectionalLight` (and
/// its `SunDisk`) itself.
fn follow_the_scene_sky(
    mut commands: Commands,
    environments: Query<(Entity, &Environment)>,
    main_world: Res<MainPhysicsWorldEntity>,
    parents: Query<&ChildOf>,
    worlds: Query<(), With<PhysicsWorld>>,
    current: Query<(Option<&AuroraSky>, Option<&GradientSky>)>,
) {
    for (entity, environment) in &environments {
        let world = parents
            .iter_ancestors(entity)
            .find(|&ancestor| worlds.contains(ancestor))
            .unwrap_or(main_world.0);
        let (sky, gradient) = current.get(world).unwrap_or((None, None));
        // Guarded writes: inserting every frame would flag the components changed every frame.
        if !environment.sky.enabled {
            // A disabled sky is black, not absent -- a miss ray still has to be answered.
            if !matches!(sky, Some(AuroraSky::Color { radiance }) if *radiance == Vec3::ZERO) {
                commands.entity(world).insert(AuroraSky::Color {
                    radiance: Vec3::ZERO,
                });
            }
            continue;
        }
        if !matches!(sky, Some(AuroraSky::Gradient)) {
            commands.entity(world).insert(AuroraSky::Gradient);
        }
        let next = gradient_sky(&environment.sky);
        let same = gradient.is_some_and(|g| {
            g.zenith == next.zenith
                && g.zenith_nits == next.zenith_nits
                && g.horizon == next.horizon
                && g.ground == next.ground
        });
        if !same {
            commands.entity(world).insert(next);
        }
    }
}

/// The camera exposure, when the scene's post settings claim it.
///
/// TODO(aurora): the rest of `PostProcess` -- tonemapper, contrast, saturation, hue shift,
/// white balance, bloom, vignette -- has no aurora term yet, and neither does `Fog`.
fn follow_the_scene_exposure(
    mut commands: Commands,
    environments: Query<&Environment, Changed<Environment>>,
    cameras: Query<Entity, With<Camera3d>>,
) {
    let Some(environment) = environments.iter().next() else {
        return;
    };
    let post = &environment.post;
    for camera in &cameras {
        if post.enabled {
            // jackdaw authors EV100, where a bright exterior is about +15; aurora's `ev` is
            // log2 of a multiplier on radiance in nits, where the same scene is about -15.
            commands
                .entity(camera)
                .insert(AuroraExposure::Fixed(FixedExposure {
                    ev: post.post_exposure - post.exposure,
                }));
        } else {
            commands.entity(camera).remove::<AuroraExposure>();
        }
    }
}
