//! UU CONTROL input (JSON on the CONTROL channel) translated to One-KVM HID events.
//! Keycodes follow the advertised host system (Windows VK or macOS virtual keycode); mouse positions
//! are normalized 0..1 to the source screen; buttons are bitmasks (1 left, 2 right, 4 middle).

use std::collections::BTreeSet;

use crate::config::UuRemoteSystem;
use serde_json::Value;

use crate::hid::{
    CanonicalKey, KeyEventType, KeyboardEvent, KeyboardModifiers, MouseButton, MouseEvent,
    MouseEventType,
};

#[derive(Debug, Clone, PartialEq)]
pub enum ControlInput {
    Keyboard { key: i32, pressed: bool },
    KeyboardClick { key: i32 },
    MouseButton { button: i32, pressed: bool },
    MouseClick { button: i32 },
    MouseAbsolute { x: f64, y: f64 },
    MouseRelative { dx: i32, dy: i32 },
    MouseScroll { dx: i32, dy: i32 },
}

fn integer(v: &Value, name: &str) -> Option<i32> {
    v.get(name)?.as_i64().and_then(|n| i32::try_from(n).ok())
}

fn coordinate(v: &Value, name: &str) -> Option<f64> {
    v.get(name)?.as_f64().filter(|n| n.is_finite())
}

/// Decode `{"action": ...}` input JSON; keyboard heartbeats and unknown actions yield None.
pub fn decode(data: &[u8]) -> Option<ControlInput> {
    let v: Value = serde_json::from_slice(data).ok()?;
    let event = match v.get("action")?.as_str()? {
        "kbd_press" => ControlInput::Keyboard {
            key: integer(&v, "key")?,
            pressed: true,
        },
        "kbd_release" => ControlInput::Keyboard {
            key: integer(&v, "key")?,
            pressed: false,
        },
        "kbd_click" => ControlInput::KeyboardClick {
            key: integer(&v, "key")?,
        },
        "mouse_press" => ControlInput::MouseButton {
            button: integer(&v, "button")?,
            pressed: true,
        },
        "mouse_release" => ControlInput::MouseButton {
            button: integer(&v, "button")?,
            pressed: false,
        },
        "mouse_click" => ControlInput::MouseClick {
            button: integer(&v, "button")?,
        },
        "mouse_move_absolute" => ControlInput::MouseAbsolute {
            x: coordinate(&v, "abs_x")?,
            y: coordinate(&v, "abs_y")?,
        },
        "mouse_move_relative" => ControlInput::MouseRelative {
            dx: integer(&v, "delta_x")?,
            dy: integer(&v, "delta_y")?,
        },
        "mouse_scroll" => ControlInput::MouseScroll {
            dx: integer(&v, "delta_x")?,
            dy: integer(&v, "delta_y")?,
        },
        _ => return None,
    };
    Some(event)
}

