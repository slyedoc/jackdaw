use std::borrow::Cow;

use bevy::app::{PluginGroup, PluginGroupBuilder};
use bevy::prelude::*;
use jackdaw::prelude::*;
use jackdaw_api_internal::lifecycle::{ExtensionAppExt as _, OperatorEntity, enable_extension};
use jackdaw_api_internal::snapshot::{ActiveSnapshotter, SceneSnapshot};

/// The aurora group the editor boots on, minus the device: the assets, types and loaders a
/// scene can name (`Assets<AuroraMaterial>`, the surface-class registry, ...), and nothing that
/// draws. No window or audio backend. With no GPU there is no transform readback, so bevy's
/// own `TransformPlugin` fills `GlobalTransform` on the CPU.
struct HeadlessAurora;

impl PluginGroup for HeadlessAurora {
    fn build(self) -> PluginGroupBuilder {
        use bevy_aurora::{
            animclip, assets, bsn, collision, material, mesh, render_texture, shader, skinning,
            sky, sphere, surface_group, ui_render,
        };
        PluginGroupBuilder::start::<Self>()
            // Before AssetPlugin: registers the `aurora://` source for the engine's own assets.
            .add(assets::AuroraAssetSourcePlugin)
            .add(bevy::log::LogPlugin::default())
            .add(bevy::app::TaskPoolPlugin::default())
            .add(bevy::diagnostic::FrameCountPlugin)
            .add(bevy::time::TimePlugin)
            .add(bevy::transform::TransformPlugin)
            .add(bevy::diagnostic::DiagnosticsPlugin)
            .add(bevy::input::InputPlugin)
            .add(bevy::window::WindowPlugin {
                close_when_requested: false,
                ..default()
            })
            .add(bevy::a11y::AccessibilityPlugin)
            .add(bevy::asset::AssetPlugin::default())
            .add(bevy::scene::ScenePlugin)
            .add(bevy::bsn_asset::BsnAssetPlugin)
            .add(bevy::animation::AnimationPlugin)
            .add(bevy::world_serialization::WorldSerializationPlugin)
            // Aurora's asset, loader and reflection registrations only.
            .add(shader::ShaderPlugin)
            .add(material::MaterialPlugin)
            .add(render_texture::RenderTexturePlugin)
            .add(mesh::AuroraMeshPlugin)
            .add(collision::CollisionPlugin)
            .add(sphere::SpherePlugin)
            .add(surface_group::SurfaceGroupPlugin)
            .add(bsn::BsnPlugin)
            .add(animclip::AnimClipPlugin)
            .add(skinning::SkinTypesPlugin)
            .add(sky::SkyPlugin)
            .add(ui_render::UiTreePlugin)
    }
}

pub fn headless_app() -> App {
    let mut app = ambient_app();
    add_editor_plugins(&mut app);
    app
}

/// Everything under the editor: the plugins a binary adds before
/// `add_editor_plugins`, and nothing of jackdaw's own. Split out so a
/// test can stand a game's plugins between the two, which is the
/// arrangement the editor meets when it loads one.
#[expect(clippy::allow_attributes, reason = "shared across test binaries")]
#[allow(dead_code, reason = "shared across test binaries")]
pub fn ambient_app() -> App {
    ambient_app_with(bevy::asset::AssetPlugin::default())
}

/// [`ambient_app`] reading assets from `assets`.
#[expect(clippy::allow_attributes, reason = "shared across test binaries")]
#[allow(dead_code, reason = "shared across test binaries")]
pub fn ambient_app_at(assets: &std::path::Path) -> App {
    ambient_app_with(bevy::asset::AssetPlugin {
        file_path: assets.to_string_lossy().into_owned(),
        ..default()
    })
}

