//! App shortcuts (`src/renderer/app/shortcuts.ts`): which chords are the app's and which
//! belong to the terminal. On macOS app shortcuts use ⌘ where Linux and Windows use Ctrl;
//! Ctrl then belongs to the terminal (Ctrl+C, Ctrl+R, Ctrl+_ in claude). Keys arrive as
//! GPUI names ("n", "/", "up"), bindings are saved as DOM key codes (`mod+shift+KeyN`).

use crate::types::ShortcutId;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Left,
    Right,
    Up,
    Down,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Shortcut(ShortcutId),
    Focus(Direction),
    /// Switch to the nth project (0-based).
    Workspace(usize),
    /// +1 / -1 zooms the whole app a step; 0 resets it to 100%.
    UiZoom(i8),
}

/// A rebindable chord: always the app modifier, plus Shift and/or Alt, on a key code.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Binding {
    pub shift: bool,
    pub alt: bool,
    pub code: String,
}

pub fn default_binding(id: ShortcutId) -> Binding {
    let code = match id {
        ShortcutId::NewPane => "KeyN",
        ShortcutId::ClosePane => "KeyW",
        ShortcutId::ToggleZoom => "KeyZ",
        ShortcutId::Palette => "KeyP",
        ShortcutId::Diff => "KeyD",
        ShortcutId::Shortcuts => "Slash",
    };
    Binding {
        shift: true,
        alt: false,
        code: code.into(),
    }
}

pub fn label(id: ShortcutId) -> &'static str {
    match id {
        ShortcutId::NewPane => "New pane",
        ShortcutId::ClosePane => "Close pane",
        ShortcutId::ToggleZoom => "Zoom pane / back to grid",
        ShortcutId::Palette => "Search panes and commands",
        ShortcutId::Diff => "Show / hide the git diff",
        ShortcutId::Shortcuts => "Show the shortcuts sheet",
    }
}

/// `mod+shift+KeyN`, as saved in settings.
pub fn format_binding(b: &Binding) -> String {
    let mut parts = vec!["mod"];
    if b.shift {
        parts.push("shift");
    }
    if b.alt {
        parts.push("alt");
    }
    parts.push(&b.code);
    parts.join("+")
}

pub fn parse_binding(raw: &str) -> Option<Binding> {
    let rest = raw.strip_prefix("mod+")?;
    let (shift, rest) = rest
        .strip_prefix("shift+")
        .map_or((false, rest), |r| (true, r));
    let (alt, code) = rest
        .strip_prefix("alt+")
        .map_or((false, rest), |r| (true, r));
    ((1..=24).contains(&code.len()) && code.chars().all(|c| c.is_ascii_alphanumeric())).then(|| {
        Binding {
            shift,
            alt,
            code: code.into(),
        }
    })
}

/// Why a binding can't be used, or None when it can. Without Shift or Alt it would take a
/// key the terminal needs; fixed chords and other bindings can't be shared.
pub fn binding_problem(
    b: &Binding,
    id: ShortcutId,
    all: &[(ShortcutId, Binding)],
) -> Option<String> {
    if !b.shift && !b.alt {
        return Some("Add Shift or Alt: the plain chord belongs to the terminal.".into());
    }
    if b.alt && !b.shift && b.code.starts_with("Arrow") {
        return Some("Reserved for moving focus.".into());
    }
    let digit = b
        .code
        .strip_prefix("Digit")
        .is_some_and(|d| d.len() == 1 && ('1'..='9').contains(&d.chars().next().unwrap()));
    if b.shift && !b.alt && digit {
        return Some("Reserved for switching projects.".into());
    }
    if b.shift && b.code == "Minus" {
        return Some("Reserved for claude's undo.".into());
    }
    if b.shift && b.code == "KeyC" {
        return Some("Reserved for copy.".into());
    }
    if b.shift && b.code == "KeyV" {
        return Some("Reserved for paste.".into());
    }
    if b.shift && !b.alt && b.code == "KeyR" {
        return Some("Reserved for replying to chat text.".into());
    }
    all.iter()
        .find(|(other, ob)| *other != id && ob == b)
        .map(|(other, _)| format!("Already used by {}.", label(*other)))
}