/// Windows virtual-key code to the physical HID key at the US-layout position.
pub fn windows_vk_to_key(vk: i32) -> Option<CanonicalKey> {
    use CanonicalKey as K;
    const LETTERS: [K; 26] = [
        K::KeyA,
        K::KeyB,
        K::KeyC,
        K::KeyD,
        K::KeyE,
        K::KeyF,
        K::KeyG,
        K::KeyH,
        K::KeyI,
        K::KeyJ,
        K::KeyK,
        K::KeyL,
        K::KeyM,
        K::KeyN,
        K::KeyO,
        K::KeyP,
        K::KeyQ,
        K::KeyR,
        K::KeyS,
        K::KeyT,
        K::KeyU,
        K::KeyV,
        K::KeyW,
        K::KeyX,
        K::KeyY,
        K::KeyZ,
    ];
    const DIGITS: [K; 10] = [
        K::Digit0,
        K::Digit1,
        K::Digit2,
        K::Digit3,
        K::Digit4,
        K::Digit5,
        K::Digit6,
        K::Digit7,
        K::Digit8,
        K::Digit9,
    ];
    const NUMPAD: [K; 10] = [
        K::Numpad0,
        K::Numpad1,
        K::Numpad2,
        K::Numpad3,
        K::Numpad4,
        K::Numpad5,
        K::Numpad6,
        K::Numpad7,
        K::Numpad8,
        K::Numpad9,
    ];
    const FUNCTION: [K; 24] = [
        K::F1,
        K::F2,
        K::F3,
        K::F4,
        K::F5,
        K::F6,
        K::F7,
        K::F8,
        K::F9,
        K::F10,
        K::F11,
        K::F12,
        K::F13,
        K::F14,
        K::F15,
        K::F16,
        K::F17,
        K::F18,
        K::F19,
        K::F20,
        K::F21,
        K::F22,
        K::F23,
        K::F24,
    ];
    let key = match vk {
        0x30..=0x39 => DIGITS[(vk - 0x30) as usize],
        0x41..=0x5a => LETTERS[(vk - 0x41) as usize],
        0x60..=0x69 => NUMPAD[(vk - 0x60) as usize],
        0x70..=0x87 => FUNCTION[(vk - 0x70) as usize],
        0x08 => K::Backspace,
        0x09 => K::Tab,
        0x0d => K::Enter,
        0x10 | 0xa0 => K::ShiftLeft,
        0xa1 => K::ShiftRight,
        0x11 | 0xa2 => K::ControlLeft,
        0xa3 => K::ControlRight,
        0x12 | 0xa4 => K::AltLeft,
        0xa5 => K::AltRight,
        0x13 => K::Pause,
        0x14 => K::CapsLock,
        0x1b => K::Escape,
        0x20 => K::Space,
        0x21 => K::PageUp,
        0x22 => K::PageDown,
        0x23 => K::End,
        0x24 => K::Home,
        0x25 => K::ArrowLeft,
        0x26 => K::ArrowUp,
        0x27 => K::ArrowRight,
        0x28 => K::ArrowDown,
        0x2c => K::PrintScreen,
        0x2d => K::Insert,
        0x2e => K::Delete,
        0x5b => K::MetaLeft,
        0x5c => K::MetaRight,
        0x5d => K::ContextMenu,
        0x6a => K::NumpadMultiply,
        0x6b => K::NumpadAdd,
        0x6d => K::NumpadSubtract,
        0x6e => K::NumpadDecimal,
        0x6f => K::NumpadDivide,
        0x90 => K::NumLock,
        0x91 => K::ScrollLock,
        0xba => K::Semicolon,
        0xbb => K::Equal,
        0xbc => K::Comma,
        0xbd => K::Minus,
        0xbe => K::Period,
        0xbf => K::Slash,
        0xc0 => K::Backquote,
        0xdb => K::BracketLeft,
        0xdc => K::Backslash,
        0xdd => K::BracketRight,
        0xde => K::Quote,
        // VK_OEM_102 is the ISO key between left Shift and Z: a fixed physical position.
        0xe2 => K::IntlBackslash,
        _ => return None,
    };
    Some(key)
}

