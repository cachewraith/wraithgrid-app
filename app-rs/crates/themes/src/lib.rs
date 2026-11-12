//! Wraithgrid's themes for the GPUI app (#166, ADR 0005), ported from the Electron app:
//! the built-in themes and the theme file format (src/shared/themes.ts), the user's theme
//! files in ~/.wraithgrid/themes (src/main/themes.ts), community themes from GitHub
//! (src/main/community-themes.ts), the theme editor's contrast checks (src/shared/contrast.ts)
//! and the terminal palettes (src/renderer/lib/term-theme.ts).
//!
//! Theme files are read and written in the same format as the Electron app, so both read
//! each other's files. [`resolve_active_theme`] turns the settings into one flat set of
//! colors the GPUI app draws with.

/// A string-valued enum: its JSON names, every value in order, and serde both ways.
macro_rules! str_enum {
    (
        $(#[$meta:meta])*
        pub enum $name:ident { $( $(#[$vmeta:meta])* $variant:ident = $key:literal ),* $(,)? }
    ) => {
        $(#[$meta])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub enum $name { $( $(#[$vmeta])* $variant ),* }

        impl $name {
            /// Every value, in the order the TS declares them.
            pub const ALL: &'static [$name] = &[$($name::$variant),*];
            /// The JSON / settings name.
            pub fn as_str(self) -> &'static str {
                match self { $($name::$variant => $key),* }
            }
            /// From its JSON / settings name.
            pub fn parse(s: &str) -> Option<Self> {
                match s { $($key => Some($name::$variant),)* _ => None }
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(self.as_str())
            }
        }

        impl serde::Serialize for $name {
            fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                s.serialize_str(self.as_str())
            }
        }

        impl<'de> serde::Deserialize<'de> for $name {
            fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                let s = String::deserialize(d)?;
                Self::parse(&s)
                    .ok_or_else(|| serde::de::Error::unknown_variant(&s, &[$($key),*]))
            }
        }
    };
}

mod community;
mod contrast;
mod prefs;
mod resolve;
mod store;
mod term;
mod theme;
mod validate;

pub use community::{
    newer_version, repo_from_url, ureq_fetch, CommunityTheme, CommunityThemeList, CommunityThemes,
    CommunityThemesDeps, FetchFn, FetchRequest, FetchResponse, ThemeManifest, COMMUNITY_THEMES_URL,
};
pub use contrast::{contrast_issues, contrast_ratio, luminance, passing_color, ContrastIssue};
pub use prefs::{
    resolve_theme, Accent, TerminalPalette, ThemeName, ThemePreference, ACCENTS, TERMINAL_PALETTES,
};
pub use resolve::{
    all_themes, drawn_theme, eval_css_color, resolve_active_theme, resolve_draft, resolve_ui_theme,
    ResolvedStyle, ResolvedTerminal, ResolvedTheme, Rgba, ThemeSettings, UiColors,
};
pub use store::{
    delete_theme, import_theme, list_themes, save_theme, theme_file_text, themes_root,
    write_theme_as, ThemeImport, ThemeList, ThemeProblem,
};
pub use term::{palette_label, terminal_font_stack, terminal_theme, TerminalTheme};
pub use theme::{
    accent_preset, built_in_themes, default_theme_id, hex_to_rgba, pick_theme, style_vars,
    theme_slug, theme_terminal, theme_vars, with_accent, AccentColors, AppTheme,
    FullTerminalColors, MonoFont, TerminalColor, ThemeBorder, ThemeColor, ThemeColors,
    ThemeDensity, ThemeDepth, ThemeFile, ThemeMotion, ThemeRadius, ThemeStyle, ThemeTerminalColors,
    ThemeVars, UiFont, ANSI_NAMES, CHAT_TEXT_MAX, CHAT_TEXT_MIN, THEMES_FILE_MAX, THEMES_MAX,
};
pub use validate::parse_theme_file;
