//! The editor's dialog protocol: open a question with [`OpenDialogEvent`], hear
//! the answer as [`DialogActionEvent`], [`DialogSecondaryActionEvent`] or
//! [`DialogDismissEvent`]. The dialog itself is `bevy_feathers`'
//! [`FeathersDialog`], whose barrier and close button ask to close with
//! [`RequestClose`].

use bevy::feathers::controls::{
    FeathersDialog, FeathersDialogBody, FeathersDialogClose, FeathersDialogFooter,
    FeathersDialogHeader,
};
use bevy::feathers::theme::ThemedText;
use bevy::prelude::*;
use bevy::ui_widgets::{ModalDialog, RequestClose};
use jackdaw_commands::KeymapCapture;

use crate::button::{ButtonClickEvent, ButtonProps, ButtonVariant, button};
use crate::icons::EditorFont;
use crate::tokens::{TEXT_MUTED_COLOR, TEXT_SIZE_LG};

/// Over the editor's panels and popovers' own layers, as the hand-built
/// dialog sat.
const DIALOG_Z: i32 = 200;

pub fn plugin(app: &mut App) {
    app.add_observer(on_open_dialog)
        .add_observer(on_open_confirmation_dialog)
        .add_observer(on_action_button_click)
        .add_observer(on_secondary_action_button_click)
        .add_observer(on_cancel_button_click)
        .add_observer(on_close_dialog)
        .add_systems(Update, (sync_children_slot_visibility, handle_esc_key));
}

#[derive(Component)]
pub struct EditorDialog;

/// What a dialog is asking and what it will take for an answer.
///
/// The footer's buttons carry their labels in `Text` children several levels
/// down, which is enough to draw them and not enough to answer one from anywhere
/// but a pointer. Recorded here at open time so a caller with no pointer can read
/// the question and name a button.
#[derive(Component, Clone, Debug, Default, PartialEq, Eq)]
pub struct DialogChoices {
    pub title: Option<String>,
    pub description: Option<String>,
    /// The primary button, on the right.
    pub action: Option<String>,
    /// The secondary button, on the far left, when there is one.
    pub secondary: Option<String>,
    /// The cancel button. Dismissing the dialog any other way (Esc, a
    /// click outside, the close cross) means the same thing.
    pub cancel: Option<String>,
}

impl DialogChoices {
    /// The buttons a caller may name, in the order their indices run:
    /// the primary action first, because that is the answer a dialog is
    /// asking for.
    pub fn labels(&self) -> Vec<String> {
        [&self.action, &self.secondary, &self.cancel]
            .into_iter()
            .flatten()
            .cloned()
            .collect()
    }
}

/// Which button of a dialog an answer means.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DialogChoice {
    Action,
    Secondary,
    Cancel,
}

/// Why [`resolve_dialog_choice`] pressed nothing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DialogChoiceError {
    /// Nothing on the dialog is spelled or numbered like this.
    NoMatch,
    /// The prefix fits more than one button, named here in the order the
    /// dialog offers them. Pressing one of them would be a guess, so the
    /// caller is asked to say which.
    Ambiguous(Vec<String>),
}

impl core::fmt::Display for DialogChoiceError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::NoMatch => write!(f, "no button on this dialog answers to it"),
            Self::Ambiguous(candidates) => {
                write!(f, "it is a prefix of more than one button: {candidates:?}")
            }
        }
    }
}