/// Physical macOS virtual keycodes from HIToolbox Events.h, not Unicode or Windows VK.
/// Unknown codes must not fall back to Windows: the namespaces overlap almost entirely.
fn macos_keycode_to_key(code: i32) -> Option<CanonicalKey> {
    use CanonicalKey as K;
    Some(match code {
        0x00 => K::KeyA,
        0x01 => K::KeyS,
        0x02 => K::KeyD,
        0x03 => K::KeyF,
        0x04 => K::KeyH,
        0x05 => K::KeyG,
        0x06 => K::KeyZ,
        0x07 => K::KeyX,
        0x08 => K::KeyC,
        0x09 => K::KeyV,
        0x0a => K::IntlBackslash,
        0x0b => K::KeyB,
        0x0c => K::KeyQ,
        0x0d => K::KeyW,
        0x0e => K::KeyE,
        0x0f => K::KeyR,
        0x10 => K::KeyY,
        0x11 => K::KeyT,
        0x12 => K::Digit1,
        0x13 => K::Digit2,
        0x14 => K::Digit3,
        0x15 => K::Digit4,
        0x16 => K::Digit6,
        0x17 => K::Digit5,
        0x18 => K::Equal,
        0x19 => K::Digit9,
        0x1a => K::Digit7,
        0x1b => K::Minus,
        0x1c => K::Digit8,
        0x1d => K::Digit0,
        0x1e => K::BracketRight,
        0x1f => K::KeyO,
        0x20 => K::KeyU,
        0x21 => K::BracketLeft,
        0x22 => K::KeyI,
        0x23 => K::KeyP,
        0x24 => K::Enter,
        0x25 => K::KeyL,
        0x26 => K::KeyJ,
        0x27 => K::Quote,
        0x28 => K::KeyK,
        0x29 => K::Semicolon,
        0x2a => K::Backslash,
        0x2b => K::Comma,
        0x2c => K::Slash,
        0x2d => K::KeyN,
        0x2e => K::KeyM,
        0x2f => K::Period,
        0x30 => K::Tab,
        0x31 => K::Space,
        0x32 => K::Backquote,
        0x33 => K::Backspace,
        0x35 => K::Escape,
        0x36 => K::MetaRight,
        0x37 => K::MetaLeft,
        0x38 => K::ShiftLeft,
        0x39 => K::CapsLock,
        0x3a => K::AltLeft,
        0x3b => K::ControlLeft,
        0x3c => K::ShiftRight,
        0x3d => K::AltRight,
        0x3e => K::ControlRight,
        0x40 => K::F17,
        0x41 => K::NumpadDecimal,
        0x43 => K::NumpadMultiply,
        0x45 => K::NumpadAdd,
        0x47 => K::NumLock,
        0x4b => K::NumpadDivide,
        0x4c => K::NumpadEnter,
        0x4e => K::NumpadSubtract,
        0x4f => K::F18,
        0x50 => K::F19,
        0x52 => K::Numpad0,
        0x53 => K::Numpad1,
        0x54 => K::Numpad2,
        0x55 => K::Numpad3,
        0x56 => K::Numpad4,
        0x57 => K::Numpad5,
        0x58 => K::Numpad6,
        0x59 => K::Numpad7,
        0x5a => K::F20,
        0x5b => K::Numpad8,
        0x5c => K::Numpad9,
        0x60 => K::F5,
        0x61 => K::F6,
        0x62 => K::F7,
        0x63 => K::F3,
        0x64 => K::F8,
        0x65 => K::F9,
        0x67 => K::F11,
        0x69 => K::F13,
        0x6a => K::F16,
        0x6b => K::F14,
        0x6d => K::F10,
        0x6e => K::ContextMenu,
        0x6f => K::F12,
        0x71 => K::F15,
        0x72 => K::Insert,
        0x73 => K::Home,
        0x74 => K::PageUp,
        0x75 => K::Delete,
        0x76 => K::F4,
        0x77 => K::End,
        0x78 => K::F2,
        0x79 => K::PageDown,
        0x7a => K::F1,
        0x7b => K::ArrowLeft,
        0x7c => K::ArrowRight,
        0x7d => K::ArrowDown,
        0x7e => K::ArrowUp,
        _ => return None,
    })
}

fn wire_button(button: i32) -> Option<MouseButton> {
    match button {
        1 => Some(MouseButton::Left),
        2 => Some(MouseButton::Right),
        4 => Some(MouseButton::Middle),
        _ => None,
    }
}

/// Normalized 0..1 to the 0..32767 absolute HID range.
fn absolute(value: f64) -> Option<i32> {
    (0.0..=1.0)
        .contains(&value)
        .then(|| (value * 32767.0).round() as i32)
}

fn modifiers_from_bits(bits: u8) -> KeyboardModifiers {
    KeyboardModifiers {
        left_ctrl: bits & 0x01 != 0,
        left_shift: bits & 0x02 != 0,
        left_alt: bits & 0x04 != 0,
        left_meta: bits & 0x08 != 0,
        right_ctrl: bits & 0x10 != 0,
        right_shift: bits & 0x20 != 0,
        right_alt: bits & 0x40 != 0,
        right_meta: bits & 0x80 != 0,
    }
}

pub enum HidAction {
    Keyboard(KeyboardEvent),
    Mouse(MouseEvent),
}

/// Per-connection input state: the HID backend replaces the modifier byte on every
/// ordinary key, so the session tracks held modifiers, keys and buttons to release them.
#[derive(Default)]
pub struct InputState {
    system: UuRemoteSystem,
    reverse_scroll: bool,
    modifiers: u8,
    keys: BTreeSet<u8>,
    buttons: BTreeSet<u8>,
    scroll_remainder: i32,
}

