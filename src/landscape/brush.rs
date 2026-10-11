//! The landscape brush: sculpt (raise, lower, smooth, flatten) on a sculpt layer, paint and
//! erase on a paint layer, at the cursor's hit on the selected landscape.
//!
//! A stroke copies each page the first time a dab reaches it; on release the copies and the
//! pages as they ended become one undo entry ([`LandscapeStroke`]). The dabs themselves run on
//! the GPU (`aurora_landscape::SculptDabs`).

use aurora_landscape::{
    BrushKind, Landscape, LandscapePages, PaintLayer, PaintPages, SculptDab, SculptDabs,
    SculptLayer, SculptPages,
};
use bevy::{platform::collections::HashMap, prelude::*};
use bevy_aurora::render_device::RenderDevice;

use super::LandscapeBrush;
use crate::commands::{CommandHistory, EditorCommand};
use crate::selection::Selection;

/// Pages a stroke has touched: their texels before it (`None` = the page did not exist).
pub(crate) struct Stroke {
    landscape: Entity,
    layer: Entity,
    paint: bool,
    flatten_to: f32,
    before: HashMap<IVec2, Option<Vec<u8>>>,
}

/// The cursor's hit on the selected landscape, landscape-local, while a tool is armed.
pub(super) fn update_brush_hit(
    mut brush: ResMut<LandscapeBrush>,
    vp: crate::viewport::ViewportCursor,
    selection: Res<Selection>,
    landscapes: Query<(), With<Landscape>>,
    parents: Query<&ChildOf>,
    pages: Query<(&LandscapePages, &GlobalTransform)>,
) {
    brush.hit = None;
    if !brush.active() {
        return;
    }
    let Some((landscape, _)) = super::selected_landscape(&selection, &landscapes, &parents) else {
        return;
    };
    let Ok((pages, transform)) = pages.get(landscape) else {
        return;
    };
    let ray = (|| {
        let cursor = vp.viewport_pointer()?;
        let camera_entity = vp.camera_entity()?;
        let viewport_entity = vp.viewport_entity()?;
        let (camera, cam_tf) = vp.camera_for(camera_entity)?;
        let local = vp.viewport_cursor_for(camera, viewport_entity, cursor)?;
        camera.viewport_to_world(cam_tf, local).ok()
    })();
    let Some(ray) = ray else {
        return;
    };
    let origin = ray.origin - transform.translation();
    if let Some(hit) = pages.raycast(origin, *ray.direction, 20_000.0) {
        brush.hit = Some((landscape, hit));
    }
}

/// The layer a tool works on: the selected one when it is the right kind, else the
/// landscape's first of that kind.
fn target_layer(
    landscape: Entity,
    selected: Option<Entity>,
    paint: bool,
    children: &Query<&Children>,
    sculpts: &Query<(), With<SculptLayer>>,
    paints: &Query<(), With<PaintLayer>>,
) -> Option<Entity> {
    let fits = |e: Entity| {
        if paint {
            paints.contains(e)
        } else {
            sculpts.contains(e)
        }
    };
    selected
        .filter(|&e| fits(e))
        .or_else(|| children.get(landscape).ok()?.iter().find(|&e| fits(e)))
}

fn page_bytes<T: bytemuck::Pod>(page: Option<&[T]>) -> Option<Vec<u8>> {
    page.map(|texels| bytemuck::cast_slice(texels).to_vec())
}

/// Press, drag, release: dabs every frame the button is held over the landscape.
#[allow(clippy::too_many_arguments)]
pub(super) fn paint_strokes(
    mut commands: Commands,
    mouse: Res<ButtonInput<MouseButton>>,
    time: Res<Time>,
    mut brush: ResMut<LandscapeBrush>,
    mut dabs: ResMut<SculptDabs>,
    selection: Res<Selection>,
    landscapes: Query<(), With<Landscape>>,
    parents: Query<&ChildOf>,
    children: Query<&Children>,
    sculpt_layers: Query<(), With<SculptLayer>>,
    paint_layers: Query<(), With<PaintLayer>>,
    sculpts: Query<&SculptPages>,
    paints: Query<&PaintPages>,
    landscape_pages: Query<(&Landscape, &LandscapePages)>,
) {
    let Some(tool) = brush.tool else {
        brush.stroke = None;
        return;
    };
    let paint = tool.paints();

    if !mouse.pressed(MouseButton::Left) {
        if let Some(stroke) = brush.stroke.take() {
            commands.queue(move |world: &mut World| finish_stroke(world, stroke));
        }
        return;
    }
    if brush.stroke.is_none() {
        if !mouse.just_pressed(MouseButton::Left) {
            return;
        }
        let Some((landscape, hit)) = brush.hit else {
            return;
        };
        let selected = super::selected_landscape(&selection, &landscapes, &parents)
            .and_then(|(_, layer)| layer);
        let Some(layer) = target_layer(
            landscape,
            selected,
            paint,
            &children,
            &sculpt_layers,
            &paint_layers,
        ) else {
            warn!(
                "landscape: no {} layer to {} on; add one in the Landscape panel",
                if paint { "paint" } else { "sculpt" },
                if paint { "paint" } else { "sculpt" },
            );
            return;
        };
        brush.stroke = Some(Stroke {
            landscape,
            layer,
            paint,
            flatten_to: hit.y,
            before: HashMap::default(),
        });
    }
    let Some((landscape, hit)) = brush.hit else {
        return;
    };
    let (radius, strength, material) = (brush.radius, brush.strength, brush.material);
    let stroke = brush.stroke.as_mut().unwrap();
    if stroke.landscape != landscape {
        return;
    }
    let Ok((shape, _)) = landscape_pages.get(landscape) else {
        return;
    };
    // Copy every page this dab reaches before the GPU writes it.
    let rect = Rect::from_center_half_size(hit.xz(), Vec2::splat(radius));
    for page in shape.pages_in(rect) {
        if stroke.before.contains_key(&page) {
            continue;
        }
        let bytes = if stroke.paint {
            paints
                .get(stroke.layer)
                .ok()
                .and_then(|p| page_bytes(p.page(page)))
        } else {
            sculpts
                .get(stroke.layer)
                .ok()
                .and_then(|p| page_bytes(p.page(page)))
        };
        stroke.before.insert(page, bytes);
    }
    let dt = time.delta_secs();
    dabs.0.push(SculptDab {
        layer: stroke.layer,
        center: hit.xz(),
        radius,
        // Metres per second for raise / lower, a rate for the rest.
        strength: match tool {
            BrushKind::Raise | BrushKind::Lower => strength * dt,
            _ => (strength * 0.25 * dt).min(1.0),
        },
        kind: tool,
        target: stroke.flatten_to,
        material,
    });
}

