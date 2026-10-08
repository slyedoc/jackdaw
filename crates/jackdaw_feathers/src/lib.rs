pub mod button;
pub mod collapsible;
pub mod context_menu;
pub mod dialog;
pub mod field_row;
pub mod file_browser;
pub mod icons;
pub mod inspector_card;
pub mod menu_bar;
pub mod panel_card;
pub mod picker;
pub mod popover;
pub mod scroll;
pub mod segmented;
pub mod separator;
pub mod slider_row;
pub mod swatch_row;
pub mod status_bar;
pub mod tab_strip;
pub mod text_edit;
pub mod toast;
pub mod tokens;
pub mod tooltip;
pub mod tree_view;
pub mod utils;
pub mod variant_edit;

use bevy::app::Plugin;

pub struct EditorFeathersPlugin;

impl Plugin for EditorFeathersPlugin {
    fn build(&self, app: &mut bevy::app::App) {
        // These widgets are built on bevy's feathers: its core plugin registers the
        // embedded font source and seeds `UiTheme`, and several of the systems below take
        // that theme by `Res`. The editor guards for this because a loaded game may have
        // added either half already; do the same here so anything else using these widgets
        // (the crate's own examples, a standalone tool) is not left to know that.
        use bevy::feathers::FeathersCorePlugin;
        use bevy::input_focus::tab_navigation::TabNavigationPlugin;
        if !app.is_plugin_added::<TabNavigationPlugin>() {
            app.add_plugins(TabNavigationPlugin);
        }
        if !app.is_plugin_added::<FeathersCorePlugin>() {
            app.add_plugins(FeathersCorePlugin);
        }
        app.add_plugins((
            jackdaw_widgets::EditorWidgetsPlugins,
            icons::IconFontPlugin,
            button::plugin,
            popover::plugin,
            dialog::plugin,
            text_edit::plugin,
            variant_edit::plugin,
            scroll::plugin,
            toast::plugin,
        ));
        app.add_plugins((
            tooltip::TooltipPlugin,
            menu_bar::plugin,
            context_menu::plugin,
            picker::plugin,
            panel_card::plugin,
        ));
    }
}
