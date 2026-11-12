//! App themes (src/shared/themes.ts): the five built in, and the shape of the theme files
//! people make (~/.wraithgrid/themes/*.json). A theme is colors and a few fixed style
//! options only. Every color is checked against a strict hex pattern (validate.rs), so a
//! theme file can never carry CSS or script. Also: accents laid over a theme, a theme as the
//! CSS variables the renderer sets, and a theme's terminal colors.

use std::sync::LazyLock;

use serde::{Deserialize, Serialize};
use unicode_normalization::UnicodeNormalization;

use crate::prefs::{Accent, ThemeName};

pub const THEMES_FILE_MAX: usize = 64 * 1024;
pub const THEMES_MAX: usize = 100;
pub const CHAT_TEXT_MIN: u8 = 13;
pub const CHAT_TEXT_MAX: u8 = 17;

/// A color slot: required ones are a `String`, optional ones an `Option<String>`.
trait Slot {
    fn get(&self) -> Option<&str>;
    fn put(&mut self, v: String);
    fn absent(&self) -> bool;
}

impl Slot for String {
    fn get(&self) -> Option<&str> {
        Some(self)
    }
    fn put(&mut self, v: String) {
        *self = v;
    }
    fn absent(&self) -> bool {
        false
    }
}

impl Slot for Option<String> {
    fn get(&self) -> Option<&str> {
        self.as_deref()
    }
    fn put(&mut self, v: String) {
        *self = Some(v);
    }
    fn absent(&self) -> bool {
        self.is_none()
    }
}