/// The bindings in effect: defaults, with the user's valid ones on top.
pub fn bindings(saved: &[(ShortcutId, String)]) -> Vec<(ShortcutId, Binding)> {
    let mut next: Vec<(ShortcutId, Binding)> = ShortcutId::ALL
        .iter()
        .map(|id| (*id, default_binding(*id)))
        .collect();
    for id in ShortcutId::ALL {
        let Some(b) = saved
            .iter()
            .find(|(s, _)| s == id)
            .and_then(|(_, raw)| parse_binding(raw))
        else {
            continue;
        };
        if binding_problem(&b, *id, &next).is_none() {
            if let Some(slot) = next.iter_mut().find(|(s, _)| s == id) {
                slot.1 = b;
            }
        }
    }
    next
}

/// The DOM key code a GPUI key name stands for (letters, digits, punctuation, arrows).
pub fn code_for_key(key: &str) -> Option<String> {
    let mut chars = key.chars();
    if let (Some(c), None) = (chars.next(), chars.next()) {
        if c.is_ascii_alphabetic() {
            return Some(format!("Key{}", c.to_ascii_uppercase()));
        }
        if c.is_ascii_digit() {
            return Some(format!("Digit{c}"));
        }
        if let Some(i) = SHIFTED_DIGITS.find(c) {
            return Some(format!("Digit{}", (i + 1) % 10));
        }
        let named = match c {
            '/' | '?' => "Slash",
            '\\' | '|' => "Backslash",
            '.' | '>' => "Period",
            ',' | '<' => "Comma",
            ';' | ':' => "Semicolon",
            '\'' | '"' => "Quote",
            '[' | '{' => "BracketLeft",
            ']' | '}' => "BracketRight",
            '`' | '~' => "Backquote",
            '=' | '+' => "Equal",
            '-' | '_' => "Minus",
            _ => return None,
        };
        return Some(named.into());
    }
    let named = match key {
        "up" => "ArrowUp",
        "down" => "ArrowDown",
        "left" => "ArrowLeft",
        "right" => "ArrowRight",
        f if f.starts_with('f') && f[1..].parse::<u8>().is_ok() => return Some(f.to_uppercase()),
        _ => return None,
    };
    Some(named.into())
}

/// What Shift turns 1…9, 0 into on a US layout.
const SHIFTED_DIGITS: &str = "!@#$%^&*()";

/// GPUI on Linux reports Ctrl+Shift+/ as `?` without Shift (it keeps Shift only for
/// letters), so a shifted symbol means Shift was held.
fn shift_held(p: &Press) -> bool {
    let mut chars = p.key.chars();
    p.shift
        || matches!((chars.next(), chars.next()),
            (Some(c), None) if SHIFTED_DIGITS.contains(c) || "~_+{}|:\"<>?".contains(c))
}

/// A key press with its modifiers, as the UI reports it.
#[derive(Clone, Copy, Debug, Default)]
pub struct Press<'a> {
    pub key: &'a str,
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
    /// ⌘ on macOS, Super elsewhere.
    pub meta: bool,
}

/// Maps a key press to an app action, or None when the key belongs to the terminal. `mac`
/// swaps Ctrl for ⌘; the other of the two must not be held.
pub fn match_shortcut(p: Press, mac: bool, bindings: &[(ShortcutId, Binding)]) -> Option<Action> {
    let m = if mac { p.meta } else { p.ctrl };
    if !m || (if mac { p.ctrl } else { p.meta }) {
        return None;
    }
    let code = code_for_key(p.key)?;
    let p = Press {
        shift: shift_held(&p),
        ..p
    };
    if let Some((id, _)) = bindings
        .iter()
        .find(|(_, b)| b.code == code && b.shift == p.shift && b.alt == p.alt)
    {
        return Some(Action::Shortcut(*id));
    }
    if p.alt && !p.shift {
        return match code.as_str() {
            "ArrowLeft" => Some(Action::Focus(Direction::Left)),
            "ArrowRight" => Some(Action::Focus(Direction::Right)),
            "ArrowUp" => Some(Action::Focus(Direction::Up)),
            "ArrowDown" => Some(Action::Focus(Direction::Down)),
            _ => None,
        };
    }
    // Ctrl+= / Ctrl++ / Ctrl+- / Ctrl+0 zoom the app, like a browser. Ctrl+Shift+- stays with
    // the terminal: it is claude's undo (Ctrl+_).
    if !p.alt {
        if code == "Equal" {
            return Some(Action::UiZoom(1));
        }
        if !p.shift && code == "Minus" {
            return Some(Action::UiZoom(-1));
        }
        if !p.shift && code == "Digit0" {
            return Some(Action::UiZoom(0));
        }
    }
    if p.shift && !p.alt {
        if let Some(d) = code
            .strip_prefix("Digit")
            .and_then(|d| d.parse::<usize>().ok())
            .filter(|d| (1..=9).contains(d))
        {
            return Some(Action::Workspace(d - 1));
        }
    }
    None
}