/// The button `choice` names: a label, or an index into
/// [`DialogChoices::labels`].
///
/// A label matches exactly first, so a button literally spelled `1` is reachable
/// by name. Failing that, a value that parses as an integer is an index. Failing
/// that, one case-insensitive prefix match answers, so `Reload` presses a button
/// spelled `Reload (discards your unsaved changes)`.
///
/// A prefix that fits two buttons presses neither and comes back as
/// [`DialogChoiceError::Ambiguous`] naming both.
pub fn resolve_dialog_choice(
    choices: &DialogChoices,
    choice: &str,
) -> Result<DialogChoice, DialogChoiceError> {
    let slots = [
        (DialogChoice::Action, choices.action.as_deref()),
        (DialogChoice::Secondary, choices.secondary.as_deref()),
        (DialogChoice::Cancel, choices.cancel.as_deref()),
    ];
    let present: Vec<(DialogChoice, &str)> = slots
        .iter()
        .filter_map(|(which, label)| label.map(|label| (*which, label)))
        .collect();

    let wanted = choice.trim();
    if let Some((which, _)) = present.iter().find(|(_, label)| *label == wanted) {
        return Ok(*which);
    }
    if let Ok(index) = wanted.parse::<usize>() {
        return present
            .get(index)
            .map(|(which, _)| *which)
            .ok_or(DialogChoiceError::NoMatch);
    }

    let lowered = wanted.to_lowercase();
    let matches: Vec<&(DialogChoice, &str)> = present
        .iter()
        .filter(|(_, label)| label.to_lowercase().starts_with(&lowered))
        .collect();
    match matches.as_slice() {
        [(which, _)] => Ok(*which),
        [] => Err(DialogChoiceError::NoMatch),
        many => Err(DialogChoiceError::Ambiguous(
            many.iter().map(|(_, label)| (*label).to_string()).collect(),
        )),
    }
}

/// Press one of `dialog`'s buttons, as a click on it would: fire the event the
/// button fires, then take the dialog down. Cancel fires
/// [`DialogDismissEvent`], which is what makes a dismissal and a cancel the
/// same answer.
pub fn answer_dialog(commands: &mut Commands, dialog: Entity, choice: DialogChoice) {
    match choice {
        DialogChoice::Action => {
            commands.trigger(DialogActionEvent { entity: dialog });
            dismiss_dialog(commands, dialog);
        }
        DialogChoice::Secondary => {
            commands.trigger(DialogSecondaryActionEvent { entity: dialog });
            dismiss_dialog(commands, dialog);
        }
        DialogChoice::Cancel => cancel_dialog(commands, dialog),
    }
}

#[derive(Component)]
struct DialogCancelButton;

#[derive(Component)]
struct DialogActionButton;

#[derive(Component)]
struct DialogSecondaryActionButton;

#[derive(Component, Clone, Default)]
pub struct DialogChildrenSlot;

#[derive(Component, Default, Clone, Copy)]
pub enum DialogVariant {
    #[default]
    Default,
    Destructive,
}

impl DialogVariant {
    fn action_button_variant(&self) -> ButtonVariant {
        match self {
            Self::Default => ButtonVariant::Primary,
            Self::Destructive => ButtonVariant::Destructive,
        }
    }
}

#[derive(Component)]
struct DialogConfig {
    close_on_click_outside: bool,
    close_on_esc: bool,
}

#[derive(EntityEvent)]
pub struct DialogActionEvent {
    pub entity: Entity,
}

#[derive(EntityEvent)]
pub struct DialogSecondaryActionEvent {
    pub entity: Entity,
}

/// A dialog going down unanswered: cancelled, closed, dismissed with Escape or
/// a click outside, or closed from code.
#[derive(EntityEvent)]
pub struct DialogDismissEvent {
    pub entity: Entity,
}

#[derive(Event)]
pub struct CloseDialogEvent;

#[derive(Event)]
pub struct OpenDialogEvent {
    pub title: Option<String>,
    pub description: Option<String>,
    pub action: Option<String>,
    pub secondary_action: Option<String>,
    pub cancel: Option<String>,
    pub variant: DialogVariant,
    pub has_close_button: bool,
    pub close_on_click_outside: bool,
    pub close_on_esc: bool,
    pub max_width: Option<Val>,
    pub content_padding: UiRect,
}

