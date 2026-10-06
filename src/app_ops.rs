//! App-level operators: open the Extensions dialog, open the Keybind
//! settings dialog, toggle hot reload, return to the project-select
//! home screen. None have keybinds currently; they exist so menus (and
//! a future command palette) can dispatch them uniformly.

use bevy::prelude::*;
use jackdaw_api::prelude::*;

pub(crate) fn add_to_extension(ctx: &mut ExtensionContext) {
    ctx.register_operator::<AppOpenExtensionsOp>()
        .register_operator::<AppOpenKeybindsOp>()
        .register_operator::<AppGoHomeOp>();
}

#[operator(
    id = "app.open_extensions",
    label = "Extensions...",
    allows_undo = false
)]
pub(crate) fn app_open_extensions(
    _: In<OperatorParameters>,
    mut commands: Commands,
) -> OperatorResult {
    commands.queue(|world: &mut World| {
        crate::extensions_dialog::open_extensions_dialog(world);
    });
    OperatorResult::Finished
}

#[operator(id = "app.open_keybinds", label = "Keybinds...", allows_undo = false)]
pub(crate) fn app_open_keybinds(
    _: In<OperatorParameters>,
    mut commands: Commands,
) -> OperatorResult {
    commands.trigger(crate::keybind_settings::OpenKeybindSettingsEvent);
    OperatorResult::Finished
}

#[operator(id = "app.go_home", label = "Home", allows_undo = false)]
pub(crate) fn app_go_home(_: In<OperatorParameters>, mut commands: Commands) -> OperatorResult {
    commands.queue(|world: &mut World| {
        // Leaving clears the live scene, so unsaved work raises the same dialog
        // quitting does.
        if crate::scenes::confirm_dialog::leave_project_or_confirm(world) {
            world
                .resource_mut::<NextState<crate::AppState>>()
                .set(crate::AppState::ProjectSelect);
        }
    });
    OperatorResult::Finished
}