/// A set of named colors: an enum of the names and a struct with one field each. Fields are
/// in the file's key order, so a saved file keeps the order the Electron app writes.
macro_rules! color_set {
    (
        $(#[$emeta:meta])* enum $enum_name:ident;
        $(#[$smeta:meta])* struct $name:ident;
        $( $variant:ident $field:ident: $ty:ty = $key:literal ),* $(,)?
    ) => {
        $(#[$emeta])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub enum $enum_name { $($variant),* }

        impl $enum_name {
            /// Every name, in the file's key order.
            pub const ALL: &'static [$enum_name] = &[$($enum_name::$variant),*];
            /// The key in a theme file.
            pub fn key(self) -> &'static str {
                match self { $($enum_name::$variant => $key),* }
            }
            pub fn from_key(key: &str) -> Option<Self> {
                match key { $($key => Some($enum_name::$variant),)* _ => None }
            }
        }

        $(#[$smeta])*
        #[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
        pub struct $name {
            $(
                #[serde(rename = $key, default, skip_serializing_if = "Slot::absent")]
                pub $field: $ty,
            )*
        }

        impl $name {
            pub fn get(&self, c: $enum_name) -> Option<&str> {
                match c { $($enum_name::$variant => Slot::get(&self.$field)),* }
            }
            pub fn set(&mut self, c: $enum_name, v: impl Into<String>) {
                match c { $($enum_name::$variant => Slot::put(&mut self.$field, v.into())),* }
            }
        }
    };
}

color_set! {
    /// A theme file's color names (the TS `ThemeColorName`).
    enum ThemeColor;
    /// A theme's colors. `bg`, `text` and `accent` are required; the rest are mixed from
    /// those when left out.
    struct ThemeColors;
    Bg bg: String = "bg",
    Sidebar sidebar: Option<String> = "sidebar",
    Panel panel: Option<String> = "panel",
    Raised raised: Option<String> = "raised",
    Line line: Option<String> = "line",
    LineStrong line_strong: Option<String> = "lineStrong",
    Text text: String = "text",
    Muted muted: Option<String> = "muted",
    Faint faint: Option<String> = "faint",
    Accent accent: String = "accent",
    AccentText accent_text: Option<String> = "accentText",
    AccentFill accent_fill: Option<String> = "accentFill",
    AccentFillHover accent_fill_hover: Option<String> = "accentFillHover",
    OnAccent on_accent: Option<String> = "onAccent",
    Terminal terminal: Option<String> = "terminal",
    Ok ok: Option<String> = "ok",
    Warn warn: Option<String> = "warn",
    Err err: Option<String> = "err",
    Info info: Option<String> = "info",
    Selection selection: Option<String> = "selection",
    Hover hover: Option<String> = "hover",
    Frame frame: Option<String> = "frame",
    // Chat and code (#24); left out, they are mixed from the colors above.
    UserBubble user_bubble: Option<String> = "userBubble",
    CodeBlock code_block: Option<String> = "codeBlock",
    DiffAdd diff_add: Option<String> = "diffAdd",
    DiffDel diff_del: Option<String> = "diffDel",
}

impl ThemeColor {
    /// The CSS variable it sets (tokens.css); the TS `THEME_COLOR_VARS`.
    pub fn css_var(self) -> &'static str {
        match self {
            ThemeColor::Bg => "--bg",
            ThemeColor::Sidebar => "--bg2",
            ThemeColor::Panel => "--panel",
            ThemeColor::Raised => "--raise",
            ThemeColor::Line => "--line",
            ThemeColor::LineStrong => "--line2",
            ThemeColor::Text => "--tx",
            ThemeColor::Muted => "--mu",
            ThemeColor::Faint => "--fa",
            ThemeColor::Accent => "--acc",
            ThemeColor::AccentText => "--acct",
            ThemeColor::AccentFill => "--accf",
            ThemeColor::AccentFillHover => "--accf-h",
            ThemeColor::OnAccent => "--on-acc",
            ThemeColor::Terminal => "--term",
            ThemeColor::Ok => "--ok",
            ThemeColor::Warn => "--warn",
            ThemeColor::Err => "--err",
            ThemeColor::Info => "--cy",
            ThemeColor::Selection => "--sel",
            ThemeColor::Hover => "--hover",
            ThemeColor::Frame => "--frame",
            ThemeColor::UserBubble => "--user-bg",
            ThemeColor::CodeBlock => "--code-bg",
            ThemeColor::DiffAdd => "--addbg",
            ThemeColor::DiffDel => "--delbg",
        }
    }

    /// The three every theme file must give.
    pub fn required(self) -> bool {
        matches!(self, ThemeColor::Bg | ThemeColor::Text | ThemeColor::Accent)
    }
}

color_set! {
    /// A theme file's terminal color names.
    enum TerminalColor;
    /// A theme's own terminal colors, all optional.
    struct ThemeTerminalColors;
    Background background: Option<String> = "background",
    Foreground foreground: Option<String> = "foreground",
    Cursor cursor: Option<String> = "cursor",
    Selection selection: Option<String> = "selection",
    Black black: Option<String> = "black",
    Red red: Option<String> = "red",
    Green green: Option<String> = "green",
    Yellow yellow: Option<String> = "yellow",
    Blue blue: Option<String> = "blue",
    Magenta magenta: Option<String> = "magenta",
    Cyan cyan: Option<String> = "cyan",
    White white: Option<String> = "white",
    BrightBlack bright_black: Option<String> = "brightBlack",
    BrightRed bright_red: Option<String> = "brightRed",
    BrightGreen bright_green: Option<String> = "brightGreen",
    BrightYellow bright_yellow: Option<String> = "brightYellow",
    BrightBlue bright_blue: Option<String> = "brightBlue",
    BrightMagenta bright_magenta: Option<String> = "brightMagenta",
    BrightCyan bright_cyan: Option<String> = "brightCyan",
    BrightWhite bright_white: Option<String> = "brightWhite",
}

impl TerminalColor {
    /// The sixteen ANSI colors: black … white, then the bright eight.
    pub const ANSI: [TerminalColor; 16] = [
        TerminalColor::Black,
        TerminalColor::Red,
        TerminalColor::Green,
        TerminalColor::Yellow,
        TerminalColor::Blue,
        TerminalColor::Magenta,
        TerminalColor::Cyan,
        TerminalColor::White,
        TerminalColor::BrightBlack,
        TerminalColor::BrightRed,
        TerminalColor::BrightGreen,
        TerminalColor::BrightYellow,
        TerminalColor::BrightBlue,
        TerminalColor::BrightMagenta,
        TerminalColor::BrightCyan,
        TerminalColor::BrightWhite,
    ];
}

#[rustfmt::skip]
pub const ANSI_NAMES: [&str; 16] = [
    "black", "red", "green", "yellow", "blue", "magenta", "cyan", "white",
    "brightBlack", "brightRed", "brightGreen", "brightYellow",
    "brightBlue", "brightMagenta", "brightCyan", "brightWhite",
];

// ---- Style: everything but colors (#24). Fixed options only, never raw CSS. ------------

str_enum! {
    /// UI fonts a theme may name. Geist is bundled; others if installed.
    pub enum UiFont {
        Geist = "Geist",
        System = "System",
        Inter = "Inter",
        IbmPlexSans = "IBM Plex Sans",
        SourceSans3 = "Source Sans 3",
        NotoSans = "Noto Sans",
    }
}

impl UiFont {
    /// The font stack used.
    pub fn stack(self) -> &'static str {
        match self {
            UiFont::Geist => "'Geist', ui-sans-serif, system-ui, sans-serif",
            UiFont::System => {
                "system-ui, -apple-system, 'Segoe UI', Roboto, 'Helvetica Neue', sans-serif"
            }
            UiFont::Inter => "'Inter', 'Geist', ui-sans-serif, system-ui, sans-serif",
            UiFont::IbmPlexSans => "'IBM Plex Sans', 'Geist', ui-sans-serif, system-ui, sans-serif",
            UiFont::SourceSans3 => "'Source Sans 3', 'Geist', ui-sans-serif, system-ui, sans-serif",
            UiFont::NotoSans => "'Noto Sans', 'Geist', ui-sans-serif, system-ui, sans-serif",
        }
    }
}

str_enum! {
    /// Mono fonts a theme may name. JetBrains Mono is bundled; others if installed.
    pub enum MonoFont {
        JetBrainsMono = "JetBrains Mono",
        System = "System",
        IbmPlexMono = "IBM Plex Mono",
        FiraCode = "Fira Code",
        CascadiaCode = "Cascadia Code",
    }
}

impl MonoFont {
    /// The font stack used.
    pub fn stack(self) -> &'static str {
        match self {
            MonoFont::JetBrainsMono => "'JetBrains Mono', ui-monospace, monospace",
            MonoFont::System => {
                "ui-monospace, 'SF Mono', Menlo, Consolas, 'DejaVu Sans Mono', monospace"
            }
            MonoFont::IbmPlexMono => "'IBM Plex Mono', 'JetBrains Mono', ui-monospace, monospace",
            MonoFont::FiraCode => "'Fira Code', 'JetBrains Mono', ui-monospace, monospace",
            MonoFont::CascadiaCode => "'Cascadia Code', 'JetBrains Mono', ui-monospace, monospace",
        }
    }
}

