//! Theme file validation: the TS `themeFileSchema` (zod 4) as code. Untrusted JSON goes in;
//! a normalized `ThemeFile` comes out (hex lowercased and expanded, names trimmed, unknown
//! top-level keys dropped), or the first problem in the same words zod uses, since the UI
//! shows them (`colors.bg: must be a hex color like #1f2937`).

use std::fmt;

use serde_json::{Map, Value};

use crate::prefs::ThemeName;
use crate::theme::{
    MonoFont, TerminalColor, ThemeBorder, ThemeColor, ThemeColors, ThemeDensity, ThemeDepth,
    ThemeFile, ThemeMotion, ThemeRadius, ThemeStyle, ThemeTerminalColors, UiFont, CHAT_TEXT_MAX,
    CHAT_TEXT_MIN,
};

/// One validation problem: where (`colors.bg`) and what.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Issue {
    pub path: Vec<String>,
    pub message: String,
}

impl fmt::Display for Issue {
    /// `path: message`, or just the message at the root (as parseThemeFile words it).
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.path.is_empty() {
            f.write_str(&self.message)
        } else {
            write!(f, "{}: {}", self.path.join("."), self.message)
        }
    }
}

pub(crate) type Check<T> = Result<T, Issue>;

fn issue<T>(message: impl Into<String>) -> Check<T> {
    Err(Issue {
        path: Vec::new(),
        message: message.into(),
    })
}

/// Puts a key in front of the path of a problem found below it.
pub(crate) fn at<T>(key: &str, r: Check<T>) -> Check<T> {
    r.map_err(|mut i| {
        i.path.insert(0, key.to_owned());
        i
    })
}

/// zod's name for what it got instead.
fn received(v: Option<&Value>) -> &'static str {
    match v {
        None => "undefined",
        Some(Value::Null) => "null",
        Some(Value::Bool(_)) => "boolean",
        Some(Value::Number(_)) => "number",
        Some(Value::String(_)) => "string",
        Some(Value::Array(_)) => "array",
        Some(Value::Object(_)) => "object",
    }
}

fn invalid_type<T>(expected: &str, v: Option<&Value>) -> Check<T> {
    issue(format!(
        "Invalid input: expected {expected}, received {}",
        received(v)
    ))
}

pub(crate) fn object(v: Option<&Value>) -> Check<&Map<String, Value>> {
    match v {
        Some(Value::Object(o)) => Ok(o),
        _ => invalid_type("object", v),
    }
}

pub(crate) fn string(v: Option<&Value>) -> Check<&str> {
    match v {
        Some(Value::String(s)) => Ok(s),
        _ => invalid_type("string", v),
    }
}

/// `String.prototype.trim`: ECMAScript white space and line terminators (not Rust's set,
/// which keeps U+FEFF and drops U+0085).
pub(crate) fn js_trim(s: &str) -> &str {
    fn ws(c: char) -> bool {
        matches!(
            c,
            '\t' | '\n' | '\u{b}' | '\u{c}' | '\r' | ' ' | '\u{a0}' | '\u{1680}' | '\u{2000}'
                ..='\u{200a}'
                    | '\u{2028}'
                    | '\u{2029}'
                    | '\u{202f}'
                    | '\u{205f}'
                    | '\u{3000}'
                    | '\u{feff}'
        )
    }
    s.trim_matches(ws)
}

/// A trimmed string between `min` and `max` characters (code points, as zod counts).
pub(crate) fn trimmed(v: Option<&Value>, min: usize, max: usize) -> Check<String> {
    let s = js_trim(string(v)?);
    let n = s.chars().count();
    if n < min {
        return issue(format!(
            "Too small: expected string to have >={min} characters"
        ));
    }
    if n > max {
        return issue(format!(
            "Too big: expected string to have <={max} characters"
        ));
    }
    Ok(s.to_owned())
}

/// `/^[^\p{Cc}<>]*$/u`: no control characters, no `<` or `>`.
pub(crate) fn plain_text(s: &str) -> bool {
    !s.chars().any(|c| c.is_control() || c == '<' || c == '>')
}

pub(crate) const TEXT_PATTERN: &str = "Invalid string: must match pattern /^[^\\p{Cc}<>]*$/u";

/// One of fixed options, by their JSON names.
pub(crate) fn one_of<T: Copy>(
    v: Option<&Value>,
    all: &[T],
    name: fn(T) -> &'static str,
) -> Check<T> {
    let s = match v {
        Some(Value::String(s)) => Some(s.as_str()),
        _ => None,
    };
    if let Some(t) = all.iter().copied().find(|t| Some(name(*t)) == s) {
        return Ok(t);
    }
    let names: Vec<String> = all.iter().map(|t| format!("\"{}\"", name(*t))).collect();
    issue(format!(
        "Invalid option: expected one of {}",
        names.join("|")
    ))
}

/// The keys an object may not have (a zod `.strict()` object), named in input order.
fn no_other_keys(o: &Map<String, Value>, known: impl Fn(&str) -> bool) -> Check<()> {
    let extra: Vec<String> = o
        .keys()
        .filter(|k| !known(k))
        .map(|k| format!("\"{k}\""))
        .collect();
    match extra.len() {
        0 => Ok(()),
        1 => issue(format!("Unrecognized key: {}", extra[0])),
        _ => issue(format!("Unrecognized keys: {}", extra.join(", "))),
    }
}

