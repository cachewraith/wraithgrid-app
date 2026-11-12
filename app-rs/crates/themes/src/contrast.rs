//! WCAG contrast for the theme editor (#24, src/shared/contrast.ts): which text and button
//! colors are hard to read on what they sit on, and a nearby color that passes. Hex in,
//! numbers out.

use crate::theme::{ThemeColor, ThemeFile};

fn rgb(hex: &str) -> [f64; 3] {
    let n = hex
        .get(1..7)
        .and_then(|h| u32::from_str_radix(h, 16).ok())
        .unwrap_or(0);
    [(n >> 16) & 255, (n >> 8) & 255, n & 255].map(f64::from)
}

fn to_hex(c: [f64; 3]) -> String {
    let [r, g, b] = c.map(|v| v.round() as u8);
    format!("#{r:02x}{g:02x}{b:02x}")
}

/// Relative luminance (WCAG 2.x) of `#rrggbb[aa]` (alpha ignored).
pub fn luminance(hex: &str) -> f64 {
    let [r, g, b] = rgb(hex).map(|v| {
        let s = v / 255.0;
        if s <= 0.03928 {
            s / 12.92
        } else {
            ((s + 0.055) / 1.055).powf(2.4)
        }
    });
    0.2126 * r + 0.7152 * g + 0.0722 * b
}

/// Contrast ratio between two colors, 1–21.
pub fn contrast_ratio(a: &str, b: &str) -> f64 {
    let (x, y) = (luminance(a), luminance(b));
    let (hi, lo) = if x >= y { (x, y) } else { (y, x) };
    (hi + 0.05) / (lo + 0.05)
}

/// `fg` moved toward black or white (whichever gains contrast) until it reaches `need`.
pub fn passing_color(fg: &str, bg: &str, need: f64) -> String {
    let towards = if luminance(bg) > 0.5 {
        [0.0; 3]
    } else {
        [255.0; 3]
    };
    let from = rgb(fg);
    // Steps of 0.05 summed as the TS sums them, so the same color comes out.
    let mut t = 0.05;
    while t <= 1.0001 {
        let c = [0, 1, 2].map(|i| from[i] + (towards[i] - from[i]) * t);
        if contrast_ratio(&to_hex(c), bg) >= need {
            return to_hex(c);
        }
        t += 0.05;
    }
    to_hex(towards)
}

#[derive(Clone, Debug, PartialEq)]
pub struct ContrastIssue {
    /// The color to change, and what it is drawn on.
    pub fg: ThemeColor,
    pub bg: ThemeColor,
    pub label: &'static str,
    /// Rounded down to one decimal.
    pub ratio: f64,
    pub need: f64,
    /// The nearest color that passes.
    pub fix: String,
}

/// Text and buttons, and the WCAG AA level each needs (4.5 text, 3 large/UI).
#[rustfmt::skip]
const PAIRS: [(ThemeColor, ThemeColor, &str, f64); 9] = [
    (ThemeColor::Text, ThemeColor::Bg, "Text on the background", 4.5),
    (ThemeColor::Text, ThemeColor::Panel, "Text on menus and cards", 4.5),
    (ThemeColor::Muted, ThemeColor::Bg, "Muted text on the background", 4.5),
    (ThemeColor::Faint, ThemeColor::Bg, "Hints and timestamps on the background", 4.5),
    (ThemeColor::AccentText, ThemeColor::Bg, "Links and accent text", 4.5),
    (ThemeColor::OnAccent, ThemeColor::AccentFill, "Button text on accent buttons", 4.5),
    (ThemeColor::Text, ThemeColor::UserBubble, "Your messages in chat", 4.5),
    (ThemeColor::Err, ThemeColor::Bg, "Error text", 3.0),
    (ThemeColor::Warn, ThemeColor::Bg, "Warnings", 3.0),
];

/// `^#[0-9a-f]{6}([0-9a-f]{2})?$`
fn hex6(s: &str) -> bool {
    let h = s.strip_prefix('#').unwrap_or("");
    s.starts_with('#')
        && matches!(h.len(), 6 | 8)
        && h.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// The pairs of a theme's own colors that fall short (colors it leaves out are not judged).
pub fn contrast_issues(t: &ThemeFile) -> Vec<ContrastIssue> {
    let mut out = Vec::new();
    for (fg, bg, label, need) in PAIRS {
        let (Some(f), Some(b)) = (t.colors.get(fg), t.colors.get(bg)) else {
            continue;
        };
        if f.is_empty() || b.is_empty() || !hex6(f) || !hex6(b) {
            continue;
        }
        let ratio = contrast_ratio(f, b);
        if ratio < need {
            out.push(ContrastIssue {
                fg,
                bg,
                label,
                ratio: (ratio * 10.0).floor() / 10.0,
                need,
                fix: passing_color(f, b, need),
            });
        }
    }
    out
}