str_enum! {
    /// Corner radius: a scale on every rounded corner (`--rs`).
    pub enum ThemeRadius {
        Small = "small",
        Medium = "medium",
        Large = "large",
    }
}

impl ThemeRadius {
    pub fn scale(self) -> f64 {
        match self {
            ThemeRadius::Small => 0.6,
            ThemeRadius::Medium => 1.2,
            ThemeRadius::Large => 1.6,
        }
    }
}

str_enum! {
    pub enum ThemeBorder {
        Thin = "thin",
        Thick = "thick",
    }
}

impl ThemeBorder {
    /// The CSS width (`--bw`).
    pub fn css(self) -> &'static str {
        match self {
            ThemeBorder::Thin => "1px",
            ThemeBorder::Thick => "1.5px",
        }
    }
    pub fn px(self) -> f64 {
        match self {
            ThemeBorder::Thin => 1.0,
            ThemeBorder::Thick => 1.5,
        }
    }
}

str_enum! {
    pub enum ThemeDepth {
        Flat = "flat",
        Soft = "soft",
        Deep = "deep",
    }
}

str_enum! {
    pub enum ThemeDensity {
        Compact = "compact",
        Comfortable = "comfortable",
        Spacious = "spacious",
    }
}

impl ThemeDensity {
    pub fn scale(self) -> f64 {
        match self {
            ThemeDensity::Compact => 0.85,
            ThemeDensity::Comfortable => 1.0,
            ThemeDensity::Spacious => 1.15,
        }
    }
}

str_enum! {
    /// Animation: `prefers-reduced-motion` always wins over `on`.
    pub enum ThemeMotion {
        On = "on",
        Reduced = "reduced",
        Off = "off",
    }
}

/// A theme's style; every key optional.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThemeStyle {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub font: Option<UiFont>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mono_font: Option<MonoFont>,
    /// Chat message text, px.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chat_text_size: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub radius: Option<ThemeRadius>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub border: Option<ThemeBorder>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub depth: Option<ThemeDepth>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub density: Option<ThemeDensity>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub motion: Option<ThemeMotion>,
    /// A faint accent glow over the window frame.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frame_gradient: Option<bool>,
}

/// A theme as a file holds it (and as the built-ins are written). Serializes to the file's
/// keys in the file's order.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThemeFile {
    pub name: String,
    pub mode: ThemeName,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    pub colors: ThemeColors,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal: Option<ThemeTerminalColors>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub style: Option<ThemeStyle>,
}

/// A theme the app can draw: built in, or loaded from the themes folder.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct AppTheme {
    pub id: String,
    #[serde(rename = "builtIn")]
    pub built_in: bool,
    #[serde(flatten)]
    pub theme: ThemeFile,
}

impl std::ops::Deref for AppTheme {
    type Target = ThemeFile;
    fn deref(&self) -> &ThemeFile {
        &self.theme
    }
}

/// A theme name as a file name / id: `My Ocean!` → `my-ocean`.
pub fn theme_slug(name: &str) -> String {
    let lower: String = name.to_lowercase().nfkd().collect();
    let mut s = String::new();
    let mut gap = false;
    for c in lower.chars() {
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            if gap && !s.is_empty() {
                s.push('-');
            }
            gap = false;
            s.push(c);
        } else {
            gap = true;
        }
    }
    // Leading and trailing dashes are dropped, then 40 characters kept (all ASCII here).
    s.truncate(40);
    if s.is_empty() {
        "theme".into()
    } else {
        s
    }
}

