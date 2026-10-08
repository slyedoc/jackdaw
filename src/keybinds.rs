use bevy::prelude::*;

/// The modifiers a chord names for itself.
///
/// A binding says nothing about the modifiers it leaves out, so anything
/// not named here holds the chord back; see [`unwanted_modifier`].
#[derive(Clone, Copy, Default)]
pub(crate) struct ChordModifiers {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
}

/// Whether a modifier the chord did not name is being held.
///
/// `bevy_enhanced_input` matches a binding on the modifiers it *names* and
/// says nothing about the ones it does not, so a binding on a bare key
/// answers a chord built on that key as well: Ctrl+C started a cut brush,
/// and Ctrl+Shift+Z ran Undo alongside Redo. Every chord that shares a key
/// with a longer one asks this before acting.
pub(crate) fn unwanted_modifier(keyboard: &ButtonInput<KeyCode>, named: ChordModifiers) -> bool {
    let held = |wanted: bool, keys: [KeyCode; 2]| !wanted && keyboard.any_pressed(keys);
    held(named.ctrl, [KeyCode::ControlLeft, KeyCode::ControlRight])
        || held(named.ctrl, [KeyCode::SuperLeft, KeyCode::SuperRight])
        || held(named.alt, [KeyCode::AltLeft, KeyCode::AltRight])
        || held(named.shift, [KeyCode::ShiftLeft, KeyCode::ShiftRight])
}