impl OpenDialogEvent {
    pub fn new(title: impl Into<String>, action: impl Into<String>) -> Self {
        Self {
            title: Some(title.into()),
            description: None,
            action: Some(action.into()),
            secondary_action: None,
            cancel: Some("Cancel".into()),
            variant: DialogVariant::Default,
            has_close_button: true,
            close_on_click_outside: true,
            close_on_esc: true,
            max_width: None,
            content_padding: UiRect::all(px(24)),
        }
    }

    pub fn without_cancel(mut self) -> Self {
        self.cancel = None;
        self
    }

    pub fn with_secondary_action(mut self, label: impl Into<String>) -> Self {
        self.secondary_action = Some(label.into());
        self
    }

    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    pub fn with_variant(mut self, variant: DialogVariant) -> Self {
        self.variant = variant;
        self
    }

    pub fn with_close_button(mut self, has_close_button: bool) -> Self {
        self.has_close_button = has_close_button;
        self
    }

    pub fn with_close_on_click_outside(mut self, close_on_click_outside: bool) -> Self {
        self.close_on_click_outside = close_on_click_outside;
        self
    }

    pub fn with_max_width(mut self, max_width: Val) -> Self {
        self.max_width = Some(max_width);
        self
    }

    pub fn without_content_padding(mut self) -> Self {
        self.content_padding = UiRect::ZERO;
        self
    }
}

#[derive(Event)]
pub struct OpenConfirmationDialogEvent {
    pub title: String,
    pub description: Option<String>,
    pub action: String,
}

impl OpenConfirmationDialogEvent {
    pub fn new(title: impl Into<String>, action: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            description: None,
            action: action.into(),
        }
    }

    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }
}

impl From<&OpenConfirmationDialogEvent> for OpenDialogEvent {
    fn from(event: &OpenConfirmationDialogEvent) -> Self {
        let mut dialog = OpenDialogEvent::new(&event.title, &event.action)
            .with_variant(DialogVariant::Destructive)
            .with_close_button(false)
            .with_close_on_click_outside(false);
        dialog.description = event.description.clone();
        dialog
    }
}

fn on_open_dialog(
    event: On<OpenDialogEvent>,
    mut commands: Commands,
    existing: Query<Entity, With<EditorDialog>>,
) {
    if !existing.is_empty() {
        return;
    }
    spawn_dialog(&mut commands, &event);
}

fn on_open_confirmation_dialog(
    event: On<OpenConfirmationDialogEvent>,
    mut commands: Commands,
    existing: Query<Entity, With<EditorDialog>>,
) {
    if !existing.is_empty() {
        return;
    }
    let dialog_event: OpenDialogEvent = event.event().into();
    spawn_dialog(&mut commands, &dialog_event);
}

