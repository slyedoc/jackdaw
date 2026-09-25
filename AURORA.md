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
* `main.rs` builds `AuroraDefaultPlugins`, not bevy's. bevy's group wires `RenderPlugin`
  and the ui/pbr render halves, which panics the moment anything asks for a
  `DrawFunctions<TransparentUi>` that was never created. Gone with it: the
  `render_diagnostics` wgpu timestamp path, and `.set(ImagePlugin)` -- it existed to make
  the default sampler REPEAT, and aurora's one global linear sampler already does that.
* `src/editor_grid_depth_patch.rs` is deleted. It overwrote `bevy_dev_tools`' embedded
  `infinite_grid.wesl` with a depth-writing variant -- a plugin and a shader that both
  stop existing here, and loading it panicked on `Assets<Shader>` never being initialized
  (aurora has its own `Shader` type; bevy's is only registered by `RenderPlugin`).
* Aurora gained multi-camera support for this: `Camera::order` sorts views, per-view
  extents, `Camera::viewport` for a docked rect, `RenderTarget::Image` offscreen targets,
  and `ViewportNode` drawing in `ui_render`.
* Debugger sparklines and system-graph edges are `UiPolyline`s; both shaders deleted.
  The sparkline lost the area fill under its curve -- the stroke is what carries the
  reading, and a fill needs a triangulated mesh rather than a polyline.
* `src/fps_overlay/overlay.rs` vendors `bevy_dev_tools::fps_overlay`, frame time graph
  included. The graph was a `UiMaterial` reading a `ShaderBuffer`; it is now one
  absolutely-positioned child `Node` per bar, sized in percentages so it never needs the
  node's size, with the same log2 height mapping and green-to-red ramp as the shader.
  The bar pool is spawned once and shown/hidden rather than respawned each refresh.

## Gotcha: verify with `--features dylib`

`crates/jackdaw_api`'s `dynamic_linking` feature carries its OWN list of bevy features,
and cargo unions features per build -- so `cargo check --workspace` and
`cargo run -r --features dylib` resolve different bevy builds. A render feature named only
in that list is invisible to a plain check and switches the whole editor over at runtime.
`bevy/bevy_feathers` (the render half) hid there and put `UiMaterialPlugin` back, which
panicked on `Assets<Shader>`. Keep that list in step with the root manifest's bevy
features, and verify with `--features dylib`, because that is the only configuration the
editor is ever run in.

Confirm a feature is really gone rather than merely compiling:

    cargo tree -e features --features dylib -i bevy_feathers | grep render_materials

## Gotcha: the shaderc rpath

`bevy_aurora` links `libshaderc_shared.so.1` from the Vulkan SDK, and the SDK's
`setup-env.sh` puts only `$VULKAN_SDK/lib/VulkanLoader/lib` on `LD_LIBRARY_PATH` --
not `$VULKAN_SDK/lib`, where shaderc actually lives. `cargo:rustc-link-arg` does NOT
propagate from a dependency's build script, so every package here that produces a
RUNNABLE artifact against aurora needs its own `build.rs` emitting the rpath. The root
crate and `jackdaw_node_graph` have one; **any new crate that takes `bevy_aurora` needs
the same file**, or it links fine and then dies at startup with "error while loading
shared libraries".

## Gotcha: a disabled MaterialPlugin takes its data with it

`MaterialPlugin` does two jobs -- `init_asset::<M>()` and the draw. Commenting one out for
this branch removed the asset collection as well, and the editor AUTHORS these materials:
it reads, edits and saves them as scene data whether or not anything draws them. Every
system touching one then fails parameter validation at runtime, one system per frame, with
nothing at compile time.

So the registrations are back without the plugins: `StandardMaterial` (normally
`PbrPlugin`), `TerrainSplatMaterial`, the two gizmo-overlay materials, and
`ScatterPrefabs`. `init_asset`/`init_resource` are idempotent, so each plugin takes its own
back unchanged when it returns.

## Left, roughly in dependency order

1. **Offscreen camera targets.** Aurora renders one `Camera3d` to one swapchain. The
   editor needs `RenderTarget::Image` in seven places: `viewport.rs`, `viewport_2d.rs`,
   `thumbnail.rs`, `material_preview.rs`, `camera_preview.rs`, `camera_capture.rs`.
   These still COMPILE today, because `bevy_render` has not actually left the graph
   yet (item 2 and item 6 keep it there) -- they will stop the day it does.
   `viewport.rs` also uses `Fxaa`, OIT settings and `render_resource` texture types
   directly, all of which go with it.
2. **`jackdaw_surface`, terrain render, and the gizmo-overlay materials.** Every
   `AsBindGroup` material and every `add_render_command` needs a `RenderApp`, which no
   longer exists -- each one panics at startup, not at compile time. They are all
   commented out with `TODO(aurora)` markers so the editor boots; what is dark right now
   is terrain splat/scatter/detail rendering, water, foliage, the sky material and the
   brush gizmo overlay.

   Details of the port itself: **`jackdaw_surface`** (~2,400 lines): `LayeredSurfaceMaterial`, `FoliageMaterial`,
   `WaterMaterial` are `AsBindGroup`, plus `SkyMaterial` and the environment. 42
   references across 8 editor files (`material_assets`, `material_browser`,
   `inspector/material_row`, `definition_assets`, `scene_io`, ...). Aurora's
   `AuroraMaterial` has to substitute. This is the last unconditional `bevy_render`
   dependency inside jackdaw itself.
3. **Draw the grid.** `src/infinite_grid.rs` is data only. On a ray tracer a ground
   grid is a ray-plane intersection in the miss path, not geometry to rasterize.
4. **`jackdaw_terrain/render` and `jackdaw_runtime/render`** are still ON, marked
   TODO(aurora) in Cargo.toml. They carry the splat material and the material
   plumbing the editor calls (`MaterialOverridesPlugin`, `material_of_reference`), so
   they go with item 2 rather than separately.
5. **Revisit `propagate_on_cpu: true`** in `main.rs` when the f64 GPU-transform branch
   lands. It is on because aurora propagates only ROOT transforms -- the tracer reads its
   own GPU transforms and never touches `GlobalTransform`, while the editor reads a
   descendant's constantly (brush handles, gizmo placement, viewport picking, the scene
   tree). If the GPU path grows a readable f64 `GlobalTransform`, the editor may be able
   to drop bevy's full CPU propagation instead of paying for both.
6. **`bevy_rerecast_core`'s `bevy_mesh` feature is `["dep:bevy_mesh", "dep:bevy_render"]`**,
   so the navmesh bake's `TriMeshFromBevyMesh` pulls bevy_render at the fork level.
   Worth fixing in slyedoc/rerecast once the bigger items land.
