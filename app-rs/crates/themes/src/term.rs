//! Terminal colors (src/renderer/lib/term-theme.ts): `match` takes the drawn app theme's own
//! terminal colors (accent included), the other palettes are fixed schemes. The fields are
//! the ones xterm's `ITheme` had, as `#rrggbb` or `rgba(…)` strings.

use crate::prefs::TerminalPalette;
use crate::theme::{hex_to_rgba, theme_terminal, ThemeFile};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TerminalTheme {
    pub background: String,
    pub foreground: String,
    pub cursor: String,
    pub cursor_accent: String,
    pub selection_background: String,
    pub scrollbar_slider_background: String,
    pub scrollbar_slider_hover_background: String,
    pub scrollbar_slider_active_background: String,
    /// black, red, green, yellow, blue, magenta, cyan, white, then the bright eight.
    pub ansi: [String; 16],
}

struct Scheme {
    label: &'static str,
    bg: &'static str,
    fg: &'static str,
    cursor: &'static str,
    selection: &'static str,
    ansi: [&'static str; 16],
}

#[rustfmt::skip]
fn scheme(p: TerminalPalette) -> Option<Scheme> {
    Some(match p {
        TerminalPalette::Match => return None,
        TerminalPalette::Dracula => Scheme {
            label: "Dracula", bg: "#282a36", fg: "#f8f8f2", cursor: "#f8f8f2", selection: "#44475a",
            ansi: [
                "#21222c", "#ff5555", "#50fa7b", "#f1fa8c", "#bd93f9", "#ff79c6", "#8be9fd", "#f8f8f2",
                "#6272a4", "#ff6e6e", "#69ff94", "#ffffa5", "#d6acff", "#ff92df", "#a4ffff", "#ffffff",
            ],
        },
        TerminalPalette::Nord => Scheme {
            label: "Nord", bg: "#2e3440", fg: "#d8dee9", cursor: "#d8dee9", selection: "#434c5e",
            ansi: [
                "#3b4252", "#bf616a", "#a3be8c", "#ebcb8b", "#81a1c1", "#b48ead", "#88c0d0", "#e5e9f0",
                "#616e88", "#bf616a", "#a3be8c", "#ebcb8b", "#81a1c1", "#b48ead", "#8fbcbb", "#eceff4",
            ],
        },
        TerminalPalette::TokyoNight => Scheme {
            label: "Tokyo Night", bg: "#1a1b26", fg: "#c0caf5", cursor: "#c0caf5", selection: "#33467c",
            ansi: [
                "#15161e", "#f7768e", "#9ece6a", "#e0af68", "#7aa2f7", "#bb9af7", "#7dcfff", "#a9b1d6",
                "#565f89", "#f7768e", "#9ece6a", "#e0af68", "#7aa2f7", "#bb9af7", "#7dcfff", "#c0caf5",
            ],
        },
        TerminalPalette::GruvboxDark => Scheme {
            label: "Gruvbox", bg: "#282828", fg: "#ebdbb2", cursor: "#ebdbb2", selection: "#504945",
            ansi: [
                "#3c3836", "#cc241d", "#98971a", "#d79921", "#458588", "#b16286", "#689d6a", "#a89984",
                "#928374", "#fb4934", "#b8bb26", "#fabd2f", "#83a598", "#d3869b", "#8ec07c", "#ebdbb2",
            ],
        },
        TerminalPalette::SolarizedDark => Scheme {
            label: "Solarized Dark", bg: "#002b36", fg: "#93a1a1", cursor: "#93a1a1", selection: "#073642",
            ansi: [
                "#073642", "#dc322f", "#859900", "#b58900", "#268bd2", "#d33682", "#2aa198", "#eee8d5",
                "#657b83", "#cb4b16", "#859900", "#b58900", "#268bd2", "#6c71c4", "#2aa198", "#fdf6e3",
            ],
        },
        TerminalPalette::SolarizedLight => Scheme {
            label: "Solarized Light", bg: "#fdf6e3", fg: "#586e75", cursor: "#586e75", selection: "#eee8d5",
            ansi: [
                "#073642", "#dc322f", "#859900", "#b58900", "#268bd2", "#d33682", "#2aa198", "#93a1a1",
                "#839496", "#cb4b16", "#859900", "#b58900", "#268bd2", "#6c71c4", "#2aa198", "#073642",
            ],
        },
    })
}

/// `#rrggbb` → `rgba(r, g, b, a)`.
fn alpha(hex: &str, a: f64) -> String {
    hex_to_rgba(hex, Some(a))
}

fn from_scheme(s: &Scheme) -> TerminalTheme {
    TerminalTheme {
        background: s.bg.into(),
        foreground: s.fg.into(),
        cursor: s.cursor.into(),
        cursor_accent: s.bg.into(),
        selection_background: s.selection.into(),
        scrollbar_slider_background: alpha(s.fg, 0.2),
        scrollbar_slider_hover_background: alpha(s.fg, 0.35),
        scrollbar_slider_active_background: alpha(s.fg, 0.5),
        ansi: s.ansi.map(str::to_owned),
    }
}

/// The app theme's own terminal colors as a terminal theme.
fn from_app_theme(t: &ThemeFile) -> TerminalTheme {
    let c = theme_terminal(t);
    let sel = if c.selection.len() == 9 {
        hex_to_rgba(&c.selection, None)
    } else {
        alpha(&c.selection, 0.3)
    };
    TerminalTheme {
        cursor_accent: c.background.clone(),
        selection_background: sel,
        scrollbar_slider_background: alpha(&c.foreground, 0.2),
        scrollbar_slider_hover_background: alpha(&c.foreground, 0.35),
        scrollbar_slider_active_background: alpha(&c.cursor, 0.5),
        background: c.background,
        foreground: c.foreground,
        cursor: c.cursor,
        ansi: c.ansi,
    }
}

/// A palette's name in Settings.
pub fn palette_label(p: TerminalPalette) -> &'static str {
    scheme(p).map_or("Match app", |s| s.label)
}

/// The terminal theme for the current settings: `match` follows the drawn app theme (accent
/// included), the rest are fixed schemes. (The TS caches these for object identity; here
/// they are cheap values the caller can keep.)
pub fn terminal_theme(palette: TerminalPalette, drawn: &ThemeFile) -> TerminalTheme {
    match scheme(palette) {
        Some(s) => from_scheme(&s),
        None => from_app_theme(drawn),
    }
}

/// Requested font first, bundled JetBrains Mono next, then the system monospace font.
pub fn terminal_font_stack(family: &str) -> String {
    format!("'{family}', 'JetBrains Mono', ui-monospace, monospace")
}
