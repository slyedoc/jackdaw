//! Applies a scene's [`Environment`] to aurora's sky and exposure.
//!
//! The raster back end dressed each 3D camera in components -- `DistanceFog`, `Bloom`,
//! `Tonemapping`, `ColorGrading`, `Fxaa`/`Smaa`/`Taa`, `Msaa` -- and drew the sky as a
//! screen triangle wearing `SkyMaterial`. None of those exist on a ray tracer: the sky is
//! evaluated in the miss shader, antialiasing is DLSS, and exposure is one camera component.
//! So this reads the same authored `Environment` and writes aurora's resources instead.

use bevy::prelude::*;
use bevy_aurora::{
    auto_exposure::{AuroraExposure, FixedExposure},
    sky::{ProceduralSky, Sky as AuroraSky},
};
use jackdaw_scene_types::{Environment, Sky};

/// Reads the scene's environment and writes aurora's sky and exposure.
pub struct EnvironmentPlugin;

impl Plugin for EnvironmentPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(
            PostUpdate,
            (
                // Aurora's sky resources come with its render side; a headless host has the
                // scene's `Environment` but nothing to write it to.
                follow_the_scene_sky.run_if(resource_exists::<AuroraSky>),
                follow_the_scene_exposure,
            ),
        );
    }
}

/// The sun the sky draws its disc at: the scene's first directional light.
#[derive(Clone, Copy, Debug)]
pub struct SkySun {
    /// Degrees above the horizon.
    pub elevation: f32,
    /// Degrees clockwise from -Z.
    pub azimuth: f32,
    pub illuminance: f32,
}

impl SkySun {
    /// From the light's forward direction, which is the way its rays travel.
    pub fn of(transform: &GlobalTransform, illuminance: f32) -> Self {
        let to_sun = -transform.forward().as_vec3();
        Self {
            elevation: to_sun.y.clamp(-1.0, 1.0).asin().to_degrees(),
            azimuth: (-to_sun.x).atan2(-to_sun.z).to_degrees().rem_euclid(360.0),
            illuminance,
        }
    }
}

/// jackdaw's gradient sky as aurora's analytic one. `brightness` is candela per square metre
/// with 1000 showing the colours as authored, which is the nits the three bands take.
pub fn procedural_sky(sky: &Sky, sun: Option<SkySun>) -> ProceduralSky {
    let nits = sky.brightness;
    ProceduralSky {
        sun_elevation: sun.map_or(45.0, |s| s.elevation),
        sun_azimuth: sun.map_or(0.0, |s| s.azimuth),
        // Authored as an angular DIAMETER, aurora takes a radius.
        sun_angular_radius: (sky.sun_size * 0.5).clamp(0.25, 20.0),
        sun_radiance: sun.map_or(0.0, |s| s.illuminance * sky.sun_intensity),
        zenith: sky.zenith,
        zenith_nits: nits,
        horizon: sky.horizon,
        // `horizon_softness` has no aurora term: the analytic sky's falloff is fixed.
        horizon_nits: nits,
        ground: sky.ground,
        ground_nits: nits,
    }
}

fn follow_the_scene_sky(
    environments: Query<&Environment>,
    suns: Query<(&DirectionalLight, &GlobalTransform)>,
    mut sky: ResMut<AuroraSky>,
    mut procedural: ResMut<ProceduralSky>,
) {
    let Some(environment) = environments.iter().next() else {
        return;
    };
    // Guarded writes rather than change detection on `Environment`: the sun is a separate
    // entity and moving it has to reach the sky too.
    if !environment.sky.enabled {
        // A disabled sky is black, not absent -- a miss ray still has to be answered.
        if !matches!(*sky, AuroraSky::Color { radiance } if radiance == Vec3::ZERO) {
            *sky = AuroraSky::Color {
                radiance: Vec3::ZERO,
            };
        }
        return;
    }
    let sun = suns
        .iter()
        .next()
        .map(|(light, transform)| SkySun::of(transform, light.illuminance));
    // `ProceduralSky` is not PartialEq, and writing it every frame would flag the resource
    // changed every frame; the sun's angles are what actually move.
    let next = procedural_sky(&environment.sky, sun);
    let moved = procedural.sun_elevation != next.sun_elevation
        || procedural.sun_azimuth != next.sun_azimuth
        || procedural.sun_radiance != next.sun_radiance
        || procedural.zenith_nits != next.zenith_nits
        || procedural.zenith != next.zenith
        || procedural.horizon != next.horizon
        || procedural.ground != next.ground;
    if moved {
        *procedural = next;
    }
    if !matches!(*sky, AuroraSky::Procedural) {
        *sky = AuroraSky::Procedural;
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