fn ambient_app_with(assets: bevy::asset::AssetPlugin) -> App {
    let mut app = App::new();
    // The two additions mirror `src/main.rs`: aurora carries no state machinery or gizmos.
    app.add_plugins(
        HeadlessAurora
            .build()
            .set(assets)
            .add(bevy::state::app::StatesPlugin)
            .add(bevy::gizmos::GizmoPlugin),
    )
    // Ambient plugins moved to the binary entry point (matches
    // the launcher's `src/main.rs` and the static template's
    // `editor.rs.template`). Mirror that here so the editor's
    // internal `debug_assert!`s for `PhysicsSchedulePlugin` and
    // `EnhancedInputPlugin` find what they expect.
    // The headless set is not AuroraDefaultPlugins, which brings physics and
    // animation graphs to the editor; add them as it does.
    .add_plugins((
        avian3d::prelude::PhysicsPlugins::default(),
        bevy_animation_graph::AnimationGraphPlugin::default(),
        bevy_enhanced_input::prelude::EnhancedInputPlugin,
    ));
    app
}

/// The editor itself, over an app that already carries the ambient
/// plugins.
#[expect(clippy::allow_attributes, reason = "shared across test binaries")]
#[allow(dead_code, reason = "shared across test binaries")]
pub fn add_editor_plugins(app: &mut App) {
    app.add_plugins(JackdawEditorPlugins::default());
}

/// Like [`headless_app`] but also runs the startup pass and ticks one
/// frame so every built-in extension is registered, enabled, and its
/// operators populated in the `OperatorIndex`. Most operator integration
/// tests should start here.
#[expect(clippy::allow_attributes, reason = "shared across test binaries")]
#[allow(
    dead_code,
    reason = "shared across test binaries; not every test exercises this path."
)]
pub fn editor_test_app() -> App {
    let mut app = headless_app();
    app.finish();
    // First tick runs Startup + extension auto-enable so every
    // built-in's operator entities are spawned.
    app.update();
    app
}

/// Advance the app's clock by a fixed step per frame, so gestures measured
/// in seconds (double clicks, spring loads) read the same on a loaded
/// runner as on a fast machine.
#[expect(clippy::allow_attributes, reason = "Some tests use this")]
#[allow(
    dead_code,
    reason = "shared across integration test binaries; not every test file calls it."
)]
pub fn fixed_frame_clock(app: &mut App) {
    app.insert_resource(bevy::time::TimeUpdateStrategy::ManualDuration(
        std::time::Duration::from_millis(16),
    ));
}

/// Register `T` in the catalog AND enable it.
///
/// `register_extension` alone only adds the extension to the catalog; the
/// editor's normal startup enables whatever `~/.config/jackdaw/extensions.json`
/// lists, plus [`REQUIRED_EXTENSIONS`](jackdaw::extensions_config::REQUIRED_EXTENSIONS).
/// Tests don't populate the on-disk config, so custom test extensions would
/// otherwise stay disabled and their operators wouldn't resolve.
///
/// This helper runs the usual `register -> finish -> first-update` dance, then
/// force-enables the extension explicitly so `app.world_mut().operator(id)`
/// can find it. It also ticks one more frame so any setup observers (operator
/// index, BEI context attachment) have settled before the caller runs
/// assertions.
#[expect(clippy::allow_attributes, reason = "Some tests use this")]
#[allow(
    dead_code,
    reason = "shared across integration test binaries; not every test file calls it."
)]
pub fn register_and_enable_extension<T: JackdawExtension + Default>(app: &mut App) {
    app.register_extension::<T>();
    app.finish();
    // First update runs Startup (which enables whatever the on-disk config
    // lists; typically nothing relevant to the test).
    app.update();
    // Force-enable the test extension; idempotent if it was already enabled
    // (returns `None` in that case).
    enable_extension(app.world_mut(), &T::default().id());
    // Let any on-add observers for the operator entities settle before the
    // caller starts asserting.
    app.update();
}

