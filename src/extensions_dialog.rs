//! File > Extensions dialog. Toggles compiled-in extensions at runtime
//! and persists the current state to `extensions.json`.


use bevy::{
    feathers::{
        controls::FeathersCheckbox,
        display::label_dim,
        theme::{ThemeBorderColor, ThemedText},
        tokens,
    },
    prelude::*,
    ui::Checked,
    ui_widgets::ValueChange,
};
use jackdaw_api::prelude::ExtensionKind;
use jackdaw_api_internal::{
    extensions_config::persist_current_enabled,
    lifecycle::{Extension, ExtensionCatalog},
};
use jackdaw_feathers::dialog::{CloseDialogEvent, DialogChildrenSlot, OpenDialogEvent};

use crate::extension_resolution;
use jackdaw_api_internal::lifecycle::{disable_extension, enable_extension};

pub struct ExtensionsDialogPlugin;

impl Plugin for ExtensionsDialogPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<ExtensionsDialogOpen>()
            .add_systems(Update, populate_extensions_dialog)
            .add_observer(on_dialog_closed);
    }
}

fn on_dialog_closed(_: On<CloseDialogEvent>, mut open: ResMut<ExtensionsDialogOpen>) {
    open.0 = false;
}

#[derive(Resource, Default)]
struct ExtensionsDialogOpen(bool);

/// Marks the top-level list node inside the dialog. Cascade-
/// despawned after an install succeeds so
/// `populate_extensions_dialog` rebuilds from the updated catalog.
#[derive(Component)]
struct ExtensionsDialogContent;

pub fn open_extensions_dialog(world: &mut World) {
    world.resource_mut::<ExtensionsDialogOpen>().0 = true;
    world.trigger(
        OpenDialogEvent::new("Extensions", "Close")
            .without_cancel()
            .with_max_width(Val::Px(380.0)),
    );
}

/// Fill the dialog's children slot with a row per catalog entry.
///
/// The slot is found by marker presence rather than `&Children` because
/// a freshly-spawned `DialogChildrenSlot` has no `Children` component
/// yet. The `ExtensionsDialogContent` marker on the list root guards
/// against double-populating a re-opened dialog.
fn populate_extensions_dialog(
    mut commands: Commands,
    catalog: Res<ExtensionCatalog>,
    open: Res<ExtensionsDialogOpen>,
    slots: Query<Entity, With<DialogChildrenSlot>>,
    loaded: Query<&Extension>,
    existing: Query<(), With<ExtensionsDialogContent>>,
) {
    if !open.0 {
        return;
    }
    if !existing.is_empty() {
        return;
    }
    let Some(slot_entity) = slots.iter().next() else {
        return;
    };

    // Split catalog entries into Built-in vs. Custom. Membership comes
    // from each extension's declared `ExtensionKind`.
    let enabled_names: std::collections::HashSet<String> =
        loaded.iter().map(|e| e.id.clone()).collect();
    let mut builtin_rows: Vec<(String, String, bool)> = Vec::new();
    let mut custom_rows: Vec<(String, String, bool)> = Vec::new();
    for (id, label, _description, kind) in catalog.iter_with_content() {
        // Required extensions are load-bearing (the editor panics
        // without them), so they're not user-toggleable. Omit them
        // from the dialog entirely rather than rendering a locked
        // checkbox; they're implementation detail, not a user
        // choice.
        if extension_resolution::is_required(&id) {
            continue;
        }
        let row = (
            id.to_string(),
            label.to_string(),
            enabled_names.contains(&id),
        );
        match kind {
            ExtensionKind::Builtin => builtin_rows.push(row),
            ExtensionKind::Regular => custom_rows.push(row),
        }
    }
    builtin_rows.sort_by(|a, b| a.0.cmp(&b.0));
    custom_rows.sort_by(|a, b| a.0.cmp(&b.0));

    let list = commands
        .spawn_scene(extensions_list_container())
        .insert((ChildOf(slot_entity), ExtensionsDialogContent))
        .id();

    commands
        .spawn_scene(section_header("Built-in"))
        .insert(ChildOf(list));
    for (id, label, checked) in builtin_rows {
        spawn_extension_row(&mut commands, list, id, label, checked);
    }

    commands
        .spawn_scene(section_header("Regular"))
        .insert(ChildOf(list));
    if custom_rows.is_empty() {
        commands
            .spawn_scene(empty_regular_notice())
            .insert(ChildOf(list));
    } else {
        for (id, label, checked) in custom_rows {
            spawn_extension_row(&mut commands, list, id, label, checked);
        }
    }
}