/// `#rgb`, `#rrggbb` or `#rrggbbaa`, stored as lowercase `#rrggbb[aa]`.
fn color(v: Option<&Value>) -> Check<String> {
    let s = string(v)?;
    let hex = s.strip_prefix('#').unwrap_or("");
    let ok = s.starts_with('#')
        && matches!(hex.len(), 3 | 6 | 8)
        && hex.chars().all(|c| c.is_ascii_hexdigit());
    if !ok {
        return issue("must be a hex color like #1f2937");
    }
    let v = s.to_ascii_lowercase();
    Ok(if hex.len() == 3 {
        let b = v.as_bytes();
        let (r, g, b) = (b[1] as char, b[2] as char, b[3] as char);
        format!("#{r}{r}{g}{g}{b}{b}")
    } else {
        v
    })
}

fn boolean(v: Option<&Value>) -> Check<bool> {
    match v {
        Some(Value::Bool(b)) => Ok(*b),
        _ => invalid_type("boolean", v),
    }
}

/// `z.number().int().min(min).max(max)`.
fn int_between(v: Option<&Value>, min: f64, max: f64) -> Check<f64> {
    const SAFE: f64 = 9007199254740991.0;
    let Some(n) = v.and_then(Value::as_f64) else {
        return invalid_type("number", v);
    };
    if n.fract() != 0.0 {
        return issue("Invalid input: expected int, received number");
    }
    if n > SAFE {
        return issue("Too big: expected int to be <=9007199254740991");
    }
    if n < -SAFE {
        return issue("Too small: expected int to be >=-9007199254740991");
    }
    if n < min {
        return issue(format!("Too small: expected number to be >={min}"));
    }
    if n > max {
        return issue(format!("Too big: expected number to be <={max}"));
    }
    Ok(n)
}

/// An optional key: absent is fine, anything present is checked (null too).
fn optional<'a, T>(
    o: &'a Map<String, Value>,
    key: &str,
    check: impl FnOnce(Option<&'a Value>) -> Check<T>,
) -> Check<Option<T>> {
    match o.get(key) {
        None => Ok(None),
        v => at(key, check(v)).map(Some),
    }
}

fn colors(v: Option<&Value>) -> Check<ThemeColors> {
    let o = object(v)?;
    let mut out = ThemeColors::default();
    for &c in ThemeColor::ALL {
        let k = c.key();
        match o.get(k) {
            None if c.required() => return at(k, invalid_type("string", None)),
            None => {}
            v => out.set(c, at(k, color(v))?),
        }
    }
    no_other_keys(o, |k| ThemeColor::from_key(k).is_some())?;
    Ok(out)
}

fn terminal(v: Option<&Value>) -> Check<ThemeTerminalColors> {
    let o = object(v)?;
    let mut out = ThemeTerminalColors::default();
    for &c in TerminalColor::ALL {
        if let Some(col) = optional(o, c.key(), color)? {
            out.set(c, col);
        }
    }
    no_other_keys(o, |k| TerminalColor::from_key(k).is_some())?;
    Ok(out)
}

const STYLE_KEYS: [&str; 9] = [
    "font",
    "monoFont",
    "chatTextSize",
    "radius",
    "border",
    "depth",
    "density",
    "motion",
    "frameGradient",
];

fn style(v: Option<&Value>) -> Check<ThemeStyle> {
    let o = object(v)?;
    let size = |v| int_between(v, CHAT_TEXT_MIN.into(), CHAT_TEXT_MAX.into());
    let s = ThemeStyle {
        font: optional(o, "font", |v| one_of(v, UiFont::ALL, UiFont::as_str))?,
        mono_font: optional(o, "monoFont", |v| {
            one_of(v, MonoFont::ALL, MonoFont::as_str)
        })?,
        chat_text_size: optional(o, "chatTextSize", size)?.map(|n| n as u8),
        radius: optional(o, "radius", |v| {
            one_of(v, ThemeRadius::ALL, ThemeRadius::as_str)
        })?,
        border: optional(o, "border", |v| {
            one_of(v, ThemeBorder::ALL, ThemeBorder::as_str)
        })?,
        depth: optional(o, "depth", |v| {
            one_of(v, ThemeDepth::ALL, ThemeDepth::as_str)
        })?,
        density: optional(o, "density", |v| {
            one_of(v, ThemeDensity::ALL, ThemeDensity::as_str)
        })?,
        motion: optional(o, "motion", |v| {
            one_of(v, ThemeMotion::ALL, ThemeMotion::as_str)
        })?,
        frame_gradient: optional(o, "frameGradient", boolean)?,
    };
    no_other_keys(o, |k| STYLE_KEYS.contains(&k))?;
    Ok(s)
}

fn theme_file(raw: &Value) -> Check<ThemeFile> {
    let o = object(Some(raw))?;
    let name = at("name", {
        trimmed(o.get("name"), 1, 40).and_then(|s| {
            if plain_text(&s) {
                Ok(s)
            } else {
                issue("has characters a name cannot use")
            }
        })
    })?;
    let mode = at(
        "mode",
        one_of(o.get("mode"), ThemeName::ALL, ThemeName::as_str),
    )?;
    let author = optional(o, "author", |v| {
        trimmed(v, 0, 60).and_then(|s| {
            if plain_text(&s) {
                Ok(s)
            } else {
                issue(TEXT_PATTERN)
            }
        })
    })?;
    let colors = at("colors", colors(o.get("colors")))?;
    let terminal = optional(o, "terminal", terminal)?;
    let style = optional(o, "style", style)?;
    // Other top-level keys are dropped, not refused (the object is not strict).
    Ok(ThemeFile {
        name,
        mode,
        author,
        colors,
        terminal,
        style,
    })
}

/// Validates an untrusted theme file's parsed JSON. The error is what the UI shows.
pub fn parse_theme_file(raw: &Value) -> Result<ThemeFile, String> {
    theme_file(raw).map_err(|i| i.to_string())
}