/// The binding a key press asks for while Settings records one (`bindingFromEvent`), or
/// None for a lone modifier or a key that can't be bound.
pub fn binding_from_press(p: Press, mac: bool) -> Option<Binding> {
    let m = if mac { p.meta } else { p.ctrl };
    if !m || (if mac { p.ctrl } else { p.meta }) {
        return None;
    }
    let code = code_for_key(p.key)?;
    let bindable = code.strip_prefix("Key").is_some_and(|k| k.len() == 1)
        || code.starts_with("Digit")
        || (code.starts_with('F') && code[1..].parse::<u8>().is_ok())
        || [
            "Slash",
            "Backslash",
            "Period",
            "Comma",
            "Semicolon",
            "Quote",
            "BracketLeft",
            "BracketRight",
            "Backquote",
        ]
        .contains(&code.as_str());
    bindable.then(|| Binding {
        shift: shift_held(&p),
        alt: p.alt,
        code,
    })
}

/// What a key code shows as: KeyN → N, Digit3 → 3, Slash → /.
fn key_name(code: &str) -> String {
    let named = match code {
        "Slash" => "/",
        "Backslash" => "\\",
        "Period" => ".",
        "Comma" => ",",
        "Semicolon" => ";",
        "Quote" => "'",
        "BracketLeft" => "[",
        "BracketRight" => "]",
        "Backquote" => "`",
        _ => {
            return code
                .strip_prefix("Key")
                .or_else(|| code.strip_prefix("Digit"))
                .unwrap_or(code)
                .to_string()
        }
    };
    named.into()
}

/// The keys of a chord, one per key cap: `mod`, `shift`, `alt` become Ctrl/Shift/Alt, or
/// ⌘/⇧/⌥ on macOS; anything else stays as it is.
pub fn chord_keys(keys: &[&str], mac: bool) -> Vec<String> {
    keys.iter()
        .map(|k| {
            match (*k, mac) {
                ("mod", false) => "Ctrl",
                ("shift", false) => "Shift",
                ("alt", false) => "Alt",
                ("mod", true) => "⌘",
                ("shift", true) => "⇧",
                ("alt", true) => "⌥",
                (k, _) => k,
            }
            .to_string()
        })
        .collect()
}

/// The keys of a binding, one per key cap: ["Ctrl", "Shift", "N"] or ["⌘", "⇧", "N"].
pub fn binding_keys(b: &Binding, mac: bool) -> Vec<String> {
    let name = key_name(&b.code);
    let mut keys = vec!["mod"];
    if b.shift {
        keys.push("shift");
    }
    if b.alt {
        keys.push("alt");
    }
    keys.push(&name);
    chord_keys(&keys, mac)
}