impl InputState {
    pub fn new(system: UuRemoteSystem, reverse_scroll: bool) -> Self {
        Self {
            system,
            reverse_scroll,
            ..Self::default()
        }
    }

    fn wire_key(&self, code: i32) -> Option<CanonicalKey> {
        match self.system {
            UuRemoteSystem::Windows => windows_vk_to_key(code),
            UuRemoteSystem::Macos => macos_keycode_to_key(code),
        }
    }

    fn key(&mut self, key: CanonicalKey, pressed: bool) -> KeyboardEvent {
        if let Some(bit) = key.modifier_bit() {
            if pressed {
                self.modifiers |= bit
            } else {
                self.modifiers &= !bit
            }
        } else if pressed {
            self.keys.insert(key.to_hid_usage());
        } else {
            self.keys.remove(&key.to_hid_usage());
        }
        KeyboardEvent {
            event_type: if pressed {
                KeyEventType::Down
            } else {
                KeyEventType::Up
            },
            key,
            modifiers: modifiers_from_bits(self.modifiers),
        }
    }

    fn button(&mut self, button: MouseButton, pressed: bool) -> MouseEvent {
        let bit = button.to_hid_bit();
        if pressed {
            self.buttons.insert(bit)
        } else {
            self.buttons.remove(&bit)
        };
        MouseEvent {
            event_type: if pressed {
                MouseEventType::Down
            } else {
                MouseEventType::Up
            },
            x: 0,
            y: 0,
            button: Some(button),
            scroll: 0,
        }
    }

    pub fn translate(&mut self, input: &ControlInput) -> Vec<HidAction> {
        let mut out = Vec::new();
        match *input {
            ControlInput::Keyboard { key, pressed } => {
                if let Some(key) = self.wire_key(key) {
                    out.push(HidAction::Keyboard(self.key(key, pressed)));
                }
            }
            ControlInput::KeyboardClick { key } => {
                if let Some(key) = self.wire_key(key) {
                    out.push(HidAction::Keyboard(self.key(key, true)));
                    out.push(HidAction::Keyboard(self.key(key, false)));
                }
            }
            ControlInput::MouseButton { button, pressed } => {
                if let Some(button) = wire_button(button) {
                    out.push(HidAction::Mouse(self.button(button, pressed)));
                }
            }
            ControlInput::MouseClick { button } => {
                if let Some(button) = wire_button(button) {
                    out.push(HidAction::Mouse(self.button(button, true)));
                    out.push(HidAction::Mouse(self.button(button, false)));
                }
            }
            ControlInput::MouseAbsolute { x, y } => {
                if let (Some(x), Some(y)) = (absolute(x), absolute(y)) {
                    out.push(HidAction::Mouse(MouseEvent::move_abs(x, y)));
                }
            }
            ControlInput::MouseRelative { dx, dy } => {
                // Relative reports carry at most ±127 per axis.
                let (mut dx, mut dy) = (dx.clamp(-4096, 4096), dy.clamp(-4096, 4096));
                while dx != 0 || dy != 0 {
                    let (sx, sy) = (dx.clamp(-127, 127), dy.clamp(-127, 127));
                    out.push(HidAction::Mouse(MouseEvent::move_rel(sx, sy)));
                    dx -= sx;
                    dy -= sy;
                }
            }
            ControlInput::MouseScroll { dy, .. } => {
                // Reverse before accumulation so precise trackpad remainders keep their direction.
                let dy = dy.clamp(-12_000, 12_000);
                let dy = if self.reverse_scroll { -dy } else { dy };
                // One wheel notch is 120; precise trackpads send smaller deltas. Positive is up.
                self.scroll_remainder =
                    (self.scroll_remainder + dy.clamp(-12_000, 12_000)).clamp(-12_000, 12_000);
                let notches = self.scroll_remainder / 120;
                self.scroll_remainder %= 120;
                for _ in 0..notches.unsigned_abs().min(100) {
                    out.push(HidAction::Mouse(MouseEvent {
                        event_type: MouseEventType::Scroll,
                        x: 0,
                        y: 0,
                        button: None,
                        scroll: if notches > 0 { 1 } else { -1 },
                    }));
                }
            }
        }
        out
    }

