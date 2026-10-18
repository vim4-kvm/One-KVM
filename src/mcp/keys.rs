//! Pure US-layout conversion. No text or key names are logged.
use crate::hid::{types::KeyboardReport, CanonicalKey};

pub fn character(c: char) -> Option<(u8, u8)> {
    if c == '\r' {
        return Some((40, 0));
    }
    if c.is_ascii_alphabetic() {
        return Some((
            4 + c.to_ascii_lowercase() as u8 - b'a',
            if c.is_ascii_uppercase() { 2 } else { 0 },
        ));
    }
    let plain = "1234567890\n\u{1b}\u{8}\t -=[]\\#;'`,./";
    let shifted = "!@#$%^&*()\n\u{1b}\u{8}\t _+{}|~:\"~<>?";
    // Usage 0x32 is the non-US key: it must never substitute for US '#'.
    for (index, value) in plain.chars().enumerate() {
        if index != 20 && c == value {
            return Some((30 + index as u8, 0));
        }
    }
    for (index, value) in shifted.chars().enumerate() {
        if index != 20 && c == value {
            return Some((30 + index as u8, 2));
        }
    }
    None
}

fn named(name: &str) -> Option<(u8, u8)> {
    if name.chars().count() == 1 {
        return character(name.chars().next()?);
    }
    let lower = name.to_ascii_lowercase();
    let alias = match lower.as_str() {
        "ctrl" | "control" | "ctrl_l" | "control_l" => "controlleft",
        "shift" | "shift_l" => "shiftleft",
        "alt" | "option" | "alt_l" => "altleft",
        "super" | "cmd" | "command" | "meta" | "win" | "super_l" => "metaleft",
        "ctrl_r" | "control_r" => "controlright",
        "shift_r" => "shiftright",
        "alt_r" => "altright",
        "super_r" | "meta_r" => "metaright",
        "return" => "enter",
        "esc" => "escape",
        "space" => "space",
        "page_up" | "prior" => "pageup",
        "page_down" | "next" => "pagedown",
        "up" => "arrowup",
        "down" => "arrowdown",
        "left" => "arrowleft",
        "right" => "arrowright",
        "print" => "printscreen",
        "scroll_lock" => "scrolllock",
        "caps_lock" => "capslock",
        "num_lock" => "numlock",
        "menu" => "contextmenu",
        "kp_enter" => "numpadenter",
        "kp_add" => "numpadadd",
        "kp_subtract" => "numpadsubtract",
        "kp_multiply" => "numpadmultiply",
        "kp_divide" => "numpaddivide",
        "kp_decimal" | "kp_delete" => "numpaddecimal",
        _ => &lower,
    };
    let alias = if let Some(digit) = alias
        .strip_prefix("kp_")
        .filter(|s| s.len() == 1 && s.as_bytes()[0].is_ascii_digit())
    {
        format!("numpad{digit}")
    } else {
        alias.to_owned()
    };
    for usage in 4..=231 {
        if let Some(key) = CanonicalKey::from_hid_usage(usage) {
            if format!("{key:?}").eq_ignore_ascii_case(&alias) {
                return Some(if let Some(bit) = key.modifier_bit() {
                    (0, bit)
                } else {
                    (usage, 0)
                });
            }
        }
    }
    None
}

pub fn chord(text: &str) -> Result<KeyboardReport, String> {
    let mut report = KeyboardReport::default();
    let parts = if text == "+" {
        vec![text]
    } else {
        text.split('+').collect()
    };
    for part in parts {
        let (key, modifiers) = named(part).ok_or_else(|| {
            format!("unknown key {part:?}. Use xdotool names like Return, ctrl+s, alt+Tab.")
        })?;
        report.modifiers |= modifiers;
        if key != 0 && !report.keys.contains(&key) && !report.add_key(key) {
            return Err("A key combination can contain at most 6 ordinary keys.".into());
        }
    }
    Ok(report)
}

pub fn modifiers(text: &str) -> Result<KeyboardReport, String> {
    let report = chord(text)?;
    if report.keys != [0; 6] {
        return Err("Use only shift, ctrl, alt or super modifiers for this action.".into());
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn printable_us_layout_and_controls() {
        for c in ' '..='~' {
            assert!(character(c).is_some(), "{c:?}");
        }
        assert_eq!(character('#'), Some((32, 2)));
        assert_eq!(character('A'), Some((4, 2)));
        assert_eq!(character('?'), Some((56, 2)));
        assert_eq!(character('中'), None);
        assert_eq!(character('\n'), Some((40, 0)));
    }
    #[test]
    fn aliases_chords_and_limits() {
        assert_eq!(
            chord("ctrl+shift+KeyS").unwrap().to_bytes(),
            [3, 0, 22, 0, 0, 0, 0, 0]
        );
        for name in ["Return", "ENTER", "enter"] {
            assert_eq!(chord(name).unwrap().keys[0], 40);
        }
        assert_eq!(chord("ctrl_r+KP_1").unwrap().modifiers, 16);
        assert_eq!(chord("F24").unwrap().keys[0], 115);
        assert!(chord("a+b+c+d+e+f+g").is_err());
        assert!(chord("Foo").unwrap_err().starts_with("unknown key \"Foo\""));
        assert!(modifiers("ctrl+s").is_err());
    }
}