// ---- Built-in themes --------------------------------------------------------------

/// Builds a built-in theme from its colors in `ThemeColor::ALL` order, its terminal
/// background, foreground, cursor and selection, and its sixteen ANSI colors.
fn built_in(
    id: &str,
    name: &str,
    mode: ThemeName,
    style: ThemeStyle,
    colors: [&str; 26],
    term: [&str; 4],
    ansi: [&str; 16],
) -> AppTheme {
    let mut c = ThemeColors::default();
    for (k, v) in ThemeColor::ALL.iter().zip(colors) {
        c.set(*k, v);
    }
    let mut t = ThemeTerminalColors::default();
    for (k, v) in TerminalColor::ALL
        .iter()
        .zip(term.iter().chain(ansi.iter()))
    {
        t.set(*k, *v);
    }
    AppTheme {
        id: id.into(),
        built_in: true,
        theme: ThemeFile {
            name: name.into(),
            mode,
            author: None,
            colors: c,
            terminal: Some(t),
            style: Some(style),
        },
    }
}

#[rustfmt::skip]
static BUILT_IN_THEMES: LazyLock<Vec<AppTheme>> = LazyLock::new(|| vec![
    built_in(
        "wraith", "Wraith", ThemeName::Dark,
        ThemeStyle {
            radius: Some(ThemeRadius::Medium), depth: Some(ThemeDepth::Soft),
            density: Some(ThemeDensity::Comfortable), ..Default::default()
        },
        [
            "#1c1c1c", "#171717", "#232323", "#2a2a2a",
            "#2b2b2b", "#3a3a3a", "#ececec", "#b4b4b4", "#8f8f8f",
            "#7c5cff", "#a996ff", "#6a4cf0", "#5a3de0",
            "#ffffff", "#1c1c1c", "#4ade80", "#fbbf24", "#fb7185",
            "#7dd3fc", "#2a2a2a", "#242424", "#121212",
            "#343044", "#181818", "#4ade801f", "#fb71851f",
        ],
        ["#1c1c1c", "#ececec", "#a996ff", "#7c5cff61"],
        [
            "#2b2b2b", "#fb7185", "#4ade80", "#fbbf24", "#8ab4ff", "#a996ff", "#7dd3fc", "#ececec",
            "#8f8f8f", "#fda4af", "#86efac", "#fde68a", "#b4ccff", "#c4b8ff", "#bae6fd", "#ffffff",
        ],
    ),
    built_in(
        "midnight", "Midnight", ThemeName::Dark,
        ThemeStyle {
            font: Some(UiFont::System), depth: Some(ThemeDepth::Flat),
            density: Some(ThemeDensity::Compact), chat_text_size: Some(14), ..Default::default()
        },
        [
            "#0f1420", "#0b0f19", "#151b29", "#1b2333",
            "#1e2636", "#2b3548", "#e2e8f5", "#a6b0c3", "#7c879c",
            "#22d3ee", "#67e8f9", "#0891b2", "#0e7490",
            "#ffffff", "#0f1420", "#4ade80", "#fbbf24", "#fb7185",
            "#7dd3fc", "#1b2333", "#141a27", "#080b12",
            "#1c3849", "#0c101b", "#4ade801f", "#fb71851f",
        ],
        ["#0f1420", "#e2e8f5", "#67e8f9", "#22d3ee4d"],
        [
            "#1e2636", "#fb7185", "#4ade80", "#fbbf24", "#7aa2f7", "#c4a7ff", "#67e8f9", "#e2e8f5",
            "#7c879c", "#fda4af", "#86efac", "#fde68a", "#a5c0ff", "#ddd0ff", "#a5f3fc", "#ffffff",
        ],
    ),
    built_in(
        "clay", "Clay", ThemeName::Dark,
        ThemeStyle {
            depth: Some(ThemeDepth::Deep), density: Some(ThemeDensity::Spacious),
            chat_text_size: Some(15), frame_gradient: Some(true), ..Default::default()
        },
        [
            "#1f1b18", "#191613", "#26211d", "#2e2823",
            "#2f2924", "#3f3730", "#f0e9e1", "#c2b6a8", "#968a7d",
            "#d97757", "#eba184", "#c8643f", "#b05533",
            "#ffffff", "#1f1b18", "#8fcf7a", "#f2b84b", "#f07a7a",
            "#8ec5d6", "#2e2823", "#26211d", "#12100e",
            "#433129", "#1a1714", "#8fcf7a1f", "#f07a7a1f",
        ],
        ["#1f1b18", "#f0e9e1", "#eba184", "#d977574d"],
        [
            "#2f2924", "#f07a7a", "#8fcf7a", "#f2b84b", "#8fb3e0", "#d9a0c8", "#8ec5d6", "#e8dfd4",
            "#968a7d", "#f7a1a1", "#b2e0a2", "#f7d08a", "#b3cdef", "#e8c0dc", "#b4dce8", "#fffaf3",
        ],
    ),
    built_in(
        "daylight", "Daylight", ThemeName::Light,
        ThemeStyle {
            radius: Some(ThemeRadius::Medium), depth: Some(ThemeDepth::Soft),
            density: Some(ThemeDensity::Comfortable), ..Default::default()
        },
        [
            "#ffffff", "#f9f9f8", "#ffffff", "#efefed",
            "#e7e7e4", "#d4d4d0", "#1f1f1f", "#4a4a4a", "#6b6b6b",
            "#7c5cff", "#5a3de0", "#6a4cf0", "#5a3de0",
            "#ffffff", "#ffffff", "#15803d", "#a16207", "#be123c",
            "#0369a1", "#ececea", "#f2f2f0", "#e8e8e7",
            "#e4e0ef", "#fafafa", "#15803d1a", "#be123c1a",
        ],
        ["#ffffff", "#1f1f1f", "#5a3de0", "#5a3de038"],
        [
            "#1f1f1f", "#be123c", "#15803d", "#a16207", "#1d4ed8", "#5a3de0", "#0369a1", "#d4d4d0",
            "#6b6b6b", "#e11d48", "#16a34a", "#ca8a04", "#2563eb", "#6d4cf0", "#0284c7", "#4a4a4a",
        ],
    ),
    built_in(
        "paper", "Paper", ThemeName::Light,
        ThemeStyle {
            border: Some(ThemeBorder::Thick), depth: Some(ThemeDepth::Flat),
            mono_font: Some(MonoFont::IbmPlexMono), chat_text_size: Some(15), ..Default::default()
        },
        [
            "#fbf8f1", "#f3eee3", "#fffdf8", "#ece5d6",
            "#e6dfcf", "#d3c9b5", "#2b2620", "#574f44", "#7a7063",
            "#2f5fb3", "#244c94", "#2f5fb3", "#244c94",
            "#ffffff", "#fbf8f1", "#2f7d32", "#9a6700", "#b42318",
            "#1f6f8b", "#ece5d6", "#f4efe4", "#e8e1d2",
            "#d9d8d2", "#f5f0e6", "#2f7d321a", "#b423181a",
        ],
        ["#fbf8f1", "#2b2620", "#244c94", "#2f5fb338"],
        [
            "#2b2620", "#b42318", "#2f7d32", "#9a6700", "#2f5fb3", "#8a3fa0", "#1f6f8b", "#d3c9b5",
            "#7a7063", "#d1361f", "#3c9a40", "#b98100", "#4876c9", "#a457bb", "#2a88a8", "#574f44",
        ],
    ),
]);