/// Collect every registered operator id in the world. Reads from
/// `OperatorEntity` components rather than the (private) `OperatorIndex`
/// resource. Sorted so test failures are stable.
#[expect(clippy::allow_attributes, reason = "shared across test binaries")]
#[allow(dead_code, reason = "smoke + availability tests use this")]
pub fn iter_operator_ids(app: &mut App) -> Vec<Cow<'static, str>> {
    let mut ids: Vec<Cow<'static, str>> = app
        .world_mut()
        .query::<&OperatorEntity>()
        .iter(app.world())
        .map(|op| Cow::Borrowed(op.id()))
        .collect();
    ids.sort();
    ids
}

/// Every registered `(id, label)` pair. Two entries sharing an id means two
/// subsystems registered that id; the dispatcher's index is last-registration
/// -wins, so only one of them is reachable by id.
#[expect(clippy::allow_attributes, reason = "shared across test binaries")]
#[allow(dead_code, reason = "scene op id tests use this")]
pub fn operator_id_labels(app: &mut App) -> Vec<(&'static str, &'static str)> {
    app.world_mut()
        .query::<&OperatorEntity>()
        .iter(app.world())
        .map(|op| (op.id(), op.label()))
        .collect()
}

/// Every label registered under `id`. More than one entry means the id is
/// ambiguous; see [`operator_id_labels`].
#[expect(clippy::allow_attributes, reason = "shared across test binaries")]
#[allow(dead_code, reason = "scene op id tests use this")]
pub fn operator_labels(app: &mut App, id: &str) -> Vec<&'static str> {
    app.world_mut()
        .query::<&OperatorEntity>()
        .iter(app.world())
        .filter(|op| op.id() == id)
        .map(OperatorEntity::label)
        .collect()
}

/// Capture a scene snapshot via the `ActiveSnapshotter`. Wrapper around
/// the standard `resource_scope` dance used by the dispatcher.
#[expect(clippy::allow_attributes, reason = "shared across test binaries")]
#[allow(dead_code, reason = "modal + undo tests use this")]
pub fn snapshot(app: &mut App) -> Box<dyn SceneSnapshot> {
    app.world_mut()
        .resource_scope(|world, snapshotter: Mut<ActiveSnapshotter>| snapshotter.0.capture(world))
}

#[expect(clippy::allow_attributes, reason = "Some tests use this")]
#[allow(
    dead_code,
    reason = "shared across integration test binaries; not every test file exercises operator dispatch."
)]
pub trait OperatorResultExt: Copy {
    /// Asserts that the operator finished successfully and panics if it did not.
    /// Hidden away in test utils so extension devs don't fall into the trap of actually doing this in production.
    fn assert_finished(self);

    /// Asserts that the operator was cancelled (e.g. its availability
    /// gate refused, or the call hit a no-op early-return). Used by
    /// gate-blocked dispatch tests.
    fn assert_cancelled(self);

    /// Asserts that the operator returned `Running`, indicating it has
    /// entered a modal session. Used by modal start tests.
    fn assert_running(self);
}

impl OperatorResultExt for OperatorResult {
    fn assert_finished(self) {
        assert_eq!(self, OperatorResult::Finished, "Operator failed to finish");
    }
    fn assert_cancelled(self) {
        assert_eq!(
            self,
            OperatorResult::Cancelled,
            "Operator did not cancel as expected"
        );
    }
    fn assert_running(self) {
        assert_eq!(
            self,
            OperatorResult::Running,
            "Operator did not enter modal Running state"
        );
    }
}

/// Whether `entity` carries bevy_ui's legacy `Interaction`, the marker of a hand-rolled
/// control rather than a `bevy_ui_widgets` one.
///
/// By name, because the fork made the type behind the `Interaction` alias private, so it
/// cannot be named in a turbofish any more.
#[expect(clippy::allow_attributes, reason = "shared across test binaries")]
#[allow(dead_code, reason = "shared across test binaries")]
pub fn has_legacy_interaction(world: &World, entity: Entity) -> bool {
    world.inspect_entity(entity).is_ok_and(|components| {
        components
            .into_iter()
            .any(|(_, info)| info.name().to_string().ends_with("::Interaction"))
    })
}