/// How a binding is written: "Ctrl+Shift+N", or "⌘⇧N" on macOS.
pub fn binding_hint(b: &Binding, mac: bool) -> String {
    binding_keys(b, mac).join(if mac { "" } else { "+" })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shifted_symbols_mean_shift() {
        let b = bindings(&[]);
        // GPUI on Linux: Ctrl+Shift+/ arrives as `?`, Ctrl+Shift+3 as `#`, both without Shift.
        assert_eq!(
            match_shortcut(press("?", false, false), false, &b),
            Some(Action::Shortcut(ShortcutId::Shortcuts))
        );
        assert_eq!(
            match_shortcut(press("#", false, false), false, &b),
            Some(Action::Workspace(2))
        );
        // Ctrl+Shift+- (`_`) is claude's undo, not zoom out.
        assert_eq!(match_shortcut(press("_", false, false), false, &b), None);
    }

    #[test]
    fn recording_and_keys() {
        let rec = |key, shift, alt| binding_from_press(press(key, shift, alt), false);
        assert_eq!(
            rec("t", false, true),
            Some(Binding {
                shift: false,
                alt: true,
                code: "KeyT".into()
            })
        );
        let slash = rec("?", false, false).unwrap();
        assert!(slash.shift && slash.code == "Slash");
        // Arrows, Minus and lone modifiers can't be bound; neither can a chord without Ctrl.
        assert_eq!(rec("left", true, false), None);
        assert_eq!(rec("-", true, false), None);
        assert_eq!(rec("shift", true, false), None);
        let plain = Press {
            key: "n",
            shift: true,
            ..Press::default()
        };
        assert_eq!(binding_from_press(plain, false), None);
        let b = default_binding(ShortcutId::Shortcuts);
        assert_eq!(binding_keys(&b, false), ["Ctrl", "Shift", "/"]);
        assert_eq!(binding_hint(&b, true), "⌘⇧/");
        assert_eq!(
            binding_hint(&default_binding(ShortcutId::NewPane), false),
            "Ctrl+Shift+N"
        );
    }

    fn press(key: &str, shift: bool, alt: bool) -> Press<'_> {
        Press {
            key,
            ctrl: true,
            shift,
            alt,
            meta: false,
        }
    }

    #[test]
    fn defaults_and_terminal_keys() {
        let b = bindings(&[]);
        assert_eq!(
            match_shortcut(press("n", true, false), false, &b),
            Some(Action::Shortcut(ShortcutId::NewPane))
        );
        assert_eq!(
            match_shortcut(press("/", true, false), false, &b),
            Some(Action::Shortcut(ShortcutId::Shortcuts))
        );
        // Plain Ctrl+C / Ctrl+R are the terminal's.
        assert_eq!(match_shortcut(press("c", false, false), false, &b), None);
        assert_eq!(match_shortcut(press("r", false, false), false, &b), None);
        // Ctrl+Shift+- is claude's undo.
        assert_eq!(match_shortcut(press("-", true, false), false, &b), None);
        assert_eq!(
            match_shortcut(press("-", false, false), false, &b),
            Some(Action::UiZoom(-1))
        );
        assert_eq!(
            match_shortcut(press("=", true, false), false, &b),
            Some(Action::UiZoom(1))
        );
        assert_eq!(
            match_shortcut(press("3", true, false), false, &b),
            Some(Action::Workspace(2))
        );
        assert_eq!(
            match_shortcut(press("left", false, true), false, &b),
            Some(Action::Focus(Direction::Left))
        );
    }

    #[test]
    fn mac_uses_command() {
        let b = bindings(&[]);
        let cmd = Press {
            key: "n",
            ctrl: false,
            shift: true,
            alt: false,
            meta: true,
        };
        assert_eq!(
            match_shortcut(cmd, true, &b),
            Some(Action::Shortcut(ShortcutId::NewPane))
        );
        assert_eq!(match_shortcut(press("n", true, false), true, &b), None);
    }

    #[test]
    fn rebinding_rules() {
        let b = bindings(&[
            (ShortcutId::NewPane, "mod+alt+KeyT".into()),
            (ShortcutId::Diff, "mod+KeyD".into()),
        ]);
        assert_eq!(
            b.iter().find(|(i, _)| *i == ShortcutId::NewPane).unwrap().1,
            Binding {
                shift: false,
                alt: true,
                code: "KeyT".into()
            }
        );
        // A plain chord is refused: Diff keeps its default.
        assert_eq!(
            b.iter().find(|(i, _)| *i == ShortcutId::Diff).unwrap().1,
            default_binding(ShortcutId::Diff)
        );
        let all = bindings(&[]);
        let taken = Binding {
            shift: true,
            alt: false,
            code: "KeyW".into(),
        };
        assert_eq!(
            binding_problem(&taken, ShortcutId::Palette, &all).unwrap(),
            "Already used by Close pane."
        );
        assert_eq!(
            binding_problem(
                &Binding {
                    shift: true,
                    alt: false,
                    code: "Digit4".into()
                },
                ShortcutId::Palette,
                &all
            )
            .unwrap(),
            "Reserved for switching projects."
        );
        assert_eq!(format_binding(&taken), "mod+shift+KeyW");
        assert_eq!(
            parse_binding("mod+shift+alt+F5"),
            Some(Binding {
                shift: true,
                alt: true,
                code: "F5".into()
            })
        );
        assert_eq!(parse_binding("ctrl+KeyN"), None);
    }
}