/// The five built-in themes: wraith, midnight, clay (dark), daylight, paper (light).
pub fn built_in_themes() -> &'static [AppTheme] {
    &BUILT_IN_THEMES
}

/// The theme a mode falls back to when the picked one is gone.
pub fn default_theme_id(mode: ThemeName) -> &'static str {
    match mode {
        ThemeName::Dark => "wraith",
        ThemeName::Light => "daylight",
    }
}

fn default_theme(mode: ThemeName) -> &'static AppTheme {
    let id = default_theme_id(mode);
    built_in_themes()
        .iter()
        .find(|t| t.id == id)
        .unwrap_or(&built_in_themes()[0])
}

/// The picked theme for a mode, or that mode's default when it is missing or the wrong mode.
pub fn pick_theme<'a>(themes: &'a [AppTheme], id: &str, mode: ThemeName) -> &'a AppTheme {
    themes
        .iter()
        .find(|t| t.id == id && t.mode == mode)
        .or_else(|| themes.iter().find(|t| t.id == default_theme_id(mode)))
        .unwrap_or_else(|| default_theme(mode))
}

// ---- Accent override -----------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AccentColors {
    pub accent: &'static str,
    pub accent_text: &'static str,
    pub accent_fill: &'static str,
    pub accent_fill_hover: &'static str,
    pub on_accent: &'static str,
}

const fn ac(
    accent: &'static str,
    accent_text: &'static str,
    accent_fill: &'static str,
    accent_fill_hover: &'static str,
    on_accent: &'static str,
) -> AccentColors {
    AccentColors {
        accent,
        accent_text,
        accent_fill,
        accent_fill_hover,
        on_accent,
    }
}

