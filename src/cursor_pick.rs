//! The cursor's ray into the hovered viewport, traced on the GPU every frame by
//! `bevy_aurora::picking`. Tools read what is under the cursor through [`CursorHits`].
//!
//! The ray is aimed in `PostUpdate` and traced with the frame; its hits are there for every
//! system of the next frame. They trail the cursor by that one frame, which nothing a tool
//! does with them can show, since its reaction draws in the next frame either way.

use bevy::{camera::visibility::RenderLayers, ecs::system::SystemParam, prelude::*};
use bevy_aurora::{
    picking::{RayCaster, RayHit, RayHits},
    world::{InWorld, world_mask},
};

use crate::viewport::{MainViewportCamera, ViewportCursor};

pub(crate) struct CursorPickPlugin;

impl Plugin for CursorPickPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, spawn_cursor_ray)
            .add_systems(PostUpdate, aim_cursor_ray);
    }
}

/// The entity whose [`RayCaster`] follows the cursor.
#[derive(Component)]
pub(crate) struct CursorRay;

fn spawn_cursor_ray(mut commands: Commands) {
    let mut caster = RayCaster::new(Ray3d::new(Vec3::ZERO, Dir3::NEG_Y));
    caster.max_distance = 0.0;
    commands.spawn((Name::new("Cursor ray"), CursorRay, caster));
}

/// Through the cursor into the hovered viewport's camera, seeing what that camera sees; a
/// cursor over no viewport traces nothing.
fn aim_cursor_ray(
    vp: ViewportCursor,
    layers: Query<(Option<&RenderLayers>, Option<&InWorld>), With<MainViewportCamera>>,
    mut caster: Query<&mut RayCaster, With<CursorRay>>,
) {
    let Ok(mut caster) = caster.single_mut() else {
        return;
    };
    let aimed = (|| {
        let cursor = vp.cursor()?;
        let (vp_computed, vp_tf) = vp.viewport()?;
        let (camera, cam_tf) = vp.camera()?;
        let map = crate::viewport_util::ViewportRemap::new(camera, vp_computed, vp_tf);
        let ray = camera
            .viewport_to_world(cam_tf, (cursor - map.top_left) * map.remap)
            .ok()?;
        let (layers, world) = layers.get(vp.camera_entity()?).ok()?;
        let mask = world_mask(layers, world);
        Some((ray, mask))
    })();
    match aimed {
        Some((ray, mask)) => {
            caster.ray = ray;
            caster.mask = mask;
            caster.max_distance = f32::MAX;
        }
        None => caster.max_distance = 0.0,
    }
}

/// What the cursor ray hit, nearest first: the stand-in for a mesh ray cast at the cursor.
#[derive(SystemParam)]
pub(crate) struct CursorHits<'w, 's> {
    hits: Query<'w, 's, &'static RayHits, With<CursorRay>>,
}

impl CursorHits<'_, '_> {
    /// Every hit, nearest first; empty without a traced cursor ray.
    pub fn all(&self) -> &[RayHit] {
        self.hits.single().map_or(&[], |hits| &hits.0)
    }

    /// The hits whose entity passes `filter`, nearest first.
    pub fn filtered<'a>(
        &'a self,
        filter: impl Fn(Entity) -> bool + 'a,
    ) -> impl Iterator<Item = &'a RayHit> + 'a {
        self.all().iter().filter(move |hit| filter(hit.entity))
    }
}