    /// Key-up / button-up events for everything this session still holds.
    pub fn release_all(&mut self) -> Vec<HidAction> {
        let mut out = Vec::new();
        for usage in std::mem::take(&mut self.keys) {
            if let Some(key) = CanonicalKey::from_hid_usage(usage) {
                out.push(HidAction::Keyboard(KeyboardEvent::key_up(
                    key,
                    modifiers_from_bits(self.modifiers),
                )));
            }
        }
        for bit in 0..8u8 {
            if self.modifiers & (1 << bit) != 0 {
                if let Some(key) = CanonicalKey::from_hid_usage(0xe0 + bit) {
                    self.modifiers &= !(1 << bit);
                    out.push(HidAction::Keyboard(KeyboardEvent::key_up(
                        key,
                        modifiers_from_bits(self.modifiers),
                    )));
                }
            }
        }
        for bit in std::mem::take(&mut self.buttons) {
            let button = [MouseButton::Left, MouseButton::Right, MouseButton::Middle]
                .into_iter()
                .find(|b| b.to_hid_bit() == bit);
            if let Some(button) = button {
                out.push(HidAction::Mouse(MouseEvent {
                    event_type: MouseEventType::Up,
                    x: 0,
                    y: 0,
                    button: Some(button),
                    scroll: 0,
                }));
            }
        }
        self.scroll_remainder = 0;
        out
    }