/// An accent preset's colors for a mode; None for `theme` (the theme keeps its own).
#[rustfmt::skip]
pub fn accent_preset(accent: Accent, mode: ThemeName) -> Option<AccentColors> {
    let dark = mode == ThemeName::Dark;
    Some(match accent {
        Accent::Theme => return None,
        Accent::Violet if dark => ac("#7c5cff", "#a996ff", "#6a4cf0", "#5a3de0", "#ffffff"),
        Accent::Violet => ac("#7c5cff", "#5a3de0", "#6a4cf0", "#5a3de0", "#ffffff"),
        Accent::Blue if dark => ac("#3b82f6", "#93c5fd", "#2563eb", "#1d4ed8", "#ffffff"),
        Accent::Blue => ac("#3b82f6", "#1d4ed8", "#2563eb", "#1d4ed8", "#ffffff"),
        Accent::Teal if dark => ac("#14b8a6", "#5eead4", "#0f766e", "#115e59", "#ffffff"),
        Accent::Teal => ac("#14b8a6", "#0f766e", "#0f766e", "#115e59", "#ffffff"),
        Accent::Amber if dark => ac("#f59e0b", "#fcd34d", "#f59e0b", "#d97706", "#1c1403"),
        Accent::Amber => ac("#f59e0b", "#b45309", "#f59e0b", "#d97706", "#1c1403"),
        Accent::Rose if dark => ac("#f43f5e", "#fda4af", "#e11d48", "#be123c", "#ffffff"),
        Accent::Rose => ac("#f43f5e", "#be123c", "#e11d48", "#be123c", "#ffffff"),
    })
}

/// A theme with an accent laid over it (`theme` keeps its own).
pub fn with_accent(t: &ThemeFile, accent: Accent) -> ThemeFile {
    let Some(a) = accent_preset(accent, t.mode) else {
        return t.clone();
    };
    let mut out = t.clone();
    // The bubble is tinted by the accent: mixed again from the new one.
    out.colors.user_bubble = None;
    out.colors.accent = a.accent.into();
    out.colors.accent_text = Some(a.accent_text.into());
    out.colors.accent_fill = Some(a.accent_fill.into());
    out.colors.accent_fill_hover = Some(a.accent_fill_hover.into());
    out.colors.on_accent = Some(a.on_accent.into());
    // The terminal cursor and selection follow the accent too.
    let mut term = t.terminal.clone().unwrap_or_default();
    term.cursor = Some(a.accent_text.into());
    term.selection = Some(format!("{}{}", a.accent, selection_alpha(t.mode)));
    out.terminal = Some(term);
    out
}

/// The alpha a selection color gets from an accent: stronger on dark.
fn selection_alpha(mode: ThemeName) -> &'static str {
    match mode {
        ThemeName::Dark => "61",
        ThemeName::Light => "38",
    }
}

// ---- Theme → CSS variables -------------------------------------------------------------

/// Status colors a theme file may leave out, taken from the default theme of its mode:
/// ok, warn, err, info.
fn base_status(mode: ThemeName) -> [&'static str; 4] {
    match mode {
        ThemeName::Dark => ["#4ade80", "#fbbf24", "#fb7185", "#7dd3fc"],
        ThemeName::Light => ["#15803d", "#a16207", "#be123c", "#0369a1"],
    }
}

/// CSS variables in the order they are set: `(name, value)`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ThemeVars(pub Vec<(&'static str, String)>);

impl ThemeVars {
    /// A variable's value.
    pub fn get(&self, name: &str) -> Option<&str> {
        self.0
            .iter()
            .find(|(k, _)| *k == name)
            .map(|(_, v)| v.as_str())
    }
    pub fn iter(&self) -> impl Iterator<Item = (&'static str, &str)> {
        self.0.iter().map(|(k, v)| (*k, v.as_str()))
    }
}

fn mix(a: &str, pct: u32, b: &str) -> String {
    format!("color-mix(in srgb, {a} {pct}%, {b})")
}

