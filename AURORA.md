# The aurora branch

Replaces jackdaw's wgpu/`bevy_render` back end with `bevy_aurora`, the pure-Vulkan
ray tracer. Based on `bevy-main`; see FORK.md for how updates flow.

## What aurora already provides

Most of the editor needs no port at all. `bevy_aurora::ui_render` rasterizes the whole
`bevy_ui` node tree on Vulkan -- backgrounds, borders, outlines, images, text glyphs,
gradients, clipping -- so feathers, the docks, the inspector and the node graph draw
as they did. `gizmo_render` is the matching half for `bevy_gizmos`. `UiPolyline`
(points, thickness, colour, plus a `bezier()` helper) covers line and curve drawing
without a custom pipeline: it emits one rounded quad per segment into the same batch.

Feathers is `bevy_feathers_core` here -- feathers minus its two `UiMaterial`s. The
colour plane loses its gradient fill and swatches lose the alpha checkerboard; the
widgets themselves, their picking observers and their value plumbing are unaffected.

## Done

* Root manifest is `default-features = false` with an explicit render-free list.
* Five crates defaulted to a `render` feature (`jackdaw_jsn`, `jackdaw_geometry`,
  `jackdaw_scene_types`, `jackdaw_runtime`, `jackdaw_terrain`); all taken with
  `default-features = false`. Cargo unifies features across the workspace, so ONE of
  these left on puts wgpu back in every binary here.
* `jackdaw_widgets_runtime`'s `feathers` feature points at `bevy_feathers_core`.
* `bevy_rerecast` off default features, keeping `bevy_mesh` + `debug_plugin`.
* Node-graph connection wires: `ConnectionMaterial` + `connection.wgsl` deleted, wires
  are `UiPolyline`s. NOTE the frame change -- the old shader worked in PHYSICAL pixels
  from the viewport's top-left, `UiPolyline` speaks LOGICAL pixels, so both update
  systems apply `inverse_scale_factor()`. Without it wires draw at 2x on HiDPI and
  look correct on a 1x monitor.
* `src/infinite_grid.rs`: the grid's components, render-free, with dev_tools' field
  names and defaults so `snapping.rs` and `view_ops.rs` are unchanged.
* In the bevy fork: `bevy_dev_tools`'s drawing half is behind a `render` feature, so
  `bevy_remote` stops dragging wgpu in for `schedule_data`.

## Left, roughly in dependency order

1. **Offscreen camera targets.** Aurora renders one `Camera3d` to one swapchain. The
   editor needs `RenderTarget::Image` in seven places: `viewport.rs`, `viewport_2d.rs`,
   `thumbnail.rs`, `material_preview.rs`, `camera_preview.rs`, `camera_capture.rs`.
   These still COMPILE today, because `bevy_render` has not actually left the graph
   yet (item 2 and item 6 keep it there) -- they will stop the day it does.
   `viewport.rs` also uses `Fxaa`, OIT settings and `render_resource` texture types
   directly, all of which go with it.
2. **`jackdaw_surface`** (~2,400 lines): `LayeredSurfaceMaterial`, `FoliageMaterial`,
   `WaterMaterial` are `AsBindGroup`, plus `SkyMaterial` and the environment. 42
   references across 8 editor files (`material_assets`, `material_browser`,
   `inspector/material_row`, `definition_assets`, `scene_io`, ...). Aurora's
   `AuroraMaterial` has to substitute. This is the last unconditional `bevy_render`
   dependency inside jackdaw itself.
3. **Remote debug panel**: `GraphEdgeMaterial` and `SparklineMaterial` are still
   `UiMaterial`s. Both are line plots, so both want `UiPolyline` -- but the sparkline
   holds a ring buffer the shader plotted, so the handles in `diagnostics.rs` become
   components and a system maps samples to points against `ComputedNode::size`.
4. **Draw the grid.** `src/infinite_grid.rs` is data only. On a ray tracer a ground
   grid is a ray-plane intersection in the miss path, not geometry to rasterize.
5. **fps overlay.** `src/fps_overlay.rs` used `bevy_dev_tools`. zero already has a
   render-free replacement at `crates/util/src/fps.rs` to lift.
6. **`bevy_rerecast_core`'s `bevy_mesh` feature is `["dep:bevy_mesh", "dep:bevy_render"]`**,
   so the navmesh bake's `TriMeshFromBevyMesh` pulls bevy_render at the fork level.
   Worth fixing in slyedoc/rerecast once the bigger items land.
