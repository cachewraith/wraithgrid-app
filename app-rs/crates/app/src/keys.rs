//! Keystroke → bytes for a terminal pane. Only keys that need an escape sequence or a
//! control byte are mapped here; plain text goes through the IME input handler instead,
//! so composed input (dead keys, CJK) reaches the pty as the platform committed it.
//! Kept free of GPUI types so it can be unit-tested.

#[derive(Clone, Copy, Default, Debug)]
pub struct Mods {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    /// ⌘ / Super: always an app shortcut, never sent to the pty.
    pub platform: bool,
}

impl Mods {
    /// xterm's modifier parameter: 1 + shift + 2·alt + 4·ctrl.
    fn param(self) -> u8 {
        1 + self.shift as u8 + 2 * self.alt as u8 + 4 * self.ctrl as u8
    }

    fn any(self) -> bool {
        self.ctrl || self.alt || self.shift
    }
}

/// Bytes for `key` (GPUI's key name), or `None` when the key is text for the input
/// handler or an app shortcut. `app_cursor` is DECCKM (cursor keys in application mode).
pub fn to_bytes(
    key: &str,
    key_char: Option<&str>,
    mods: Mods,
    app_cursor: bool,
) -> Option<Vec<u8>> {
    if mods.platform {
        return None;
    }
    let csi = |final_byte: char| -> Vec<u8> {
        if mods.any() {
            format!("\x1b[1;{}{final_byte}", mods.param()).into_bytes()
        } else if app_cursor {
            format!("\x1bO{final_byte}").into_bytes()
        } else {
            format!("\x1b[{final_byte}").into_bytes()
        }
    };
    let tilde = |code: u8| -> Vec<u8> {
        if mods.any() {
            format!("\x1b[{code};{}~", mods.param()).into_bytes()
        } else {
            format!("\x1b[{code}~").into_bytes()
        }
    };
    let alt_prefixed = |bytes: &[u8]| -> Vec<u8> {
        let mut out = Vec::with_capacity(bytes.len() + 1);
        if mods.alt {
            out.push(0x1b);
        }
        out.extend_from_slice(bytes);
        out
    };

    let bytes = match key {
        "enter" => alt_prefixed(b"\r"),
        "backspace" if mods.ctrl => alt_prefixed(b"\x08"),
        "backspace" => alt_prefixed(b"\x7f"),
        "tab" if mods.shift => b"\x1b[Z".to_vec(),
        "tab" => alt_prefixed(b"\t"),
        "escape" => alt_prefixed(b"\x1b"),
        "space" if mods.ctrl => alt_prefixed(b"\0"),
        "up" => csi('A'),
        "down" => csi('B'),
        "right" => csi('C'),
        "left" => csi('D'),
        "home" => csi('H'),
        "end" => csi('F'),
        "insert" => tilde(2),
        "delete" => tilde(3),
        "pageup" => tilde(5),
        "pagedown" => tilde(6),
        "f1" | "f2" | "f3" | "f4" => {
            let final_byte = (b'P' + key.as_bytes()[1] - b'1') as char;
            if mods.any() {
                format!("\x1b[1;{}{final_byte}", mods.param()).into_bytes()
            } else {
                format!("\x1bO{final_byte}").into_bytes()
            }
        }
        "f5" => tilde(15),
        "f6" => tilde(17),
        "f7" => tilde(18),
        "f8" => tilde(19),
        "f9" => tilde(20),
        "f10" => tilde(21),
        "f11" => tilde(23),
        "f12" => tilde(24),
        _ => {
            let mut chars = key.chars();
            let (Some(c), None) = (chars.next(), chars.next()) else {
                return None;
            };
            if mods.ctrl {
                // Ctrl+Shift+letter is left for app shortcuts (copy, paste).
                if mods.shift && c.is_ascii_alphabetic() {
                    return None;
                }
                let byte = match c.to_ascii_lowercase() {
                    l @ 'a'..='z' => l as u8 & 0x1f,
                    '@' | '2' => 0,
                    '[' | '3' => 0x1b,
                    '\\' | '4' => 0x1c,
                    ']' | '5' => 0x1d,
                    '^' | '6' => 0x1e,
                    '_' | '-' | '/' | '7' => 0x1f,
                    '8' => 0x7f,
                    _ => return None,
                };
                alt_prefixed(&[byte])
            } else if mods.alt {
                let text = key_char.unwrap_or(key);
                alt_prefixed(text.as_bytes())
            } else {
                return None;
            }
        }
    };
    Some(bytes)
}