/// Every CSS variable a theme sets. Colors the file leaves out are mixed from the ones it
/// gives (bg, text, accent), so a three-color theme still looks finished.
pub fn theme_vars(t: &ThemeFile) -> ThemeVars {
    let dark = t.mode == ThemeName::Dark;
    let [ok, warn, err, info] = base_status(t.mode);
    let derived = |c: ThemeColor| -> String {
        match c {
            ThemeColor::Bg => t.colors.bg.clone(),
            ThemeColor::Text => t.colors.text.clone(),
            ThemeColor::Accent => t.colors.accent.clone(),
            ThemeColor::Sidebar => mix(
                "var(--bg)",
                if dark { 88 } else { 97 },
                if dark { "#000" } else { "var(--tx)" },
            ),
            ThemeColor::Panel => mix("var(--bg)", 95, "var(--tx)"),
            ThemeColor::Raised => mix("var(--bg)", 90, "var(--tx)"),
            ThemeColor::Line => mix("var(--bg)", 89, "var(--tx)"),
            ThemeColor::LineStrong => mix("var(--bg)", 80, "var(--tx)"),
            ThemeColor::Muted => mix("var(--tx)", 76, "var(--bg)"),
            ThemeColor::Faint => mix("var(--tx)", 56, "var(--bg)"),
            ThemeColor::AccentText => mix(
                "var(--acc)",
                if dark { 65 } else { 78 },
                if dark { "#fff" } else { "#000" },
            ),
            ThemeColor::AccentFill => "var(--acc)".into(),
            ThemeColor::AccentFillHover => mix("var(--acc)", 85, "#000"),
            ThemeColor::OnAccent => "#ffffff".into(),
            ThemeColor::Terminal => "var(--bg)".into(),
            ThemeColor::Ok => ok.into(),
            ThemeColor::Warn => warn.into(),
            ThemeColor::Err => err.into(),
            ThemeColor::Info => info.into(),
            ThemeColor::Selection => mix("var(--bg)", 90, "var(--tx)"),
            ThemeColor::Hover => mix("var(--bg)", 95, "var(--tx)"),
            ThemeColor::Frame => mix("var(--bg2)", if dark { 78 } else { 93 }, "#000"),
            ThemeColor::UserBubble => mix("var(--acc)", 12, "var(--raise)"),
            ThemeColor::CodeBlock => mix("var(--bg2)", 75, "var(--bg)"),
            ThemeColor::DiffAdd => mix("var(--ok)", 10, "transparent"),
            ThemeColor::DiffDel => mix("var(--err)", 10, "transparent"),
        }
    };
    let mut vars: Vec<(&'static str, String)> = ThemeColor::ALL
        .iter()
        .map(|&c| {
            let v = t.colors.get(c).map(str::to_owned);
            (c.css_var(), v.unwrap_or_else(|| derived(c)))
        })
        .collect();
    // The rest of tokens.css follows from those.
    let scrim = if dark {
        "rgba(0, 0, 0, 0.55)"
    } else {
        "rgba(0, 0, 0, 0.25)"
    };
    vars.extend([
        ("--selr", "var(--line2)".to_owned()),
        ("--hover-line", mix("var(--fa)", 70, "var(--bg)")),
        ("--scrim", scrim.to_owned()),
        ("--banner-bg", mix("var(--warn)", 9, "var(--bg)")),
        ("--banner-line", mix("var(--warn)", 45, "transparent")),
        ("--banner-title", mix("var(--warn)", 65, "var(--tx)")),
        ("--logo-line", mix("var(--fa)", 75, "var(--bg)")),
        ("--empty-line", "var(--line2)".to_owned()),
        ("--mini-hover", mix("var(--acc)", 45, "var(--bg)")),
        ("--solid", "var(--tx)".to_owned()),
        (
            "--solid-h",
            mix("var(--tx)", 80, if dark { "#fff" } else { "#000" }),
        ),
        ("--on-solid", "var(--bg2)".to_owned()),
    ]);
    vars.extend(style_vars(t).0);
    ThemeVars(vars)
}

/// Card and pop-over shadows for a depth and mode.
fn shadows(depth: ThemeDepth, mode: ThemeName) -> [&'static str; 2] {
    match (depth, mode) {
        (ThemeDepth::Flat, _) => ["none", "0 0 0 1px var(--line2)"],
        (ThemeDepth::Soft, ThemeName::Dark) => [
            "0 6px 24px -14px rgba(0, 0, 0, 0.55)",
            "0 16px 40px -12px rgba(0, 0, 0, 0.45), 0 0 0 1px var(--line2)",
        ],
        (ThemeDepth::Soft, ThemeName::Light) => [
            "0 4px 18px -12px rgba(0, 0, 0, 0.25)",
            "0 16px 40px -12px rgba(0, 0, 0, 0.18), 0 0 0 1px var(--line2)",
        ],
        (ThemeDepth::Deep, ThemeName::Dark) => [
            "0 14px 36px -12px rgba(0, 0, 0, 0.75)",
            "0 24px 56px -12px rgba(0, 0, 0, 0.6), 0 0 0 1px var(--line2)",
        ],
        (ThemeDepth::Deep, ThemeName::Light) => [
            "0 10px 30px -12px rgba(0, 0, 0, 0.32)",
            "0 22px 50px -14px rgba(0, 0, 0, 0.28), 0 0 0 1px var(--line2)",
        ],
    }
}

/// A theme's style as CSS variables (motion is the `data-motion` attribute, see App).
pub fn style_vars(t: &ThemeFile) -> ThemeVars {
    let s = t.style.clone().unwrap_or_default();
    let [card, pop] = shadows(s.depth.unwrap_or(ThemeDepth::Soft), t.mode);
    let chat = s.chat_text_size.map_or(14.5, f64::from);
    let frame = if s.frame_gradient == Some(true) {
        let pct = if t.mode == ThemeName::Dark { 10 } else { 7 };
        format!(
            "radial-gradient(120% 80% at 0% 0%, color-mix(in srgb, var(--acc) {pct}%, transparent), transparent 60%)"
        )
    } else {
        "none".into()
    };
    ThemeVars(vec![
        ("--font-ui", s.font.unwrap_or(UiFont::Geist).stack().into()),
        (
            "--font-mono",
            s.mono_font
                .unwrap_or(MonoFont::JetBrainsMono)
                .stack()
                .into(),
        ),
        ("--chat-size", format!("{chat}px")),
        (
            "--rs",
            s.radius.unwrap_or(ThemeRadius::Medium).scale().to_string(),
        ),
        ("--bw", s.border.unwrap_or(ThemeBorder::Thin).css().into()),
        (
            "--dens",
            s.density
                .unwrap_or(ThemeDensity::Comfortable)
                .scale()
                .to_string(),
        ),
        ("--shadow-card", card.into()),
        ("--shadow-pop", pop.into()),
        ("--frame-img", frame),
    ])
}

/// `#rrggbb[aa]` → `rgba(…)`; xterm and color math want one form. `a` overrides the
/// color's own alpha.
pub fn hex_to_rgba(hex: &str, a: Option<f64>) -> String {
    let n = hex
        .get(1..7)
        .and_then(|h| u32::from_str_radix(h, 16).ok())
        .unwrap_or(0);
    let own = match hex.get(7..) {
        Some(aa) if hex.len() == 9 => u32::from_str_radix(aa, 16).map_or(0.0, |v| v as f64 / 255.0),
        _ => 1.0,
    };
    let al = (a.unwrap_or(own) * 1000.0).round() / 1000.0;
    format!(
        "rgba({}, {}, {}, {al})",
        (n >> 16) & 255,
        (n >> 8) & 255,
        n & 255
    )
}

/// Every terminal color a theme draws with (the TS `Required<ThemeTerminalColors>`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FullTerminalColors {
    pub background: String,
    pub foreground: String,
    pub cursor: String,
    pub selection: String,
    /// black … white, then the bright eight.
    pub ansi: [String; 16],
}

