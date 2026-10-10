//! User-bindable hotkeys for toggling features without opening the menu.
//!
//! Bindings are stored in settings as winit `KeyCode` names ("F6", "KeyA")
//! and matched on the physical key, so they follow key position rather than
//! keyboard layout. Escape (menu) and F11 (fullscreen) stay reserved.

use crate::settings::Hotkeys;
use winit::keyboard::{KeyCode, PhysicalKey};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HotkeyAction {
    SuperResolution,
    ArtifactReduction,
}

impl HotkeyAction {
    pub const ALL: [Self; 2] = [Self::SuperResolution, Self::ArtifactReduction];

    pub fn label(self) -> &'static str {
        match self {
            Self::SuperResolution => "Toggle Super Resolution",
            Self::ArtifactReduction => "Toggle MJPEG artifact reduction",
        }
    }

    pub fn binding(self, hotkeys: &Hotkeys) -> &str {
        match self {
            Self::SuperResolution => &hotkeys.super_resolution,
            Self::ArtifactReduction => &hotkeys.artifact_reduction,
        }
    }

    fn binding_mut(self, hotkeys: &mut Hotkeys) -> &mut String {
        match self {
            Self::SuperResolution => &mut hotkeys.super_resolution,
            Self::ArtifactReduction => &mut hotkeys.artifact_reduction,
        }
    }
}

macro_rules! bindable_keys {
    ($($code:ident),* $(,)?) => {
        const BINDABLE: &[(KeyCode, &str)] = &[$((KeyCode::$code, stringify!($code))),*];
    };
}

bindable_keys!(
    F1, F2, F3, F4, F5, F6, F7, F8, F9, F10, F12, F13, F14, F15, F16, F17, F18, F19, F20, F21,
    F22, F23, F24, KeyA, KeyB, KeyC, KeyD, KeyE, KeyF, KeyG, KeyH, KeyI, KeyJ, KeyK, KeyL, KeyM,
    KeyN, KeyO, KeyP, KeyQ, KeyR, KeyS, KeyT, KeyU, KeyV, KeyW, KeyX, KeyY, KeyZ, Digit0, Digit1,
    Digit2, Digit3, Digit4, Digit5, Digit6, Digit7, Digit8, Digit9, Numpad0, Numpad1, Numpad2,
    Numpad3, Numpad4, Numpad5, Numpad6, Numpad7, Numpad8, Numpad9, NumpadAdd, NumpadSubtract,
    NumpadMultiply, NumpadDivide, NumpadDecimal, Insert, Delete, Home, End, PageUp, PageDown,
    Pause, ScrollLock, Backquote, Minus, Equal, BracketLeft, BracketRight, Backslash, Semicolon,
    Quote, Comma, Period, Slash, Space, Tab,
);

/// The stored name for a key, or `None` if it can't be bound.
pub fn key_name(key: PhysicalKey) -> Option<&'static str> {
    let PhysicalKey::Code(code) = key else { return None };
    BINDABLE.iter().find(|(bindable, _)| *bindable == code).map(|(_, name)| *name)
}

/// Human-readable form of a stored binding.
pub fn display_name(binding: &str) -> String {
    if binding.is_empty() {
        return "Unbound".to_owned();
    }
    if let Some(letter) = binding.strip_prefix("Key") {
        return letter.to_owned();
    }
    if let Some(digit) = binding.strip_prefix("Digit") {
        return digit.to_owned();
    }
    if let Some(rest) = binding.strip_prefix("Numpad") {
        return format!("Numpad {rest}");
    }
    binding.to_owned()
}

/// The action bound to `key`, if any.
pub fn action_for(hotkeys: &Hotkeys, key: PhysicalKey) -> Option<HotkeyAction> {
    let name = key_name(key)?;
    HotkeyAction::ALL.into_iter().find(|action| action.binding(hotkeys) == name)
}

/// Binds `action` to `name`. A key bound to another action moves to this one
/// and the other action takes this action's old key, so no key does two things.
pub fn rebind(hotkeys: &mut Hotkeys, action: HotkeyAction, name: &str) {
    let previous = action.binding(hotkeys).to_owned();
    for other in HotkeyAction::ALL {
        if other != action && other.binding(hotkeys) == name {
            *other.binding_mut(hotkeys) = previous.clone();
        }
    }
    *action.binding_mut(hotkeys) = name.to_owned();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_f6_and_f7() {
        let hotkeys = Hotkeys::default();
        assert_eq!(action_for(&hotkeys, PhysicalKey::Code(KeyCode::F6)), Some(HotkeyAction::SuperResolution));
        assert_eq!(action_for(&hotkeys, PhysicalKey::Code(KeyCode::F7)), Some(HotkeyAction::ArtifactReduction));
        assert_eq!(action_for(&hotkeys, PhysicalKey::Code(KeyCode::F8)), None);
    }

    #[test]
    fn reserved_keys_cannot_be_bound() {
        assert_eq!(key_name(PhysicalKey::Code(KeyCode::Escape)), None);
        assert_eq!(key_name(PhysicalKey::Code(KeyCode::F11)), None);
        assert_eq!(key_name(PhysicalKey::Code(KeyCode::KeyV)), Some("KeyV"));
        assert_eq!(display_name("KeyV"), "V");
        assert_eq!(display_name("Numpad5"), "Numpad 5");
    }

    #[test]
    fn rebinding_to_a_used_key_swaps_the_bindings() {
        let mut hotkeys = Hotkeys::default();
        rebind(&mut hotkeys, HotkeyAction::SuperResolution, "F7");
        assert_eq!(hotkeys.super_resolution, "F7");
        assert_eq!(hotkeys.artifact_reduction, "F6");
        rebind(&mut hotkeys, HotkeyAction::ArtifactReduction, "KeyR");
        assert_eq!(hotkeys.artifact_reduction, "KeyR");
        assert_eq!(hotkeys.super_resolution, "F7");
    }
}