    /// Press then release a shortcut (modifiers first), for SimpleAction menu commands.
    pub fn shortcut(&mut self, keys: &[CanonicalKey]) -> Vec<HidAction> {
        let mut out: Vec<HidAction> = keys
            .iter()
            .map(|k| HidAction::Keyboard(self.key(*k, true)))
            .collect();
        out.extend(
            keys.iter()
                .rev()
                .map(|k| HidAction::Keyboard(self.key(*k, false))),
        );
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keyboard(actions: &[HidAction]) -> Vec<(KeyEventType, CanonicalKey, u8)> {
        actions
            .iter()
            .filter_map(|a| match a {
                HidAction::Keyboard(k) => Some((k.event_type, k.key, k.modifiers.to_hid_byte())),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn decodes_official_input_json_and_ignores_heartbeat() {
        assert_eq!(
            decode(br#"{"action":"mouse_move_absolute","abs_x":0.5,"abs_y":1}"#),
            Some(ControlInput::MouseAbsolute { x: 0.5, y: 1.0 })
        );
        assert_eq!(
            decode(br#"{"action":"kbd_release","key":65}"#),
            Some(ControlInput::Keyboard {
                key: 65,
                pressed: false
            })
        );
        assert_eq!(
            decode(br#"{"action":"mouse_move_relative","delta_x":-4,"delta_y":5}"#),
            Some(ControlInput::MouseRelative { dx: -4, dy: 5 })
        );
        assert_eq!(decode(br#"{"heartbeat":"kbd_heartbeat"}"#), None);
        assert_eq!(decode(br#"{"action":"kbd_press","key":2147483648}"#), None);
        assert_eq!(decode(br#"{"action":"unverified"}"#), None);
        assert_eq!(decode(&[0xff, 0x08]), None);
    }

    #[test]
    fn maps_windows_vk_to_physical_keys() {
        assert_eq!(windows_vk_to_key(0x41), Some(CanonicalKey::KeyA));
        assert_eq!(windows_vk_to_key(0x5a), Some(CanonicalKey::KeyZ));
        assert_eq!(windows_vk_to_key(0x30), Some(CanonicalKey::Digit0));
        assert_eq!(windows_vk_to_key(0x39), Some(CanonicalKey::Digit9));
        assert_eq!(windows_vk_to_key(0x87), Some(CanonicalKey::F24));
        assert_eq!(windows_vk_to_key(0x69), Some(CanonicalKey::Numpad9));
        assert_eq!(windows_vk_to_key(160), Some(CanonicalKey::ShiftLeft));
        assert_eq!(windows_vk_to_key(0xa5), Some(CanonicalKey::AltRight));
        assert_eq!(windows_vk_to_key(0xde), Some(CanonicalKey::Quote));
        for vk in [-1, 0, 7, 21, 136, 255, i32::MAX] {
            assert_eq!(windows_vk_to_key(vk), None, "{vk}");
        }
    }

    #[test]
    fn backspace_wire_events_follow_the_advertised_system() {
        let press = decode(br#"{"action":"kbd_press","key":51}"#).unwrap();
        let release = decode(br#"{"action":"kbd_release","key":51}"#).unwrap();
        let click = decode(br#"{"action":"kbd_click","key":51}"#).unwrap();
        for (system, key, usage) in [
            (UuRemoteSystem::Macos, CanonicalKey::Backspace, 0x2a),
            (UuRemoteSystem::Windows, CanonicalKey::Digit3, 0x20),
        ] {
            let mut state = InputState::new(system, false);
            assert_eq!(
                keyboard(&state.translate(&press)),
                vec![(KeyEventType::Down, key, 0)]
            );
            assert_eq!(key.to_hid_usage(), usage);
            assert_eq!(
                keyboard(&state.translate(&release)),
                vec![(KeyEventType::Up, key, 0)]
            );
            assert!(state.release_all().is_empty());
            assert_eq!(
                keyboard(&state.translate(&click)),
                vec![(KeyEventType::Down, key, 0), (KeyEventType::Up, key, 0)]
            );
            assert!(state.release_all().is_empty());
        }
    }

    #[test]
    fn macos_copy_and_disconnect_preserve_then_release_command() {
        let mut state = InputState::new(UuRemoteSystem::Macos, true);
        let command = decode(br#"{"action":"kbd_press","key":55}"#).unwrap();
        let copy = decode(br#"{"action":"kbd_press","key":8}"#).unwrap();
        assert_eq!(
            keyboard(&state.translate(&command)),
            vec![(KeyEventType::Down, CanonicalKey::MetaLeft, 0x08)]
        );
        assert_eq!(
            keyboard(&state.translate(&copy)),
            vec![(KeyEventType::Down, CanonicalKey::KeyC, 0x08)]
        );
        // A Windows-only keycode must not inject a modifier into a Mac session.
        assert!(state
            .translate(&ControlInput::Keyboard {
                key: 0xa2,
                pressed: true
            })
            .is_empty());
        assert_eq!(
            keyboard(&state.release_all()),
            vec![
                (KeyEventType::Up, CanonicalKey::KeyC, 0x08),
                (KeyEventType::Up, CanonicalKey::MetaLeft, 0),
            ]
        );
        assert!(state.release_all().is_empty());
        let a = decode(br#"{"action":"kbd_click","key":0}"#).unwrap();
        assert_eq!(
            keyboard(&state.translate(&a)),
            vec![
                (KeyEventType::Down, CanonicalKey::KeyA, 0),
                (KeyEventType::Up, CanonicalKey::KeyA, 0),
            ]
        );
    }

    #[test]
    fn ordinary_keys_carry_held_modifiers() {
        let mut state = InputState::default();
        state.translate(&ControlInput::Keyboard {
            key: 0xa2,
            pressed: true,
        });
        let press = state.translate(&ControlInput::Keyboard {
            key: 0x43,
            pressed: true,
        });
        assert_eq!(
            keyboard(&press),
            vec![(KeyEventType::Down, CanonicalKey::KeyC, 0x01)]
        );
        state.translate(&ControlInput::Keyboard {
            key: 0xa2,
            pressed: false,
        });
        let release = state.translate(&ControlInput::Keyboard {
            key: 0x43,
            pressed: false,
        });
        assert_eq!(
            keyboard(&release),
            vec![(KeyEventType::Up, CanonicalKey::KeyC, 0)]
        );
    }

    #[test]
    fn release_all_frees_keys_modifiers_and_buttons() {
        let mut state = InputState::default();
        state.translate(&ControlInput::Keyboard {
            key: 0x10,
            pressed: true,
        });
        state.translate(&ControlInput::Keyboard {
            key: 0x41,
            pressed: true,
        });
        state.translate(&ControlInput::MouseButton {
            button: 2,
            pressed: true,
        });
        let released = state.release_all();
        assert_eq!(
            keyboard(&released),
            vec![
                (KeyEventType::Up, CanonicalKey::KeyA, 0x02),
                (KeyEventType::Up, CanonicalKey::ShiftLeft, 0)
            ]
        );
        assert!(released.iter().any(|a| matches!(a, HidAction::Mouse(m) if m.event_type == MouseEventType::Up && m.button == Some(MouseButton::Right))));
        assert!(state.release_all().is_empty());
    }

    #[test]
    fn pointer_mapping_scales_splits_and_accumulates() {
        let mut state = InputState::default();
        let abs = state.translate(&ControlInput::MouseAbsolute { x: 1.0, y: 0.5 });
        assert!(
            matches!(&abs[..], [HidAction::Mouse(m)] if m.event_type == MouseEventType::MoveAbs && m.x == 32767 && m.y == 16384)
        );
        assert!(state
            .translate(&ControlInput::MouseAbsolute { x: 1.1, y: 0.5 })
            .is_empty());
        let rel = state.translate(&ControlInput::MouseRelative { dx: 300, dy: -10 });
        let moves: Vec<(i32, i32)> = rel
            .iter()
            .filter_map(|a| match a {
                HidAction::Mouse(m) => Some((m.x, m.y)),
                _ => None,
            })
            .collect();
        assert_eq!(moves, vec![(127, -10), (127, 0), (46, 0)]);
        assert!(state
            .translate(&ControlInput::MouseScroll { dx: 0, dy: 60 })
            .is_empty());
        let scroll = state.translate(&ControlInput::MouseScroll { dx: 0, dy: 60 });
        assert!(matches!(&scroll[..], [HidAction::Mouse(m)] if m.scroll == 1));
        let down = state.translate(&ControlInput::MouseScroll { dx: 0, dy: -240 });
        assert_eq!(down.len(), 2);
        assert!(state
            .translate(&ControlInput::MouseButton {
                button: 3,
                pressed: true
            })
            .is_empty());
    }

    #[test]
    fn scroll_override_preserves_precise_deltas_and_disconnect_clears_remainder() {
        for reverse in [false, true] {
            let mut state = InputState::new(UuRemoteSystem::Macos, reverse);
            let scroll = |dy| ControlInput::MouseScroll { dx: 0, dy };
            assert!(state.translate(&scroll(60)).is_empty());
            let events = state.translate(&scroll(180));
            assert_eq!(events.len(), 2);
            assert!(events.iter().all(|event| matches!(event, HidAction::Mouse(m) if m.scroll == if reverse { -1 } else { 1 })));
            let events = state.translate(&scroll(-120));
            assert!(
                matches!(&events[..], [HidAction::Mouse(m)] if m.scroll == if reverse { 1 } else { -1 })
            );
            assert!(state.translate(&scroll(60)).is_empty());
            state.release_all();
            assert!(state.translate(&scroll(60)).is_empty());
            // Clamp before reversal, including the minimum signed input.
            assert!(state.translate(&scroll(i32::MIN)).len() <= 100);
        }
    }

    #[test]
    fn saved_scroll_override_survives_reload_but_system_switch_resets_default() {
        use crate::config::UuRemoteConfig;
        let mut config: UuRemoteConfig = serde_json::from_str(r#"{"system":"macos"}"#).unwrap();
        assert!(config.reverse_scroll_enabled());
        config.reverse_scroll = Some(false);
        config = serde_json::from_str(&serde_json::to_string(&config).unwrap()).unwrap();
        config.set_system(UuRemoteSystem::Macos);
        assert!(!config.reverse_scroll_enabled());
        config.set_system(UuRemoteSystem::Windows);
        assert!(!config.reverse_scroll_enabled());
        config.set_system(UuRemoteSystem::Macos);
        assert!(config.reverse_scroll_enabled());
    }

    #[test]
    fn shortcut_presses_in_order_and_releases_in_reverse() {
        let mut state = InputState::default();
        let keys = keyboard(&state.shortcut(&[CanonicalKey::MetaLeft, CanonicalKey::KeyD]));
        assert_eq!(
            keys,
            vec![
                (KeyEventType::Down, CanonicalKey::MetaLeft, 0x08),
                (KeyEventType::Down, CanonicalKey::KeyD, 0x08),
                (KeyEventType::Up, CanonicalKey::KeyD, 0x08),
                (KeyEventType::Up, CanonicalKey::MetaLeft, 0),
            ]
        );
        assert!(state.release_all().is_empty());
    }
}