impl FullTerminalColors {
    pub fn get(&self, c: TerminalColor) -> &str {
        match c {
            TerminalColor::Background => &self.background,
            TerminalColor::Foreground => &self.foreground,
            TerminalColor::Cursor => &self.cursor,
            TerminalColor::Selection => &self.selection,
            ansi => {
                let i = TerminalColor::ANSI.iter().position(|x| *x == ansi);
                &self.ansi[i.unwrap_or(0)]
            }
        }
    }

    fn set(&mut self, c: TerminalColor, v: String) {
        match c {
            TerminalColor::Background => self.background = v,
            TerminalColor::Foreground => self.foreground = v,
            TerminalColor::Cursor => self.cursor = v,
            TerminalColor::Selection => self.selection = v,
            ansi => {
                if let Some(i) = TerminalColor::ANSI.iter().position(|x| *x == ansi) {
                    self.ansi[i] = v;
                }
            }
        }
    }
}

/// The terminal colors for a theme: its own, then ones made from its app colors.
pub fn theme_terminal(t: &ThemeFile) -> FullTerminalColors {
    let base = default_theme(t.mode).terminal.clone().unwrap_or_default();
    let c = &t.colors;
    let [ok, warn, err, info] = base_status(t.mode);
    let accent_text = c.accent_text.as_deref().unwrap_or(&c.accent);
    let mut out = FullTerminalColors {
        background: String::new(),
        foreground: String::new(),
        cursor: String::new(),
        selection: String::new(),
        ansi: Default::default(),
    };
    for &k in TerminalColor::ALL {
        out.set(k, base.get(k).unwrap_or_default().to_owned());
    }
    // A theme that names its status colors gets them in the terminal too.
    out.set(TerminalColor::Red, c.err.as_deref().unwrap_or(err).into());
    out.set(TerminalColor::Green, c.ok.as_deref().unwrap_or(ok).into());
    out.set(
        TerminalColor::Yellow,
        c.warn.as_deref().unwrap_or(warn).into(),
    );
    out.set(
        TerminalColor::Cyan,
        c.info.as_deref().unwrap_or(info).into(),
    );
    out.set(TerminalColor::Magenta, accent_text.into());
    out.background = t
        .terminal
        .as_ref()
        .and_then(|x| x.background.clone())
        .or_else(|| c.terminal.clone())
        .unwrap_or_else(|| c.bg.clone());
    out.foreground = c.text.clone();
    out.cursor = accent_text.into();
    let acc7: String = c.accent.chars().take(7).collect();
    out.selection = format!("{acc7}{}", selection_alpha(t.mode));
    if let Some(own) = &t.terminal {
        for &k in TerminalColor::ALL {
            if let Some(v) = own.get(k) {
                out.set(k, v.into());
            }
        }
    }
    out
}