fn spawn_dialog(commands: &mut Commands, event: &OpenDialogEvent) {
    let title = event.title.clone();
    let description = event.description.clone();
    let action = event.action.clone();
    let secondary = event.secondary_action.clone();
    let cancel = event.cancel.clone();
    let variant = event.variant;
    let has_close_button = event.has_close_button;
    let max_width = event.max_width.unwrap_or(px(448));
    let content_padding = event.content_padding;
    let choices = DialogChoices {
        title: title.clone(),
        description: description.clone(),
        action: action.clone(),
        secondary: secondary.clone(),
        cancel: cancel.clone(),
    };
    let config = DialogConfig {
        close_on_click_outside: event.close_on_click_outside,
        close_on_esc: event.close_on_esc,
    };

    commands.queue(move |world: &mut World| {
        let root = match world.spawn_scene(bsn! { @FeathersDialog GlobalZIndex(DIALOG_Z) }) {
            Ok(mut root) => {
                root.insert((EditorDialog, choices, variant, config))
                    .observe(on_request_close);
                root.id()
            }
            Err(error) => {
                error!("a dialog did not spawn: {error}");
                return;
            }
        };
        let Some(panel) = world.entity(root).get::<Children>().and_then(|children| {
            children
                .iter()
                .find(|&child| world.entity(child).contains::<ModalDialog>())
        }) else {
            return;
        };
        if let Some(mut node) = world.get_mut::<Node>(panel) {
            node.width = percent(100);
            node.max_width = max_width;
        }

        if title.is_some() || has_close_button {
            let header = spawn_part(world, panel, bsn! { @FeathersDialogHeader });
            if let Some(title) = title {
                spawn_part(world, header, bsn! { Text(title) ThemedText });
            }
            if has_close_button {
                spawn_part(world, header, bsn! { @FeathersDialogClose });
            }
        }

        if let Some(description) = description {
            let body = spawn_part(world, panel, bsn! { @FeathersDialogBody });
            let font = world
                .get_resource::<EditorFont>()
                .map(|font| font.0.clone())
                .unwrap_or_default();
            world.spawn((
                Text::new(description),
                TextFont {
                    font: font.into(),
                    font_size: TEXT_SIZE_LG,
                    ..default()
                },
                TextColor(TEXT_MUTED_COLOR.into()),
                ChildOf(body),
            ));
        }

        let slot = spawn_part(world, panel, bsn! { @FeathersDialogBody });
        if let Some(mut node) = world.get_mut::<Node>(slot) {
            node.display = Display::None;
            node.padding = content_padding;
            node.row_gap = px(12);
        }
        world.entity_mut(slot).insert(DialogChildrenSlot);

        if action.is_none() && cancel.is_none() && secondary.is_none() {
            return;
        }
        let footer = spawn_part(world, panel, bsn! { @FeathersDialogFooter });
        // Secondary action on the far left; the wrapper's auto margin pushes it there.
        if let Some(secondary) = secondary {
            let wrapper = world
                .spawn((
                    Node {
                        margin: UiRect::right(Val::Auto),
                        ..default()
                    },
                    ChildOf(footer),
                ))
                .id();
            world.spawn((
                DialogSecondaryActionButton,
                button(ButtonProps::new(secondary)),
                ChildOf(wrapper),
            ));
        }
        if let Some(cancel) = cancel {
            world.spawn((
                DialogCancelButton,
                button(ButtonProps::new(cancel)),
                ChildOf(footer),
            ));
        }
        if let Some(action) = action {
            world.spawn((
                DialogActionButton,
                button(ButtonProps::new(action).with_variant(variant.action_button_variant())),
                ChildOf(footer),
            ));
        }
    });
}

/// Spawn one section of a dialog under `parent`, or a bare node if the scene
/// will not resolve, so the rest of the dialog still has somewhere to go.
fn spawn_part(world: &mut World, parent: Entity, scene: impl Scene) -> Entity {
    match world.spawn_scene(scene) {
        Ok(mut part) => part.insert(ChildOf(parent)).id(),
        Err(error) => {
            error!("a dialog section did not spawn: {error}");
            world.spawn((Node::default(), ChildOf(parent))).id()
        }
    }
}

/// The barrier (a click outside, or Escape with focus inside) and the close
/// button ask to close. A dialog that does not close on a click outside
/// ignores its barrier; Escape still reaches [`handle_esc_key`].
fn on_request_close(
    mut close: On<RequestClose>,
    dialogs: Query<&DialogConfig, With<EditorDialog>>,
    capture: Option<Res<KeymapCapture>>,
    mut commands: Commands,
) {
    close.propagate(false);
    let dialog = close.event_target();
    let Ok(config) = dialogs.get(dialog) else {
        return;
    };
    if KeymapCapture::is_recording(capture.as_deref()) {
        return;
    }
    if close.original_event_target() == dialog && !config.close_on_click_outside {
        return;
    }
    cancel_dialog(&mut commands, dialog);
}

fn dismiss_dialog(commands: &mut Commands, entity: Entity) {
    commands.entity(entity).try_despawn();
}