/// The stroke's pages as they ended, beside their copies from before: one undo entry. The
/// layer's pages also go to the store (they live outside the scene until it saves).
fn finish_stroke(world: &mut World, stroke: Stroke) {
    let pages: Vec<IVec2> = stroke.before.keys().copied().collect();
    let after: HashMap<IVec2, Option<Vec<u8>>> = pages
        .iter()
        .map(|&page| {
            let bytes = if stroke.paint {
                world
                    .get::<PaintPages>(stroke.layer)
                    .and_then(|p| page_bytes(p.page(page)))
            } else {
                world
                    .get::<SculptPages>(stroke.layer)
                    .and_then(|p| page_bytes(p.page(page)))
            };
            (page, bytes)
        })
        .collect();
    super::save::store_layer(world, stroke.layer);
    world
        .resource_mut::<CommandHistory>()
        .push_executed(Box::new(LandscapeStroke {
            landscape: stroke.landscape,
            layer: stroke.layer,
            paint: stroke.paint,
            before: stroke.before,
            after,
        }));
}

/// One brush stroke, undoable: the pages it touched, before and after.
pub(crate) struct LandscapeStroke {
    landscape: Entity,
    layer: Entity,
    paint: bool,
    before: HashMap<IVec2, Option<Vec<u8>>>,
    after: HashMap<IVec2, Option<Vec<u8>>>,
}

impl LandscapeStroke {
    fn apply(&self, world: &mut World, pages: &HashMap<IVec2, Option<Vec<u8>>>) {
        if !world.contains_resource::<RenderDevice>() {
            return;
        }
        world.resource_scope(|world, rd: Mut<RenderDevice>| {
            for (&page, bytes) in pages {
                if self.paint {
                    if let Some(mut layer) = world.get_mut::<PaintPages>(self.layer) {
                        let texels = bytes.as_deref().map(bytemuck::cast_slice::<u8, [u32; 2]>);
                        layer.set_page(&rd, page, texels);
                    }
                } else if let Some(mut layer) = world.get_mut::<SculptPages>(self.layer) {
                    let texels = bytes.as_deref().map(bytemuck::cast_slice::<u8, f32>);
                    layer.set_page(&rd, page, texels);
                }
            }
        });
        if let Some(mut landscape) = world.get_mut::<LandscapePages>(self.landscape) {
            let touched = pages.keys().copied();
            if self.paint {
                landscape.mark_materials_dirty(touched);
            } else {
                landscape.mark_dirty(touched);
            }
        }
        super::save::store_layer(world, self.layer);
    }
}

impl EditorCommand for LandscapeStroke {
    fn execute(&mut self, world: &mut World) {
        let after = std::mem::take(&mut self.after);
        self.apply(world, &after);
        self.after = after;
    }

    fn undo(&mut self, world: &mut World) {
        let before = std::mem::take(&mut self.before);
        self.apply(world, &before);
        self.before = before;
    }

    fn description(&self) -> &str {
        if self.paint {
            "Landscape Paint"
        } else {
            "Landscape Sculpt"
        }
    }

    fn heap_bytes(&self) -> usize {
        self.before
            .values()
            .chain(self.after.values())
            .map(|b| b.as_ref().map_or(0, Vec::len))
            .sum()
    }
}

/// The brush ring on the ground at the cursor.
pub(super) fn draw_brush_ring(
    brush: Res<LandscapeBrush>,
    landscapes: Query<(&LandscapePages, &GlobalTransform)>,
    mut gizmos: Gizmos,
) {
    let Some((landscape, hit)) = brush.hit else {
        return;
    };
    let Ok((pages, transform)) = landscapes.get(landscape) else {
        return;
    };
    let origin = transform.translation();
    let colour = if brush.tool.is_some_and(BrushKind::paints) {
        Color::srgb(0.3, 0.8, 1.0)
    } else {
        Color::srgb(1.0, 0.75, 0.2)
    };
    const SEGMENTS: usize = 48;
    let ring = |i: usize| {
        let a = i as f32 / SEGMENTS as f32 * std::f32::consts::TAU;
        let xz = hit.xz() + Vec2::new(a.cos(), a.sin()) * brush.radius;
        origin + Vec3::new(xz.x, pages.height_at(xz) + 0.15, xz.y)
    };
    for i in 0..SEGMENTS {
        gizmos.line(ring(i), ring(i + 1), colour);
    }
}