/// Text for a paste: bracketed when the program asked for it (ESC stripped so a paste
/// can't end the bracket early), otherwise with newlines as the Enter key sends them.
pub fn paste_bytes(text: &str, bracketed: bool) -> Vec<u8> {
    if bracketed {
        let mut out = b"\x1b[200~".to_vec();
        out.extend(text.replace('\x1b', "").into_bytes());
        out.extend_from_slice(b"\x1b[201~");
        out
    } else {
        text.replace("\r\n", "\r").replace('\n', "\r").into_bytes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn none() -> Mods {
        Mods::default()
    }

    fn ctrl() -> Mods {
        Mods {
            ctrl: true,
            ..Mods::default()
        }
    }

    #[test]
    fn plain_text_goes_to_the_input_handler() {
        assert_eq!(to_bytes("a", Some("a"), none(), false), None);
        assert_eq!(to_bytes("space", Some(" "), none(), false), None);
        assert_eq!(
            to_bytes(
                "a",
                Some("A"),
                Mods {
                    shift: true,
                    ..none()
                },
                false
            ),
            None
        );
    }

    #[test]
    fn control_bytes() {
        assert_eq!(to_bytes("c", None, ctrl(), false), Some(vec![0x03]));
        assert_eq!(to_bytes("[", None, ctrl(), false), Some(vec![0x1b]));
        assert_eq!(to_bytes("space", None, ctrl(), false), Some(vec![0]));
        assert_eq!(to_bytes("enter", None, none(), false), Some(b"\r".to_vec()));
        assert_eq!(to_bytes("backspace", None, none(), false), Some(vec![0x7f]));
    }

    #[test]
    fn ctrl_shift_letters_are_app_shortcuts() {
        let m = Mods {
            ctrl: true,
            shift: true,
            ..none()
        };
        assert_eq!(to_bytes("c", None, m, false), None);
        assert_eq!(to_bytes("v", None, m, false), None);
    }

    #[test]
    fn platform_key_is_never_sent() {
        let m = Mods {
            platform: true,
            ..none()
        };
        assert_eq!(to_bytes("c", None, m, false), None);
        assert_eq!(to_bytes("enter", None, m, false), None);
    }

    #[test]
    fn cursor_keys_follow_decckm_and_modifiers() {
        assert_eq!(
            to_bytes("up", None, none(), false),
            Some(b"\x1b[A".to_vec())
        );
        assert_eq!(to_bytes("up", None, none(), true), Some(b"\x1bOA".to_vec()));
        assert_eq!(
            to_bytes("left", None, ctrl(), true),
            Some(b"\x1b[1;5D".to_vec())
        );
        assert_eq!(
            to_bytes("delete", None, none(), false),
            Some(b"\x1b[3~".to_vec())
        );
        assert_eq!(
            to_bytes("f1", None, none(), false),
            Some(b"\x1bOP".to_vec())
        );
        assert_eq!(
            to_bytes(
                "f5",
                None,
                Mods {
                    shift: true,
                    ..none()
                },
                false
            ),
            Some(b"\x1b[15;2~".to_vec())
        );
        assert_eq!(
            to_bytes(
                "tab",
                None,
                Mods {
                    shift: true,
                    ..none()
                },
                false
            ),
            Some(b"\x1b[Z".to_vec())
        );
    }

    #[test]
    fn alt_prefixes_escape() {
        let m = Mods {
            alt: true,
            ..none()
        };
        assert_eq!(to_bytes("b", Some("b"), m, false), Some(b"\x1bb".to_vec()));
        assert_eq!(to_bytes("enter", None, m, false), Some(b"\x1b\r".to_vec()));
    }

    #[test]
    fn paste_is_bracketed_on_request() {
        assert_eq!(paste_bytes("a\nb", false), b"a\rb".to_vec());
        assert_eq!(
            paste_bytes("x\x1b[201~y", true),
            b"\x1b[200~x[201~y\x1b[201~".to_vec()
        );
    }
}