/// Spawn one extension checkbox under `list`, seeding its initial
/// `Checked` state. The checkbox carries its own `ValueChange<bool>`
/// observer (see [`extension_checkbox`]) which toggles the extension
/// and persists the enabled set.
fn spawn_extension_row(
    commands: &mut Commands,
    list: Entity,
    id: String,
    label: String,
    checked: bool,
) {
    let row = commands
        .spawn((
            Node {
                flex_direction: FlexDirection::Row,
                align_items: AlignItems::Center,
                justify_content: JustifyContent::SpaceBetween,
                ..Default::default()
            },
            ChildOf(list),
        ))
        .id();
    let mut checkbox = commands.spawn_scene(extension_checkbox(id.clone(), label));
    if checked {
        // Checkboxes don't seed their own `Checked` state, so an enabled
        // extension is marked here at spawn.
        checkbox.insert(Checked);
    }
    checkbox.insert(ChildOf(row));
}

/// The list root spawned into the dialog slot. The
/// `ExtensionsDialogContent` marker is inserted at the spawn site so an
/// install can cascade-despawn this subtree and trigger a rebuild.
fn extensions_list_container() -> impl Scene {
    bsn! {
        Node {
            flex_direction: FlexDirection::Column,
            row_gap: px(2),
            min_width: px(280),
        }
    }
}

/// A checkbox bound to one extension. The observer keeps the visual
/// `Checked` state in sync, since the checkbox doesn't self-update, and
/// runs the enable/disable and persist pipeline.
fn extension_checkbox(id: String, label: String) -> impl Scene {
    bsn! {
        @FeathersCheckbox {
            @caption: bsn! { Text(label) ThemedText }
        }
        on(move |change: On<ValueChange<bool>>, mut commands: Commands| {
            let checked = change.value;
            let source = change.source;

            // Belt-and-suspenders: required extensions shouldn't have a
            // checkbox in the first place (see `populate_extensions_dialog`),
            // but if one slipped through we refuse to disable it and keep
            // it visually enabled rather than letting the editor end up in
            // a broken state.
            if !checked && extension_resolution::is_required(&id) {
                warn!("Refusing to disable required extension `{id}`");
                commands.entity(source).insert(Checked);
                return;
            }

            jackdaw_feathers::utils::set_marker_if_alive::<Checked>(
                &mut commands,
                source,
                checked,
            );

            let name = id.clone();
            commands.queue(move |world: &mut World| {
                if checked {
                    enable_extension(world, &name);
                    // Re-apply the keymap so newly registered operator
                    // actions get bindings without requiring a restart.
                    crate::extension_lifecycle::apply_active_keymap(world);
                } else {
                    disable_extension(world, &name);
                }
                persist_current_enabled(world);
            });
        })
    }
}

/// Underlined section heading.
fn section_header(label: impl Into<String>) -> impl Scene {
    bsn! {
        Node {
            width: percent(100),
            padding: UiRect::new(px(12), px(12), px(8), px(2)),
            border: UiRect::bottom(px(1)),
        }
        ThemeBorderColor(tokens::PANE_HEADER_DIVIDER)
        Children [ @label_dim(label) ]
    }
}

/// Placeholder shown when no regular (non-built-in) extensions exist.
fn empty_regular_notice() -> impl Scene {
    bsn! {
        Node {
            padding: UiRect::axes(px(12), px(4)),
        }
        Children [ @label_dim("No regular extensions installed") ]
    }
}