/// Take a dialog down with its question unanswered, telling whoever put it up.
/// Escape can reach both the barrier and [`handle_esc_key`]; only the first
/// to land answers.
fn cancel_dialog(commands: &mut Commands, entity: Entity) {
    commands.queue(move |world: &mut World| {
        if !world
            .get_entity(entity)
            .is_ok_and(|dialog| dialog.contains::<EditorDialog>())
        {
            return;
        }
        world.trigger(DialogDismissEvent { entity });
        if let Ok(dialog) = world.get_entity_mut(entity) {
            dialog.despawn();
        }
    });
}

fn sync_children_slot_visibility(
    mut slots: Query<(&Children, &mut Node), (With<DialogChildrenSlot>, Changed<Children>)>,
) {
    for (children, mut node) in &mut slots {
        node.display = if children.is_empty() {
            Display::None
        } else {
            Display::Flex
        };
    }
}

/// Escape closes a dialog -- except while the keybind settings are recording, when
/// the press belongs to the recorder. Without this Escape could never be bound to
/// anything.
fn handle_esc_key(
    keyboard: Res<ButtonInput<KeyCode>>,
    capture: Option<Res<KeymapCapture>>,
    dialogs: Query<(Entity, &DialogConfig), With<EditorDialog>>,
    mut commands: Commands,
) {
    if KeymapCapture::is_recording(capture.as_deref()) {
        return;
    }
    if !keyboard.just_pressed(KeyCode::Escape) {
        return;
    }

    for (entity, config) in &dialogs {
        if config.close_on_esc {
            cancel_dialog(&mut commands, entity);
        }
    }
}

fn on_close_dialog(
    _event: On<CloseDialogEvent>,
    dialogs: Query<Entity, With<EditorDialog>>,
    mut commands: Commands,
) {
    for entity in &dialogs {
        cancel_dialog(&mut commands, entity);
    }
}

fn on_action_button_click(
    event: On<ButtonClickEvent>,
    action_buttons: Query<&ChildOf, With<DialogActionButton>>,
    parents: Query<&ChildOf>,
    dialogs: Query<Entity, With<EditorDialog>>,
    mut commands: Commands,
) {
    let Ok(button_parent) = action_buttons.get(event.entity) else {
        return;
    };

    if let Some(dialog_entity) = find_dialog_ancestor(button_parent.parent(), &parents, &dialogs) {
        commands.trigger(DialogActionEvent {
            entity: dialog_entity,
        });
        dismiss_dialog(&mut commands, dialog_entity);
    }
}

fn on_secondary_action_button_click(
    event: On<ButtonClickEvent>,
    secondary_buttons: Query<&ChildOf, With<DialogSecondaryActionButton>>,
    parents: Query<&ChildOf>,
    dialogs: Query<Entity, With<EditorDialog>>,
    mut commands: Commands,
) {
    let Ok(button_parent) = secondary_buttons.get(event.entity) else {
        return;
    };

    if let Some(dialog_entity) = find_dialog_ancestor(button_parent.parent(), &parents, &dialogs) {
        commands.trigger(DialogSecondaryActionEvent {
            entity: dialog_entity,
        });
        dismiss_dialog(&mut commands, dialog_entity);
    }
}

fn on_cancel_button_click(
    event: On<ButtonClickEvent>,
    cancel_buttons: Query<&ChildOf, With<DialogCancelButton>>,
    parents: Query<&ChildOf>,
    dialogs: Query<Entity, With<EditorDialog>>,
    mut commands: Commands,
) {
    let Ok(button_parent) = cancel_buttons.get(event.entity) else {
        return;
    };

    if let Some(dialog_entity) = find_dialog_ancestor(button_parent.parent(), &parents, &dialogs) {
        cancel_dialog(&mut commands, dialog_entity);
    }
}

fn find_dialog_ancestor(
    start: Entity,
    parents: &Query<&ChildOf>,
    dialogs: &Query<Entity, With<EditorDialog>>,
) -> Option<Entity> {
    let mut current = start;
    loop {
        if dialogs.get(current).is_ok() {
            return Some(current);
        }
        let Ok(child_of) = parents.get(current) else {
            return None;
        };
        current = child_of.parent();
    }
}
