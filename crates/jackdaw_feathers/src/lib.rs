pub mod alert;
pub mod button;
pub mod collapsible;
pub mod color_picker;
pub mod combobox;
pub mod context_menu;
pub mod dialog;
pub mod field_row;
pub mod file_browser;
pub mod icons;
pub mod inspector_card;
pub mod inspector_field;
pub mod list_view;
pub mod menu_bar;
pub mod number_input;
pub mod panel_card;
pub mod panel_section;
pub mod picker;
pub mod popover;
pub mod progress;
pub mod scroll;
pub mod segmented;
pub mod separator;
pub mod slider_row;
pub mod split_panel;
pub mod status_bar;
pub mod swatch_row;
pub mod tab_strip;
pub mod text_edit;
pub mod toast;
pub mod tokens;
pub mod tooltip;
pub mod tree_view;
pub mod utils;
pub mod variant_edit;
pub mod vector_edit;

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
            split_panel::SplitPanelPlugin,
            icons::IconFontPlugin,
            button::plugin,
            popover::plugin,
            combobox::plugin,
            dialog::plugin,
            text_edit::plugin,
            panel_section::plugin,
            inspector_field::plugin,
            variant_edit::plugin,
            scroll::plugin,
            list_view::plugin,
            toast::plugin,
            number_input::ScrubNumberInputPlugin,
        ));
        app.add_plugins((
            tooltip::TooltipPlugin,
            alert::plugin,
            color_picker::plugin,
            menu_bar::plugin,
            context_menu::plugin,
            picker::plugin,
            panel_card::plugin,
        ));
    }
}
